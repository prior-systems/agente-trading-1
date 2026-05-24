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
