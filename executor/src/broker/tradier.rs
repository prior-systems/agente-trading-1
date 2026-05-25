use super::Broker;
use crate::orders::{Order, OrderLeg, OrderSide, OrderStatus};
use anyhow::{bail, Result};
use reqwest::Client;
use serde::Deserialize;
use serde_json::Value;
use tracing::{debug, warn};

const BASE_LIVE:    &str = "https://api.tradier.com/v1";
const BASE_SANDBOX: &str = "https://sandbox.tradier.com/v1";

pub struct TradierBroker {
    client:     Client,
    base_url:   String,
    token:      String,
    account_id: String,
}

impl TradierBroker {
    pub fn new(token: String, account_id: String, sandbox: bool) -> Self {
        TradierBroker {
            client: Client::new(),
            base_url: if sandbox { BASE_SANDBOX } else { BASE_LIVE }.to_string(),
            token,
            account_id,
        }
    }

    fn auth(&self) -> String {
        format!("Bearer {}", self.token)
    }

    // OCC symbol with spaces → Tradier format (no spaces)
    // "SPY   231215C00450000" → "SPY231215C00450000"
    fn tradier_symbol(occ: &str) -> String {
        occ.replace(' ', "")
    }

    // First 6 chars of OCC symbol contain the root (space-padded)
    fn extract_root(occ: &str) -> String {
        occ.chars().take(6).collect::<String>().trim().to_string()
    }

    // Map our side to Tradier option side — all orders assumed opening positions
    fn option_side(side: OrderSide) -> &'static str {
        match side {
            OrderSide::Buy  => "buy_to_open",
            OrderSide::Sell => "sell_to_open",
        }
    }

    // Net limit price for the combo per unit
    // Positive = credit received, negative = debit paid
    fn net_price(legs: &[OrderLeg]) -> f64 {
        legs.iter().map(|l| match l.side {
            OrderSide::Sell =>  l.limit_price.unwrap_or(0.0),
            OrderSide::Buy  => -l.limit_price.unwrap_or(0.0),
        }).sum()
    }

    async fn submit_single(&self, leg: &OrderLeg) -> Result<String> {
        let url = format!("{}/accounts/{}/orders", self.base_url, self.account_id);

        let side     = Self::option_side(leg.side);
        let symbol   = Self::tradier_symbol(&leg.symbol);
        let price    = leg.limit_price.unwrap_or(0.0);

        let params = [
            ("class",          "option"),
            ("option_symbol",  &symbol),
            ("side",           side),
            ("quantity",       &leg.quantity.to_string()),
            ("type",           "limit"),
            ("price",          &format!("{:.2}", price)),
            ("duration",       "day"),
        ];

        let resp = self.client
            .post(&url)
            .header("Authorization", self.auth())
            .header("Accept", "application/json")
            .form(&params)
            .send()
            .await?;

        self.parse_order_response(resp).await
    }

    async fn submit_multileg(&self, order: &Order) -> Result<String> {
        let url = format!("{}/accounts/{}/orders", self.base_url, self.account_id);

        let root      = Self::extract_root(&order.legs[0].symbol);
        let net_price = Self::net_price(&order.legs).abs();

        // Build form params — Tradier uses indexed leg notation
        let mut params: Vec<(String, String)> = vec![
            ("class".to_string(),    "multileg".to_string()),
            ("symbol".to_string(),   root),
            ("type".to_string(),     "limit".to_string()),
            ("duration".to_string(), "day".to_string()),
            ("price".to_string(),    format!("{:.2}", net_price)),
        ];

        for (i, leg) in order.legs.iter().enumerate() {
            params.push((format!("option[{}][option_symbol]", i), Self::tradier_symbol(&leg.symbol)));
            params.push((format!("option[{}][side]",          i), Self::option_side(leg.side).to_string()));
            params.push((format!("option[{}][quantity]",      i), leg.quantity.to_string()));
        }

        debug!(legs = order.legs.len(), net_price, "Tradier multileg submit");

        let resp = self.client
            .post(&url)
            .header("Authorization", self.auth())
            .header("Accept", "application/json")
            .form(&params)
            .send()
            .await?;

        self.parse_order_response(resp).await
    }

    async fn parse_order_response(&self, resp: reqwest::Response) -> Result<String> {
        let status = resp.status();
        let body: Value = resp.json().await?;

        if !status.is_success() {
            let msg = body.get("fault")
                .and_then(|f| f.get("faultstring"))
                .and_then(|s| s.as_str())
                .unwrap_or("unknown error");
            bail!("Tradier API error [{}]: {}", status, msg);
        }

        // {"order": {"id": 12345, "status": "ok", "partner_id": "..."}}
        let order_id = body
            .get("order")
            .and_then(|o| o.get("id"))
            .and_then(|id| id.as_u64())
            .ok_or_else(|| anyhow::anyhow!("Tradier response missing order.id: {:?}", body))?;

        Ok(order_id.to_string())
    }
}

#[derive(Deserialize)]
struct TradierOrderStatus {
    order: TradierOrderDetail,
}

#[derive(Deserialize)]
struct TradierOrderDetail {
    status: String,
}

#[async_trait::async_trait]
impl Broker for TradierBroker {
    async fn submit_order(&self, order: &Order) -> Result<String> {
        if order.legs.is_empty() {
            bail!("Tradier: order has no legs");
        }
        if order.legs.len() == 1 {
            self.submit_single(&order.legs[0]).await
        } else {
            self.submit_multileg(order).await
        }
    }

    async fn cancel_order(&self, broker_id: &str) -> Result<()> {
        let url = format!(
            "{}/accounts/{}/orders/{}",
            self.base_url, self.account_id, broker_id
        );
        let resp = self.client
            .delete(&url)
            .header("Authorization", self.auth())
            .header("Accept", "application/json")
            .send()
            .await?;

        if !resp.status().is_success() {
            let body: Value = resp.json().await.unwrap_or(Value::Null);
            warn!(broker_id, ?body, "Tradier cancel_order non-success");
        }
        Ok(())
    }

    async fn get_status(&self, broker_id: &str) -> Result<OrderStatus> {
        let url = format!(
            "{}/accounts/{}/orders/{}",
            self.base_url, self.account_id, broker_id
        );
        let resp: TradierOrderStatus = self.client
            .get(&url)
            .header("Authorization", self.auth())
            .header("Accept", "application/json")
            .send()
            .await?
            .json()
            .await?;

        Ok(match resp.order.status.as_str() {
            "open" | "partially_filled"                  => OrderStatus::Submitted,
            "filled"                                     => OrderStatus::Filled,
            "canceled" | "expired"                       => OrderStatus::Cancelled,
            "rejected"                                   => OrderStatus::Rejected,
            _                                            => OrderStatus::Pending,
        })
    }

    async fn account_buying_power(&self) -> Result<f64> {
        let url = format!("{}/accounts/{}/balances", self.base_url, self.account_id);
        let body: Value = self.client
            .get(&url)
            .header("Authorization", self.auth())
            .header("Accept", "application/json")
            .send()
            .await?
            .json()
            .await?;

        let bp = body
            .get("balances")
            .and_then(|b| b.get("option_buying_power"))
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        Ok(bp)
    }
}
