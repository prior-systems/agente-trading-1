use super::Broker;
use crate::orders::{Order, OrderStatus};
use anyhow::Result;
use tracing::info;
use uuid::Uuid;

pub struct DryRunBroker {
    buying_power: f64,
}

impl DryRunBroker {
    pub fn new(buying_power: f64) -> Self {
        DryRunBroker { buying_power }
    }
}

#[async_trait::async_trait]
impl Broker for DryRunBroker {
    async fn submit_order(&self, order: &Order) -> Result<String> {
        let fake_id = format!("DRY-{}", &Uuid::new_v4().to_string()[..8]);
        for leg in &order.legs {
            info!(
                broker_id = %fake_id,
                side      = ?leg.side,
                symbol    = %leg.symbol,
                qty       = leg.quantity,
                price     = leg.limit_price.unwrap_or(0.0),
                "DRY-RUN order leg"
            );
        }
        Ok(fake_id)
    }

    async fn cancel_order(&self, broker_id: &str) -> Result<()> {
        info!(%broker_id, "DRY-RUN: cancel");
        Ok(())
    }

    async fn get_status(&self, _broker_id: &str) -> Result<OrderStatus> {
        Ok(OrderStatus::Filled)
    }

    async fn account_buying_power(&self) -> Result<f64> {
        Ok(self.buying_power)
    }
}
