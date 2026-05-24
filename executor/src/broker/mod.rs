pub mod alpaca;

use crate::orders::{Order, OrderStatus};
use anyhow::Result;

// Broker trait — swap implementations without changing OMS
#[async_trait::async_trait]
pub trait Broker: Send + Sync {
    async fn submit_order(&self, order: &Order) -> Result<String>;   // returns broker_id
    async fn cancel_order(&self, broker_id: &str) -> Result<()>;
    async fn get_status(&self, broker_id: &str) -> Result<OrderStatus>;
    async fn account_buying_power(&self) -> Result<f64>;
}
