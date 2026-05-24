mod data;
mod broker;
mod orders;

use anyhow::Result;
use tracing::info;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    info!("Zeta Field Executor starting");

    let config = Config::from_env()?;
    let oms    = orders::OrderManagementSystem::new();

    // Spin up data feeds concurrently
    let (td_tx, mut td_rx) = tokio::sync::mpsc::channel(4096);
    let (db_tx, mut db_rx) = tokio::sync::mpsc::channel(4096);

    let td_feed = data::thetadata::ThetaDataFeed::new(config.thetadata_key.clone(), td_tx);
    let db_feed = data::databento::DatabentoFeed::new(config.databento_key.clone(), db_tx);

    // Subscribe to configured instruments
    tokio::spawn(async move {
        td_feed.stream_options(&config.equity_roots).await
            .expect("ThetaData feed failed");
    });

    tokio::spawn(async move {
        db_feed.stream_futures(&config.cme_symbols).await
            .expect("Databento feed failed");
    });

    // Main event loop: consume market data, route hedge signals to broker
    loop {
        tokio::select! {
            Some(event) = td_rx.recv() => {
                oms.on_equity_event(event).await?;
            }
            Some(event) = db_rx.recv() => {
                oms.on_futures_event(event).await?;
            }
        }
    }
}

#[derive(Debug)]
struct Config {
    thetadata_key: String,
    databento_key: String,
    equity_roots:  Vec<String>,
    cme_symbols:   Vec<String>,
    broker:        String,
}

impl Config {
    fn from_env() -> Result<Self> {
        Ok(Config {
            thetadata_key: std::env::var("THETADATA_API_KEY")?,
            databento_key: std::env::var("DATABENTO_API_KEY")?,
            equity_roots:  std::env::var("EQUITY_ROOTS")
                               .unwrap_or_default()
                               .split(',').map(String::from).collect(),
            cme_symbols:   std::env::var("CME_SYMBOLS")
                               .unwrap_or_default()
                               .split(',').map(String::from).collect(),
            broker:        std::env::var("BROKER").unwrap_or("alpaca".to_string()),
        })
    }
}
