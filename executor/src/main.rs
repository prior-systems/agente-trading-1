mod agent;
mod broker;
mod data;
mod execution;
mod ipc;
mod orders;

use agent::StrategyAgent;
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
    let (zeta_tx, mut zeta_rx) = tokio::sync::mpsc::channel::<ipc::ZetaSignal>(64);

    // Data feeds
    // ZMQ receiver — Rust binds, Julia connects
    let zmq_endpoint = std::env::var("ZMQ_ENDPOINT")
        .unwrap_or_else(|_| "ipc:///tmp/zeta.sock".to_string());
    let receiver = ipc::ZetaReceiver::new(zmq_endpoint);
    let zeta_tx_zmq = zeta_tx.clone();
    tokio::spawn(async move {
        receiver.run(zeta_tx_zmq).await
            .expect("ZMQ receiver failed");
    });

    let td_feed = data::thetadata::ThetaDataFeed::new(config.thetadata_key.clone(), td_tx);
    let db_feed = data::databento::DatabentoFeed::new(config.databento_key.clone(), db_tx);
    let equity_roots = config.equity_roots.clone();
    let cme_symbols  = config.cme_symbols.clone();

    tokio::spawn(async move {
        td_feed.stream_options(&equity_roots).await
            .expect("ThetaData feed failed");
    });

    tokio::spawn(async move {
        db_feed.stream_futures(&cme_symbols).await
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

async fn handle_zeta_signal(
    signal: ipc::ZetaSignal,
    oms:    Arc<OrderManagementSystem>,
    agent:  Arc<StrategyAgent>,
) -> Result<()> {
    let decision = if signal.needs_llm {
        // Ambiguous case → consult LLM
        agent.consult(
            &signal.zeta_context,
            &signal.llm_questions,
            &signal.proposal.clone().into(),
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

    // Translate decision → concrete order legs using live chain candidates
    let greeks = (
        signal.proposal.est_delta,
        0.0,                          // gamma not in proposal — estimated per-leg
        signal.proposal.est_vega,
        signal.proposal.est_theta_day,
    );
    let strategy_id = uuid::Uuid::new_v4().to_string();

    let order = match execution::build_order(
        &decision.strategy_type,
        &signal.chain_candidates,
        final_contracts,
        decision.target_dte,
        &strategy_id,
        greeks,
    ) {
        Ok(o) => o,
        Err(e) => {
            tracing::warn!(
                strategy = %decision.strategy_type,
                error    = %e,
                candidates = signal.chain_candidates.len(),
                "Order construction failed — no trade submitted"
            );
            return Ok(());
        }
    };

    let leg_summary: Vec<_> = order.legs.iter()
        .map(|l| format!("{:?} {} x{}", l.side, l.symbol, l.quantity))
        .collect();
    info!(legs = ?leg_summary, "Order built — submitting to OMS");

    // Stage in OMS (DashMap); broker submission is the next integration step
    let order_id = oms.submit(order);
    info!(%order_id, "Order staged in OMS");

    // TODO: route order_id to AlpacaBroker::submit_order() and update OMS with broker_id

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
