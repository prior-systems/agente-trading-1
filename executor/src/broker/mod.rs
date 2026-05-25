pub mod tradier;
pub mod tradovate;

use crate::orders::{Instrument, Order, OrderStatus};
use anyhow::Result;
use std::sync::Arc;

// ── Broker trait ──────────────────────────────────────────────────────────────

#[async_trait::async_trait]
pub trait Broker: Send + Sync {
    async fn submit_order(&self, order: &Order) -> Result<String>;   // returns broker_id
    async fn cancel_order(&self, broker_id: &str) -> Result<()>;
    async fn get_status(&self, broker_id: &str) -> Result<OrderStatus>;
    async fn account_buying_power(&self) -> Result<f64>;
}

// ── BrokerRouter ──────────────────────────────────────────────────────────────
// Routes orders to the correct broker by instrument type:
//   EquityOption / Equity → options_broker (Tradier)
//   Future / FutureOption → futures_broker (Tradovate)

pub struct BrokerRouter {
    options: Arc<dyn Broker>,
    futures: Arc<dyn Broker>,
}

impl BrokerRouter {
    pub fn new(options: Arc<dyn Broker>, futures: Arc<dyn Broker>) -> Self {
        BrokerRouter { options, futures }
    }

    fn route(&self, order: &Order) -> &Arc<dyn Broker> {
        let is_futures = order.legs.iter().any(|l| {
            matches!(l.instrument, Instrument::Future | Instrument::FutureOption)
        });
        if is_futures { &self.futures } else { &self.options }
    }
}

#[async_trait::async_trait]
impl Broker for BrokerRouter {
    async fn submit_order(&self, order: &Order) -> Result<String> {
        self.route(order).submit_order(order).await
    }

    async fn cancel_order(&self, broker_id: &str) -> Result<()> {
        // Without instrument context we try options broker first
        // Callers that need futures cancellation should call the broker directly
        self.options.cancel_order(broker_id).await
    }

    async fn get_status(&self, broker_id: &str) -> Result<OrderStatus> {
        self.options.get_status(broker_id).await
    }

    async fn account_buying_power(&self) -> Result<f64> {
        let (opt, fut) = tokio::join!(
            self.options.account_buying_power(),
            self.futures.account_buying_power(),
        );
        Ok(opt.unwrap_or(0.0) + fut.unwrap_or(0.0))
    }
}
