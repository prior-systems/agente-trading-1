use super::Broker;
use crate::orders::{Order, OrderLeg, OrderSide, OrderStatus, OrderType, TimeInForce};
use anyhow::Result;
use reqwest::Client;
use serde::Deserialize;
use serde_json::{json, Value};

const ALPACA_BASE_PAPER: &str = "https://paper-api.alpaca.markets/v2";
const ALPACA_BASE_LIVE:  &str = "https://api.alpaca.markets/v2";

pub struct AlpacaBroker {
    client:   Client,
    base_url: String,
    key_id:   String,
    secret:   String,
}

impl AlpacaBroker {
    pub fn new(key_id: String, secret: String, paper: bool) -> Self {
        AlpacaBroker {
            client:   Client::new(),
            base_url: if paper { ALPACA_BASE_PAPER } else { ALPACA_BASE_LIVE }.to_string(),
            key_id,
            secret,
        }
    }

    fn headers(&self) -> [(&'static str, String); 2] {
        [
            ("APCA-API-KEY-ID",     self.key_id.clone()),
            ("APCA-API-SECRET-KEY", self.secret.clone()),
        ]
    }

    fn leg_to_json(leg: &OrderLeg) -> Value {
        let side = match leg.side {
            OrderSide::Buy  => "buy",
            OrderSide::Sell => "sell",
        };
        let order_type = match leg.order_type {
            OrderType::Market    => "market",
            OrderType::Limit     => "limit",
            OrderType::Stop      => "stop",
            OrderType::StopLimit => "stop_limit",
        };
        let mut body = json!({
            "symbol":     leg.symbol,
            "qty":        leg.quantity.to_string(),
            "side":       side,
            "type":       order_type,
        });
        if let Some(lp) = leg.limit_price {
            body["limit_price"] = json!(format!("{:.4}", lp));
        }
        if let Some(sp) = leg.stop_price {
            body["stop_price"] = json!(format!("{:.4}", sp));
        }
        body
    }

    fn tif_str(tif: TimeInForce) -> &'static str {
        match tif {
            TimeInForce::Day => "day",
            TimeInForce::GTC => "gtc",
            TimeInForce::IOC => "ioc",
            TimeInForce::FOK => "fok",
        }
    }
}

#[derive(Deserialize)]
struct AlpacaOrderResponse {
    id: String,
    status: String,
}

#[async_trait::async_trait]
impl Broker for AlpacaBroker {
    async fn submit_order(&self, order: &Order) -> Result<String> {
        let url = format!("{}/orders", self.base_url);

        let body = if order.legs.len() == 1 {
            // Simple single-leg order
            let leg = &order.legs[0];
            let mut b = Self::leg_to_json(leg);
            b["time_in_force"] = json!(Self::tif_str(order.tif));
            b["client_order_id"] = json!(order.id.to_string());
            b
        } else {
            // Multi-leg order (spread) — Alpaca supports via legs array
            let legs_json: Vec<Value> = order.legs.iter().map(Self::leg_to_json).collect();
            json!({
                "type":            "mlo",   // multi-leg order
                "time_in_force":   Self::tif_str(order.tif),
                "client_order_id": order.id.to_string(),
                "legs":            legs_json,
            })
        };

        let [h1, h2] = self.headers();
        let resp = self.client
            .post(&url)
            .header(h1.0, h1.1)
            .header(h2.0, h2.1)
            .json(&body)
            .send()
            .await?;

        let status = resp.status();
        let text = resp.text().await?;
        if !status.is_success() {
            anyhow::bail!("Alpaca submit_order failed [{}]: {}", status, text);
        }

        let parsed: AlpacaOrderResponse = serde_json::from_str(&text)?;
        Ok(parsed.id)
    }

    async fn cancel_order(&self, broker_id: &str) -> Result<()> {
        let url = format!("{}/orders/{}", self.base_url, broker_id);
        let [h1, h2] = self.headers();
        let resp = self.client
            .delete(&url)
            .header(h1.0, h1.1)
            .header(h2.0, h2.1)
            .send()
            .await?;

        if !resp.status().is_success() && resp.status().as_u16() != 204 {
            anyhow::bail!("Alpaca cancel_order failed: {}", resp.status());
        }
        Ok(())
    }

    async fn get_status(&self, broker_id: &str) -> Result<OrderStatus> {
        let url = format!("{}/orders/{}", self.base_url, broker_id);
        let [h1, h2] = self.headers();
        let resp = self.client
            .get(&url)
            .header(h1.0, h1.1)
            .header(h2.0, h2.1)
            .send()
            .await?;

        let v: Value = resp.json().await?;
        let status = v.get("status").and_then(|s| s.as_str()).unwrap_or("unknown");
        Ok(match status {
            "new" | "accepted" | "pending_new" => OrderStatus::Submitted,
            "partially_filled"                  => OrderStatus::PartiallyFilled,
            "filled"                            => OrderStatus::Filled,
            "canceled" | "expired"              => OrderStatus::Cancelled,
            "rejected" | "suspended"            => OrderStatus::Rejected,
            _                                   => OrderStatus::Pending,
        })
    }

    async fn account_buying_power(&self) -> Result<f64> {
        let url = format!("{}/account", self.base_url);
        let [h1, h2] = self.headers();
        let resp = self.client
            .get(&url)
            .header(h1.0, h1.1)
            .header(h2.0, h2.1)
            .send()
            .await?;

        let v: Value = resp.json().await?;
        let bp = v.get("buying_power")
            .and_then(|b| b.as_str())
            .and_then(|s| s.parse::<f64>().ok())
            .unwrap_or(0.0);
        Ok(bp)
    }
}
