mod agent;
mod broker;
mod data;
mod orders;

use agent::{RuleProposal, StrategyAgent};
use anyhow::Result;
use orders::OrderManagementSystem;
use std::sync::Arc;
use tracing::info;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    info!("Zeta Field Executor starting");

    let config = Config::from_env()?;
    let oms    = Arc::new(OrderManagementSystem::new());
    let agent  = Arc::new(StrategyAgent::new(config.anthropic_key.clone()));

    // Data feed channels
    let (td_tx, mut td_rx) = tokio::sync::mpsc::channel(4096);
    let (db_tx, mut db_rx) = tokio::sync::mpsc::channel(4096);
    // Julia → Rust channel: ZetaState context + rule proposal
    let (zeta_tx, mut zeta_rx) = tokio::sync::mpsc::channel::<ZetaSignal>(64);

    // Data feeds
    let td_feed = data::thetadata::ThetaDataFeed::new(config.thetadata_key.clone(), td_tx);
    let db_feed = data::databento::DatabentoFeed::new(config.databento_key.clone(), db_tx);

    let oms_td = oms.clone();
    tokio::spawn(async move {
        td_feed.stream_options(&config.equity_roots).await
            .expect("ThetaData feed failed");
    });

    let oms_db = oms.clone();
    tokio::spawn(async move {
        db_feed.stream_futures(&config.cme_symbols).await
            .expect("Databento feed failed");
    });

    // Main event loop
    loop {
        tokio::select! {
            Some(event) = td_rx.recv() => {
                oms.on_equity_event(event).await?;
            }

            Some(event) = db_rx.recv() => {
                oms.on_futures_event(event).await?;
            }

            // ZetaState signal from Julia (via HTTP or stdin IPC)
            Some(signal) = zeta_rx.recv() => {
                let oms_ref   = oms.clone();
                let agent_ref = agent.clone();

                tokio::spawn(async move {
                    if let Err(e) = handle_zeta_signal(signal, oms_ref, agent_ref).await {
                        tracing::error!("ZetaSignal handling error: {}", e);
                    }
                });
            }
        }
    }
}

// ── ZetaSignal: message from Julia rule engine ────────────────────────────────

#[derive(Debug, Clone, serde::Deserialize)]
pub struct ZetaSignal {
    pub zeta_context: String,       // output of zeta_context_string()
    pub needs_llm:    bool,
    pub llm_questions: Vec<String>,
    pub proposal:     RuleProposal,
}

async fn handle_zeta_signal(
    signal: ZetaSignal,
    oms:    Arc<OrderManagementSystem>,
    agent:  Arc<StrategyAgent>,
) -> Result<()> {
    let decision = if signal.needs_llm {
        // Ambiguous case → consult LLM
        agent.consult(
            &signal.zeta_context,
            &signal.llm_questions,
            &signal.proposal,
        ).await?
    } else {
        // Clear case → echo rule engine proposal as decision (no LLM cost)
        agent::decision::StrategyDecision {
            approved:          true,
            strategy_type:     signal.proposal.strategy_type.clone(),
            contracts:         signal.proposal.contracts,
            target_delta:      signal.proposal.est_delta,
            target_vega:       signal.proposal.est_vega,
            target_dte:        signal.proposal.target_dte,
            entry_urgency:     signal.proposal.entry_urgency.clone(),
            reasoning:         "Rule engine clear signal — no LLM review needed.".to_string(),
            confidence:        0.85,
            macro_concerns:    None,
            sizing_adjustment: None,
            conditional_trigger: None,
        }
    };

    if !decision.approved || decision.contracts == 0 {
        info!(
            strategy  = %decision.strategy_type,
            reasoning = %decision.reasoning,
            "Trade blocked by decision layer"
        );
        return Ok(());
    }

    // Apply sizing adjustment if LLM modified it
    let final_contracts = decision.sizing_adjustment
        .map(|adj| (decision.contracts as f64 * adj).round() as u32)
        .unwrap_or(decision.contracts);

    info!(
        strategy  = %decision.strategy_type,
        contracts = final_contracts,
        urgency   = %decision.entry_urgency,
        confidence = decision.confidence,
        reasoning  = %decision.reasoning,
        "Executing approved trade"
    );

    // TODO: translate decision → Order legs → oms.submit()
    // This requires the option chain data (strikes, expirations) which
    // comes from ThetaData snapshot — next module.

    Ok(())
}

// ── Config ────────────────────────────────────────────────────────────────────

#[derive(Debug)]
struct Config {
    thetadata_key:  String,
    databento_key:  String,
    anthropic_key:  String,
    equity_roots:   Vec<String>,
    cme_symbols:    Vec<String>,
}

impl Config {
    fn from_env() -> Result<Self> {
        Ok(Config {
            thetadata_key: std::env::var("THETADATA_API_KEY")?,
            databento_key: std::env::var("DATABENTO_API_KEY")?,
            anthropic_key: std::env::var("ANTHROPIC_API_KEY")?,
            equity_roots:  std::env::var("EQUITY_ROOTS")
                               .unwrap_or_default()
                               .split(',').map(String::from).collect(),
            cme_symbols:   std::env::var("CME_SYMBOLS")
                               .unwrap_or_default()
                               .split(',').map(String::from).collect(),
        })
    }
}
