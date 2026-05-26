use super::{Order, OrderStatus, Position};
use anyhow::Result;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tokio::fs::OpenOptions;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tracing::{info, warn};

// ── Append-only event log ─────────────────────────────────────────────────────
// Every state change is written as a JSON line.
// On startup, replay the log to reconstruct OMS state.
// Then reconcile against the broker API to catch divergences.

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event_type")]
pub enum OmsEvent {
    OrderSubmitted {
        ts:    String,
        order: Order,
    },
    OrderFilled {
        ts:         String,
        order_id:   String,
        broker_id:  String,
        fill_qty:   u64,
        fill_price: f64,
    },
    OrderCancelled {
        ts:       String,
        order_id: String,
        reason:   String,
    },
    OrderRejected {
        ts:       String,
        order_id: String,
        reason:   String,
    },
    PositionUpdated {
        ts:       String,
        position: Position,
    },
    GreeksUpdated {
        ts:           String,
        symbol:       String,
        delta:        f64,
        gamma:        f64,
        theta:        f64,
        vega:         f64,
    },
    SystemStart {
        ts:      String,
        version: String,
    },
    SystemStop {
        ts:     String,
        reason: String,
    },
    // Signal pipeline audit trail — links signal → decision → action together via signal_id
    SignalReceived {
        ts:                   String,
        signal_id:            String,
        symbol:               String,
        strategy_type:        String,
        needs_llm:            bool,
        zeta_context_preview: String,   // first 200 chars — enough for audit, not bloat
    },
    DecisionMade {
        ts:                String,
        signal_id:         String,
        source:            String,      // "llm" | "rule_engine"
        request_hash:      String,      // UUID v5 of inputs — enables LLM response caching
        approved:          bool,
        reasoning:         String,
        confidence:        f64,
        sizing_adjustment: Option<f64>,
    },
    ActionPlanned {
        ts:        String,
        signal_id: String,
        action:    String,   // "Submit" | "Skip"
        detail:    String,   // order UUID if Submit, skip reason if Skip
    },
}

pub struct EventLog {
    path: PathBuf,
}

impl EventLog {
    pub fn new(path: impl AsRef<Path>) -> Self {
        EventLog { path: path.as_ref().to_path_buf() }
    }

    /// Append a single event — O(1), never blocks for long
    pub async fn append(&self, event: &OmsEvent) -> Result<()> {
        let line = serde_json::to_string(event)? + "\n";
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .await?;
        file.write_all(line.as_bytes()).await?;
        file.flush().await?;
        Ok(())
    }

    /// Replay all events from the log — called on startup
    pub async fn replay(&self) -> Result<Vec<OmsEvent>> {
        if !self.path.exists() {
            info!("No existing event log at {:?} — starting fresh", self.path);
            return Ok(vec![]);
        }

        let file = tokio::fs::File::open(&self.path).await?;
        let reader = BufReader::new(file);
        let mut lines = reader.lines();
        let mut events = Vec::new();
        let mut line_num = 0u64;

        while let Some(line) = lines.next_line().await? {
            line_num += 1;
            let line = line.trim();
            if line.is_empty() { continue; }
            match serde_json::from_str::<OmsEvent>(line) {
                Ok(ev) => events.push(ev),
                Err(e) => warn!("Skipping corrupt log line {}: {}", line_num, e),
            }
        }

        info!("Replayed {} events from {:?}", events.len(), self.path);
        Ok(events)
    }

    /// Reconstruct OMS state from event log
    pub async fn reconstruct(&self) -> Result<ReconstructedState> {
        let events = self.replay().await?;
        let mut state = ReconstructedState::default();

        for event in &events {
            match event {
                OmsEvent::OrderSubmitted { order, .. } => {
                    state.orders.push(order.clone());
                }
                OmsEvent::OrderFilled { order_id, fill_qty, fill_price, broker_id, .. } => {
                    if let Some(o) = state.orders.iter_mut().find(|o| o.id.to_string() == *order_id) {
                        o.status      = OrderStatus::Filled;
                        o.filled_qty  = *fill_qty;
                        o.avg_fill_px = *fill_price;
                        o.broker_id   = Some(broker_id.clone());
                    }
                }
                OmsEvent::OrderCancelled { order_id, .. } => {
                    if let Some(o) = state.orders.iter_mut().find(|o| o.id.to_string() == *order_id) {
                        o.status = OrderStatus::Cancelled;
                    }
                }
                OmsEvent::OrderRejected { order_id, .. } => {
                    if let Some(o) = state.orders.iter_mut().find(|o| o.id.to_string() == *order_id) {
                        o.status = OrderStatus::Rejected;
                    }
                }
                OmsEvent::PositionUpdated { position, .. } => {
                    state.positions.retain(|p: &Position| p.symbol != position.symbol);
                    state.positions.push(position.clone());
                }
                OmsEvent::SystemStart { ts, version } => {
                    state.last_start = Some(ts.clone());
                    state.version    = version.clone();
                }
                OmsEvent::SystemStop { .. } => {}
                OmsEvent::GreeksUpdated { .. } => {}  // live data, not reconstructed
                OmsEvent::SignalReceived { .. } => {}  // audit only
                OmsEvent::DecisionMade { .. }   => {}  // audit only
                OmsEvent::ActionPlanned { .. }  => {}  // audit only
            }
        }

        // Filter out terminal orders — only keep open ones
        state.open_orders = state.orders.iter()
            .filter(|o| matches!(o.status,
                OrderStatus::Pending | OrderStatus::Submitted | OrderStatus::PartiallyFilled))
            .cloned()
            .collect();

        info!(
            total_orders = state.orders.len(),
            open_orders  = state.open_orders.len(),
            positions    = state.positions.len(),
            "OMS state reconstructed from log"
        );

        Ok(state)
    }

    pub fn path(&self) -> &Path { &self.path }
}

#[derive(Debug, Default)]
pub struct ReconstructedState {
    pub orders:      Vec<Order>,
    pub open_orders: Vec<Order>,
    pub positions:   Vec<Position>,
    pub last_start:  Option<String>,
    pub version:     String,
}

// ── Convenience constructors for events ──────────────────────────────────────

pub fn ev_start() -> OmsEvent {
    OmsEvent::SystemStart {
        ts:      Utc::now().to_rfc3339(),
        version: env!("CARGO_PKG_VERSION").to_string(),
    }
}

pub fn ev_stop(reason: impl Into<String>) -> OmsEvent {
    OmsEvent::SystemStop {
        ts:     Utc::now().to_rfc3339(),
        reason: reason.into(),
    }
}

pub fn ev_submitted(order: Order) -> OmsEvent {
    OmsEvent::OrderSubmitted { ts: Utc::now().to_rfc3339(), order }
}

pub fn ev_filled(order_id: &str, broker_id: &str, qty: u64, price: f64) -> OmsEvent {
    OmsEvent::OrderFilled {
        ts:         Utc::now().to_rfc3339(),
        order_id:   order_id.to_string(),
        broker_id:  broker_id.to_string(),
        fill_qty:   qty,
        fill_price: price,
    }
}

pub fn ev_cancelled(order_id: &str, reason: impl Into<String>) -> OmsEvent {
    OmsEvent::OrderCancelled {
        ts:       Utc::now().to_rfc3339(),
        order_id: order_id.to_string(),
        reason:   reason.into(),
    }
}

pub fn ev_signal_received(
    signal_id:     &str,
    symbol:        &str,
    strategy_type: &str,
    needs_llm:     bool,
    zeta_context:  &str,
) -> OmsEvent {
    OmsEvent::SignalReceived {
        ts:                   Utc::now().to_rfc3339(),
        signal_id:            signal_id.to_string(),
        symbol:               symbol.to_string(),
        strategy_type:        strategy_type.to_string(),
        needs_llm,
        zeta_context_preview: zeta_context.chars().take(200).collect(),
    }
}

pub fn ev_decision_made(
    signal_id:         &str,
    source:            &str,
    request_hash:      &str,
    approved:          bool,
    reasoning:         &str,
    confidence:        f64,
    sizing_adjustment: Option<f64>,
) -> OmsEvent {
    OmsEvent::DecisionMade {
        ts:                Utc::now().to_rfc3339(),
        signal_id:         signal_id.to_string(),
        source:            source.to_string(),
        request_hash:      request_hash.to_string(),
        approved,
        reasoning:         reasoning.to_string(),
        confidence,
        sizing_adjustment,
    }
}

pub fn ev_action_planned(signal_id: &str, action: &str, detail: &str) -> OmsEvent {
    OmsEvent::ActionPlanned {
        ts:        Utc::now().to_rfc3339(),
        signal_id: signal_id.to_string(),
        action:    action.to_string(),
        detail:    detail.to_string(),
    }
}
