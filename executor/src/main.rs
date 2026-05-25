mod agent;
mod broker;
mod data;
mod execution;
mod ipc;
mod orders;

use agent::StrategyAgent;
use anyhow::Result;
use broker::{Broker, BrokerRouter};
use orders::{persistence::{ev_start, ev_submitted, EventLog}, OrderManagementSystem};
use std::sync::Arc;
use tracing::info;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    info!("Zeta Field Executor starting");

    let config = Config::from_env()?;

    // ── Persistence ───────────────────────────────────────────────────────────
    let log = Arc::new(EventLog::new(&config.oms_log_path));
    log.append(&ev_start()).await?;

    let oms = Arc::new(OrderManagementSystem::new());

    // Reconstruct OMS state from event log on startup
    let prior = log.reconstruct().await?;
    if !prior.open_orders.is_empty() {
        info!(open = prior.open_orders.len(), "Restoring open orders from event log");
        for order in prior.open_orders {
            oms.submit(order);
        }
    }

    // ── Brokers ───────────────────────────────────────────────────────────────
    let tradier = Arc::new(broker::tradier::TradierBroker::new(
        config.tradier_token.clone(),
        config.tradier_account_id.clone(),
        config.tradier_sandbox,
    ));

    let tradovate = Arc::new(
        broker::tradovate::TradovateBroker::connect(
            config.tradovate_username.clone(),
            config.tradovate_password.clone(),
            config.tradovate_cid,
            config.tradovate_secret.clone(),
            config.tradovate_device_id.clone(),
            config.tradovate_demo,
        ).await?,
    );
    tradovate.spawn_refresh_task();  // renew token every 23h in background

    let broker: Arc<dyn Broker> = Arc::new(BrokerRouter::new(tradier, tradovate));

    // ── LLM agent ─────────────────────────────────────────────────────────────
    let agent = Arc::new(StrategyAgent::new(config.anthropic_key.clone()));

    // ── Channels ──────────────────────────────────────────────────────────────
    let (td_tx, mut td_rx) = tokio::sync::mpsc::channel(4096);
    let (db_tx, mut db_rx) = tokio::sync::mpsc::channel(4096);
    let (zeta_tx, mut zeta_rx) = tokio::sync::mpsc::channel::<ipc::ZetaSignal>(64);

    // ── ZMQ receiver ──────────────────────────────────────────────────────────
    let zmq_endpoint = std::env::var("ZMQ_ENDPOINT")
        .unwrap_or_else(|_| "ipc:///tmp/zeta.sock".to_string());
    let receiver = ipc::ZetaReceiver::new(zmq_endpoint);
    let zeta_tx_zmq = zeta_tx.clone();
    tokio::spawn(async move {
        receiver.run(zeta_tx_zmq).await
            .expect("ZMQ receiver failed");
    });

    // ── Market data feeds ─────────────────────────────────────────────────────
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

    info!("Zeta Field Executor ready");

    // ── Main event loop ───────────────────────────────────────────────────────
    loop {
        tokio::select! {
            Some(event) = td_rx.recv() => {
                oms.on_equity_event(event).await?;
            }

            Some(event) = db_rx.recv() => {
                oms.on_futures_event(event).await?;
            }

            Some(signal) = zeta_rx.recv() => {
                let oms_ref    = oms.clone();
                let agent_ref  = agent.clone();
                let broker_ref = broker.clone();
                let log_ref    = log.clone();

                tokio::spawn(async move {
                    if let Err(e) = handle_zeta_signal(
                        signal, oms_ref, agent_ref, broker_ref, log_ref
                    ).await {
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
    broker: Arc<dyn Broker>,
    log:    Arc<EventLog>,
) -> Result<()> {
    // ── Decision ──────────────────────────────────────────────────────────────
    let decision = if signal.needs_llm {
        agent.consult(
            &signal.zeta_context,
            &signal.llm_questions,
            &signal.proposal.clone().into(),
        ).await?
    } else {
        agent::decision::StrategyDecision {
            approved:            true,
            strategy_type:       signal.proposal.strategy_type.clone(),
            contracts:           signal.proposal.contracts,
            target_delta:        signal.proposal.est_delta,
            target_vega:         signal.proposal.est_vega,
            target_dte:          signal.proposal.target_dte,
            entry_urgency:       signal.proposal.entry_urgency.clone(),
            reasoning:           "Rule engine clear signal — no LLM review needed.".to_string(),
            confidence:          0.85,
            macro_concerns:      None,
            sizing_adjustment:   None,
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

    let final_contracts = decision.sizing_adjustment
        .map(|adj| (decision.contracts as f64 * adj).round() as u32)
        .unwrap_or(decision.contracts);

    info!(
        strategy   = %decision.strategy_type,
        contracts  = final_contracts,
        urgency    = %decision.entry_urgency,
        confidence = decision.confidence,
        "Executing approved trade"
    );

    // ── Order construction ────────────────────────────────────────────────────
    let greeks = (
        signal.proposal.est_delta,
        0.0,
        signal.proposal.est_vega,
        signal.proposal.est_theta_day,
    );
    let strategy_id = uuid::Uuid::new_v4().to_string();

    let mut order = match execution::build_order(
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
                strategy   = %decision.strategy_type,
                candidates = signal.chain_candidates.len(),
                error      = %e,
                "Order construction failed — no trade submitted"
            );
            return Ok(());
        }
    };

    let leg_summary: Vec<_> = order.legs.iter()
        .map(|l| format!("{:?} {} x{}", l.side, l.symbol, l.quantity))
        .collect();
    info!(legs = ?leg_summary, "Submitting to broker");

    // ── Broker submission ─────────────────────────────────────────────────────
    let broker_id = match broker.submit_order(&order).await {
        Ok(id) => id,
        Err(e) => {
            tracing::error!(
                strategy = %decision.strategy_type,
                error    = %e,
                "Broker submission failed"
            );
            return Ok(());
        }
    };

    info!(%broker_id, "Broker accepted order");

    // Update order with broker confirmation
    order.broker_id = Some(broker_id.clone());
    order.status    = orders::OrderStatus::Submitted;

    // ── Stage in OMS + persist ────────────────────────────────────────────────
    let order_id = oms.submit(order.clone());
    log.append(&ev_submitted(order)).await?;

    info!(%order_id, %broker_id, "Order staged in OMS and persisted");

    Ok(())
}

// ── Config ────────────────────────────────────────────────────────────────────

#[derive(Debug)]
struct Config {
    // Data feeds
    thetadata_key:      String,
    databento_key:      String,
    equity_roots:       Vec<String>,
    cme_symbols:        Vec<String>,
    // LLM
    anthropic_key:      String,
    // Tradier — options and stocks
    tradier_token:      String,
    tradier_account_id: String,
    tradier_sandbox:    bool,
    // Tradovate — futures
    tradovate_username: String,
    tradovate_password: String,
    tradovate_cid:      i64,
    tradovate_secret:   String,
    tradovate_device_id: String,
    tradovate_demo:     bool,
    // OMS
    oms_log_path:       String,
}

impl Config {
    fn from_env() -> Result<Self> {
        Ok(Config {
            thetadata_key:      std::env::var("THETADATA_API_KEY")?,
            databento_key:      std::env::var("DATABENTO_API_KEY")?,
            equity_roots:       std::env::var("EQUITY_ROOTS")
                                    .unwrap_or_default()
                                    .split(',').map(String::from).collect(),
            cme_symbols:        std::env::var("CME_SYMBOLS")
                                    .unwrap_or_default()
                                    .split(',').map(String::from).collect(),
            anthropic_key:      std::env::var("ANTHROPIC_API_KEY")?,
            tradier_token:      std::env::var("TRADIER_TOKEN")?,
            tradier_account_id: std::env::var("TRADIER_ACCOUNT_ID")?,
            tradier_sandbox:    std::env::var("TRADIER_SANDBOX")
                                    .map(|v| v == "true" || v == "1")
                                    .unwrap_or(true),    // default sandbox
            tradovate_username:  std::env::var("TRADOVATE_USERNAME")?,
            tradovate_password:  std::env::var("TRADOVATE_PASSWORD")?,
            tradovate_cid:       std::env::var("TRADOVATE_CID")
                                     .unwrap_or_default()
                                     .parse()
                                     .unwrap_or(0),
            tradovate_secret:    std::env::var("TRADOVATE_SECRET")?,
            tradovate_device_id: std::env::var("TRADOVATE_DEVICE_ID")
                                     .unwrap_or_else(|_| uuid::Uuid::new_v4().to_string()),
            tradovate_demo:      std::env::var("TRADOVATE_DEMO")
                                     .map(|v| v == "true" || v == "1")
                                     .unwrap_or(true),   // default demo
            oms_log_path:        std::env::var("OMS_LOG_PATH")
                                     .unwrap_or_else(|_| "/var/log/trading/oms.jsonl".to_string()),
        })
    }
}
