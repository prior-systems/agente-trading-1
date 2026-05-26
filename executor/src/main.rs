mod agent;
mod broker;
mod data;
mod execution;
mod ipc;
mod orders;

use agent::StrategyAgent;
use anyhow::Result;
use broker::{Broker, BrokerRouter};
use orders::{persistence::{
    ev_start, ev_submitted, ev_signal_received, ev_decision_made, ev_action_planned, EventLog,
}, OrderManagementSystem};
use std::sync::Arc;
use tracing::info;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let dry_run = std::env::var("DRY_RUN")
        .map(|v| v == "true" || v == "1")
        .unwrap_or(false);

    if dry_run {
        info!("★ DRY-RUN mode — mock broker, no live feeds, no real orders");
    }

    info!("Zeta Field Executor starting");

    let config = if dry_run { Config::dry_run_defaults() } else { Config::from_env()? };

    // ── Persistence ───────────────────────────────────────────────────────────
    let log = Arc::new(EventLog::new(&config.oms_log_path));
    log.append(&ev_start()).await?;

    let oms = Arc::new(OrderManagementSystem::new());

    let prior = log.reconstruct().await?;
    if !prior.open_orders.is_empty() {
        info!(open = prior.open_orders.len(), "Restoring open orders from event log");
        for order in prior.open_orders {
            oms.submit(order);
        }
    }

    // ── Brokers ───────────────────────────────────────────────────────────────
    let broker: Arc<dyn Broker> = if dry_run {
        Arc::new(broker::dryrun::DryRunBroker::new(50_000.0))
    } else {
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
        tradovate.spawn_refresh_task();
        Arc::new(BrokerRouter::new(tradier, tradovate))
    };

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

    // ── Market data feeds (skipped in dry-run) ────────────────────────────────
    if !dry_run {
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
    }

    info!("Zeta Field Executor ready — waiting for ZetaSignals");

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

// Pure: the decision used when the rule-engine signal is unambiguous (no LLM).
fn rule_engine_decision(p: &ipc::RuleProposalMsg) -> agent::decision::StrategyDecision {
    agent::decision::StrategyDecision {
        approved:            true,
        strategy_type:       p.strategy_type.clone(),
        contracts:           p.contracts,
        target_delta:        p.est_delta,
        target_vega:         p.est_vega,
        target_dte:          p.target_dte,
        entry_urgency:       p.entry_urgency.clone(),
        reasoning:           "Rule engine clear signal — no LLM review needed.".to_string(),
        confidence:          0.85,
        macro_concerns:      None,
        sizing_adjustment:   None,
        conditional_trigger: None,
    }
}

// Deterministic content hash of the signal's inputs — UUID v5(SHA1) of the
// key fields. Same market state + same proposal → same hash, enabling
// LLM response caching in the future backtester.
fn signal_request_hash(signal: &ipc::ZetaSignal) -> String {
    let data = format!("{}|{}|{}",
        signal.zeta_context,
        signal.proposal.strategy_type,
        signal.proposal.contracts,
    );
    uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, data.as_bytes()).to_string()
}

async fn handle_zeta_signal(
    signal: ipc::ZetaSignal,
    oms:    Arc<OrderManagementSystem>,
    agent:  Arc<StrategyAgent>,
    broker: Arc<dyn Broker>,
    log:    Arc<EventLog>,
) -> Result<()> {
    let signal_id    = uuid::Uuid::new_v4().to_string();
    let request_hash = signal_request_hash(&signal);

    // ── Persist: signal arrived ────────────────────────────────────────────────
    log.append(&ev_signal_received(
        &signal_id, &signal.symbol, &signal.proposal.strategy_type,
        signal.needs_llm, &signal.zeta_context,
    )).await?;

    // ── Effect: decision (LLM consult or rule-engine passthrough) ──────────────
    let (decision, decision_source) = if signal.needs_llm {
        let raw = agent.consult(
            &signal.zeta_context,
            &signal.llm_questions,
            &signal.proposal.clone().into(),
        ).await?;
        match execution::validate_llm_decision(raw, signal.proposal.contracts) {
            Ok(v)  => (v, "llm"),
            Err(e) => {
                tracing::warn!(signal_id = %signal_id, error = %e, "LLM decision failed validation — skipping signal");
                log.append(&ev_action_planned(&signal_id, "Skip",
                    &format!("llm_validation_failed: {e}"))).await?;
                return Ok(());
            }
        }
    } else {
        let d = execution::ValidatedDecision::from_rule_engine(rule_engine_decision(&signal.proposal));
        (d, "rule_engine")
    };

    // ── Persist: decision recorded as immutable fact ───────────────────────────
    log.append(&ev_decision_made(
        &signal_id, decision_source, &request_hash,
        decision.inner().approved,
        &decision.inner().reasoning,
        decision.inner().confidence,
        decision.inner().sizing_adjustment,
    )).await?;

    // ── Effect: query available capital ────────────────────────────────────────
    let available_bp = broker.account_buying_power().await.unwrap_or(0.0);

    // ── Pure: plan action (deterministic, no I/O) ─────────────────────────────
    let greeks = (
        signal.proposal.est_delta,
        0.0,
        signal.proposal.est_vega,
        signal.proposal.est_theta_day,
    );
    let strategy_id = uuid::Uuid::new_v4().to_string();

    let mut order = match execution::plan_action(
        &decision, &signal.chain_candidates, greeks, available_bp, &strategy_id,
    ) {
        execution::Action::Skip { ref reason } => {
            log.append(&ev_action_planned(&signal_id, "Skip", reason)).await?;
            info!(strategy = %decision.inner().strategy_type, %reason, "Trade not submitted");
            return Ok(());
        }
        execution::Action::Submit(order) => {
            log.append(&ev_action_planned(&signal_id, "Submit", &order.id.to_string())).await?;
            *order
        }
    };

    // ── Effects: submit, stage, persist ────────────────────────────────────────
    let leg_summary: Vec<_> = order.legs.iter()
        .map(|l| format!("{:?} {} x{}", l.side, l.symbol, l.quantity))
        .collect();
    info!(
        strategy   = %decision.inner().strategy_type,
        confidence = decision.inner().confidence,
        legs       = ?leg_summary,
        bp         = available_bp,
        "Executing approved trade — submitting to broker"
    );

    let broker_id = match broker.submit_order(&order).await {
        Ok(id) => id,
        Err(e) => {
            tracing::error!(
                strategy = %decision.inner().strategy_type,
                error    = %e,
                "Broker submission failed"
            );
            return Ok(());
        }
    };

    info!(%broker_id, "Broker accepted order");

    order.broker_id = Some(broker_id.clone());
    order.status    = orders::OrderStatus::Submitted;

    let order_id = oms.submit(order.clone());
    log.append(&ev_submitted(order)).await?;

    info!(%order_id, %broker_id, "Order staged in OMS and persisted");

    Ok(())
}

// ── Config ────────────────────────────────────────────────────────────────────

#[derive(Debug)]
struct Config {
    thetadata_key:       String,
    databento_key:       String,
    equity_roots:        Vec<String>,
    cme_symbols:         Vec<String>,
    anthropic_key:       String,
    tradier_token:       String,
    tradier_account_id:  String,
    tradier_sandbox:     bool,
    tradovate_username:  String,
    tradovate_password:  String,
    tradovate_cid:       i64,
    tradovate_secret:    String,
    tradovate_device_id: String,
    tradovate_demo:      bool,
    oms_log_path:        String,
}

impl Config {
    fn from_env() -> Result<Self> {
        Ok(Config {
            thetadata_key:      std::env::var("THETADATA_API_KEY").unwrap_or_default(),
            databento_key:      std::env::var("DATABENTO_API_KEY")?,
            equity_roots:       std::env::var("EQUITY_ROOTS")
                                    .unwrap_or_else(|_| "SPY,QQQ".to_string())
                                    .split(',').map(String::from).collect(),
            cme_symbols:        std::env::var("CME_SYMBOLS")
                                    .unwrap_or_else(|_| "ES.FUT,NQ.FUT".to_string())
                                    .split(',').map(String::from).collect(),
            anthropic_key:      std::env::var("ANTHROPIC_API_KEY")?,
            tradier_token:      std::env::var("TRADIER_TOKEN")?,
            tradier_account_id: std::env::var("TRADIER_ACCOUNT_ID")?,
            tradier_sandbox:    std::env::var("TRADIER_SANDBOX")
                                    .map(|v| v == "true" || v == "1")
                                    .unwrap_or(true),
            tradovate_username:  std::env::var("TRADOVATE_USERNAME")?,
            tradovate_password:  std::env::var("TRADOVATE_PASSWORD")?,
            tradovate_cid:       std::env::var("TRADOVATE_CID")
                                     .unwrap_or_default().parse().unwrap_or(0),
            tradovate_secret:    std::env::var("TRADOVATE_SECRET")?,
            tradovate_device_id: std::env::var("TRADOVATE_DEVICE_ID")
                                     .unwrap_or_else(|_| uuid::Uuid::new_v4().to_string()),
            tradovate_demo:      std::env::var("TRADOVATE_DEMO")
                                     .map(|v| v == "true" || v == "1")
                                     .unwrap_or(true),
            oms_log_path:        std::env::var("OMS_LOG_PATH")
                                     .unwrap_or_else(|_| "/var/log/trading/oms.jsonl".to_string()),
        })
    }

    fn dry_run_defaults() -> Self {
        Config {
            thetadata_key:       "dry-run".to_string(),
            databento_key:       "dry-run".to_string(),
            equity_roots:        vec!["SPY".to_string()],
            cme_symbols:         vec!["ES.FUT".to_string()],
            anthropic_key:       std::env::var("ANTHROPIC_API_KEY").unwrap_or_else(|_| "dry-run".to_string()),
            tradier_token:       "dry-run".to_string(),
            tradier_account_id:  "dry-run".to_string(),
            tradier_sandbox:     true,
            tradovate_username:  "dry-run".to_string(),
            tradovate_password:  "dry-run".to_string(),
            tradovate_cid:       0,
            tradovate_secret:    "dry-run".to_string(),
            tradovate_device_id: uuid::Uuid::new_v4().to_string(),
            tradovate_demo:      true,
            oms_log_path:        "/tmp/zeta_dryrun.jsonl".to_string(),
        }
    }
}
