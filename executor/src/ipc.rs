use crate::agent::RuleProposal;
use anyhow::Result;
use serde::Deserialize;
use tokio::sync::mpsc::Sender;
use tracing::{debug, info, warn};
use zeromq::{PullSocket, Socket, SocketRecv};

// ── ZetaSignal received from Julia ────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
pub struct ZetaSignal {
    pub timestamp:        String,
    pub symbol:           String,
    pub zeta_context:     String,
    pub needs_llm:        bool,
    pub llm_questions:    Vec<String>,
    pub proposal:         RuleProposalMsg,
    pub chain_candidates: Vec<crate::execution::StrikeCandidate>,
}

// JSON representation of Julia's StrategyProposal
#[derive(Debug, Clone, Deserialize)]
pub struct RuleProposalMsg {
    pub strategy_type:    String,
    pub contracts:        u32,
    pub max_risk_dollars: f64,
    pub est_delta:        f64,
    pub est_vega:         f64,
    pub est_theta_day:    f64,
    pub target_dte:       u32,
    pub entry_urgency:    String,
    pub rationale:        String,
    pub passes_limits:    bool,
    pub limit_violations: Vec<String>,
}

impl From<RuleProposalMsg> for RuleProposal {
    fn from(m: RuleProposalMsg) -> Self {
        RuleProposal {
            strategy_type:    m.strategy_type,
            contracts:        m.contracts,
            max_risk_dollars: m.max_risk_dollars,
            est_delta:        m.est_delta,
            est_vega:         m.est_vega,
            est_theta_day:    m.est_theta_day,
            target_dte:       m.target_dte,
            entry_urgency:    m.entry_urgency,
            rationale:        m.rationale,
        }
    }
}

// ── Heartbeat ─────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct Heartbeat {
    #[serde(rename = "type")]
    msg_type: String,
    ts: String,
}

// ── ZMQ PULL receiver ─────────────────────────────────────────────────────────

pub struct ZetaReceiver {
    endpoint: String,
}

impl ZetaReceiver {
    pub fn new(endpoint: impl Into<String>) -> Self {
        ZetaReceiver { endpoint: endpoint.into() }
    }

    /// Bind the PULL socket and forward signals to the channel.
    /// Rust binds (stable endpoint), Julia connects (transient sender).
    pub async fn run(self, tx: Sender<ZetaSignal>) -> Result<()> {
        let mut socket = PullSocket::new();
        socket.bind(&self.endpoint).await?;
        info!("ZMQ PULL bound to {}", self.endpoint);

        loop {
            match socket.recv().await {
                Ok(msg) => {
                    // ZmqMessage is a Vec<Bytes>; flatten all frames into one buffer
                    let raw: Vec<u8> = msg.into_vec().into_iter()
                        .flat_map(|frame| frame.to_vec())
                        .collect();
                    let text = match std::str::from_utf8(&raw) {
                        Ok(s) => s,
                        Err(e) => {
                            warn!("ZMQ message not valid UTF-8: {}", e);
                            continue;
                        }
                    };

                    // Heartbeat — log and skip
                    if text.contains("\"heartbeat\"") {
                        if let Ok(hb) = serde_json::from_str::<Heartbeat>(text) {
                            debug!("Julia heartbeat @ {}", hb.ts);
                        }
                        continue;
                    }

                    match serde_json::from_str::<ZetaSignal>(text) {
                        Ok(signal) => {
                            debug!(
                                symbol   = %signal.symbol,
                                strategy = %signal.proposal.strategy_type,
                                needs_llm = signal.needs_llm,
                                "ZetaSignal received"
                            );

                            // Drop if channel full — don't block the ZMQ receive loop
                            if tx.try_send(signal).is_err() {
                                warn!("ZetaSignal channel full — signal dropped (executor busy)");
                            }
                        }
                        Err(e) => {
                            warn!("Failed to parse ZetaSignal: {} | preview: {}", e,
                                  &text[..text.len().min(120)]);
                        }
                    }
                }
                Err(e) => {
                    warn!("ZMQ recv error: {}", e);
                    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
                }
            }
        }
    }
}
