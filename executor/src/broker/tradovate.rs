use super::Broker;
use crate::orders::{Order, OrderSide, OrderStatus, TimeInForce};
use anyhow::{bail, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

const BASE_DEMO: &str = "https://demo.tradovate.com/v1";
const BASE_LIVE: &str = "https://live.tradovate.com/v1";

// ── Auth ──────────────────────────────────────────────────────────────────────

#[derive(Serialize)]
struct AuthRequest<'a> {
    name:       &'a str,
    password:   &'a str,
    #[serde(rename = "appId")]
    app_id:     &'a str,
    #[serde(rename = "appVersion")]
    app_version: &'a str,
    #[serde(rename = "cid")]
    cid:        i64,
    sec:        &'a str,
    #[serde(rename = "deviceId")]
    device_id:  &'a str,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AuthResponse {
    access_token: Option<String>,
    user_status:  Option<String>,
    #[serde(rename = "p-token")]
    p_token:      Option<String>,   // short-lived renewal ticket
    #[serde(rename = "p-time")]
    p_time:       Option<String>,   // timestamp paired with p-token
}

// Renewal uses the p-token + p-time from the previous auth response
#[derive(Serialize)]
struct RenewRequest<'a> {
    #[serde(rename = "p-ticket")]
    p_ticket: &'a str,
    #[serde(rename = "p-time")]
    p_time:   &'a str,
    #[serde(rename = "p-captcha")]
    p_captcha: bool,
}

// ── Order placement ───────────────────────────────────────────────────────────

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PlaceOrderRequest {
    account_spec: String,
    account_id:   i64,
    action:       String,   // "Buy" | "Sell"
    symbol:       String,   // e.g. "ESM5"
    order_qty:    u64,
    order_type:   String,   // "Limit" | "Market" | "Stop"
    #[serde(skip_serializing_if = "Option::is_none")]
    price:        Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stop_price:   Option<f64>,
    time_in_force: String,  // "Day" | "GTC" | "IOC" | "FOK"
    #[serde(skip_serializing_if = "Option::is_none")]
    text:         Option<String>,  // optional order tag
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PlaceOrderResponse {
    order_id:       Option<i64>,
    failure_reason: Option<String>,
    failure_text:   Option<String>,
}

// ── Account lookup ────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct AccountEntry {
    id:   i64,
    name: String,
}

// ── Broker ───────────────────────────────────────────────────────────────────

pub struct TradovateBroker {
    client:       Client,
    base_url:     String,
    username:     String,
    password:     String,
    cid:          i64,
    secret:       String,
    device_id:    String,
    access_token: Arc<RwLock<String>>,
    // p-token + p-time for renewal without re-sending password
    p_token:      Arc<RwLock<Option<String>>>,
    p_time:       Arc<RwLock<Option<String>>>,
    account_id:   Arc<RwLock<i64>>,
    account_name: Arc<RwLock<String>>,
}

impl TradovateBroker {
    pub async fn connect(
        username:  String,
        password:  String,
        cid:       i64,
        secret:    String,
        device_id: String,
        demo:      bool,
    ) -> Result<Self> {
        let broker = TradovateBroker {
            client: Client::new(),
            base_url: if demo { BASE_DEMO } else { BASE_LIVE }.to_string(),
            username,
            password,
            cid,
            secret,
            device_id,
            access_token: Arc::new(RwLock::new(String::new())),
            p_token:      Arc::new(RwLock::new(None)),
            p_time:       Arc::new(RwLock::new(None)),
            account_id:   Arc::new(RwLock::new(0)),
            account_name: Arc::new(RwLock::new(String::new())),
        };
        broker.authenticate().await?;
        broker.load_account().await?;
        Ok(broker)
    }

    async fn authenticate(&self) -> Result<()> {
        let url = format!("{}/auth/accesstokenrequest", self.base_url);
        let req = AuthRequest {
            name:        &self.username,
            password:    &self.password,
            app_id:      "ZetaField",
            app_version: env!("CARGO_PKG_VERSION"),
            cid:         self.cid,
            sec:         &self.secret,
            device_id:   &self.device_id,
        };

        let resp: AuthResponse = self.client
            .post(&url)
            .json(&req)
            .send()
            .await?
            .json()
            .await?;

        let status = resp.user_status.as_deref().unwrap_or("unknown");
        if status != "Active" {
            bail!("Tradovate auth: unexpected userStatus = {}", status);
        }

        let token = resp.access_token
            .ok_or_else(|| anyhow::anyhow!("Tradovate auth: missing accessToken"))?;

        *self.access_token.write().await = token;
        *self.p_token.write().await = resp.p_token;
        *self.p_time.write().await  = resp.p_time;

        info!("Tradovate authenticated ({})", if self.base_url.contains("demo") { "demo" } else { "live" });
        Ok(())
    }

    // Renew using p-token (no password retransmission).
    // Falls back to full re-auth if p-token is absent or expired.
    async fn renew(&self) -> Result<()> {
        let p_tok  = self.p_token.read().await.clone();
        let p_time = self.p_time.read().await.clone();

        if let (Some(ticket), Some(time)) = (p_tok, p_time) {
            let url = format!("{}/auth/renewaccesstoken", self.base_url);
            let req = RenewRequest { p_ticket: &ticket, p_time: &time, p_captcha: false };

            let resp: AuthResponse = self.client
                .post(&url)
                .json(&req)
                .send()
                .await?
                .json()
                .await?;

            if let Some(token) = resp.access_token {
                *self.access_token.write().await = token;
                *self.p_token.write().await = resp.p_token;
                *self.p_time.write().await  = resp.p_time;
                info!("Tradovate token renewed via p-token");
                return Ok(());
            }
            warn!("Tradovate p-token renewal returned no accessToken — falling back to re-auth");
        }

        // Full re-auth fallback
        self.authenticate().await
    }

    /// Spawn a background task that renews the token every 23 hours.
    /// Call once after `connect()` — the task holds weak references via
    /// the shared Arc fields, so it stops automatically when the broker drops.
    pub fn spawn_refresh_task(self: &Arc<Self>) {
        let broker = Arc::clone(self);
        tokio::spawn(async move {
            // Tradovate tokens live ~24h; renew at 23h to stay ahead
            let interval = tokio::time::Duration::from_secs(23 * 3600);
            loop {
                tokio::time::sleep(interval).await;
                match broker.renew().await {
                    Ok(())  => {}
                    Err(e)  => warn!("Tradovate token refresh failed: {} — will retry next cycle", e),
                }
            }
        });
    }

    async fn load_account(&self) -> Result<()> {
        let accounts: Vec<AccountEntry> = self.get("/account/list").await?
            .json()
            .await?;

        let acct = accounts.into_iter().next()
            .ok_or_else(|| anyhow::anyhow!("Tradovate: no accounts found"))?;

        info!(account_id = acct.id, account_name = %acct.name, "Tradovate account loaded");
        *self.account_id.write().await   = acct.id;
        *self.account_name.write().await = acct.name;
        Ok(())
    }

    async fn bearer(&self) -> String {
        format!("Bearer {}", self.access_token.read().await)
    }

    async fn get(&self, path: &str) -> Result<reqwest::Response> {
        let url = format!("{}{}", self.base_url, path);
        Ok(self.client
            .get(&url)
            .header("Authorization", self.bearer().await)
            .send()
            .await?)
    }

    async fn post(&self, path: &str, body: Value) -> Result<reqwest::Response> {
        let url = format!("{}{}", self.base_url, path);
        Ok(self.client
            .post(&url)
            .header("Authorization", self.bearer().await)
            .json(&body)
            .send()
            .await?)
    }

    fn tif_str(tif: TimeInForce) -> &'static str {
        match tif {
            TimeInForce::Day => "Day",
            TimeInForce::GTC => "GTC",
            TimeInForce::IOC => "IOC",
            TimeInForce::FOK => "FOK",
        }
    }
}

#[async_trait::async_trait]
impl Broker for TradovateBroker {
    async fn submit_order(&self, order: &Order) -> Result<String> {
        // Tradovate: one order per leg for futures
        // For multi-leg futures spreads, each leg is a separate order
        // (calendar spreads use the spread instrument symbol directly)
        if order.legs.is_empty() {
            bail!("Tradovate: order has no legs");
        }
        if order.legs.len() > 1 {
            bail!("Tradovate: multi-leg futures not yet supported — use spread symbol");
        }

        let leg          = &order.legs[0];
        let account_id   = *self.account_id.read().await;
        let account_name = self.account_name.read().await.clone();

        let req = PlaceOrderRequest {
            account_spec:  account_name,
            account_id,
            action:        match leg.side { OrderSide::Buy => "Buy", OrderSide::Sell => "Sell" }.to_string(),
            symbol:        leg.symbol.clone(),
            order_qty:     leg.quantity,
            order_type:    match leg.order_type {
                crate::orders::OrderType::Market    => "Market",
                crate::orders::OrderType::Limit     => "Limit",
                crate::orders::OrderType::Stop      => "Stop",
                crate::orders::OrderType::StopLimit => "StopLimit",
            }.to_string(),
            price:         leg.limit_price,
            stop_price:    leg.stop_price,
            time_in_force: Self::tif_str(order.tif).to_string(),
            text:          Some(format!("strategy:{}", order.strategy_id)),
        };

        debug!(symbol = %req.symbol, action = %req.action, qty = req.order_qty, "Tradovate placeOrder");

        let resp: PlaceOrderResponse = self.post("/order/placeorder", serde_json::to_value(&req)?)
            .await?
            .json()
            .await?;

        if let Some(reason) = &resp.failure_reason {
            if reason != "None" {
                bail!(
                    "Tradovate order rejected: {} — {}",
                    reason,
                    resp.failure_text.as_deref().unwrap_or("")
                );
            }
        }

        let id = resp.order_id
            .ok_or_else(|| anyhow::anyhow!("Tradovate: missing orderId in response"))?;

        Ok(id.to_string())
    }

    async fn cancel_order(&self, broker_id: &str) -> Result<()> {
        let order_id: i64 = broker_id.parse()
            .map_err(|_| anyhow::anyhow!("Tradovate: invalid broker_id '{}'", broker_id))?;

        self.post("/order/cancelorder", json!({"orderId": order_id})).await?;
        Ok(())
    }

    async fn get_status(&self, broker_id: &str) -> Result<OrderStatus> {
        let resp: Value = self.get(&format!("/order/{}", broker_id))
            .await?
            .json()
            .await?;

        let status = resp.get("ordStatus")
            .and_then(|s| s.as_str())
            .unwrap_or("Unknown");

        Ok(match status {
            "Working" | "PendingNew" | "Triggered" => OrderStatus::Submitted,
            "Canceled" | "Expired"                  => OrderStatus::Cancelled,
            "Filled"                                 => OrderStatus::Filled,
            "Rejected"                               => OrderStatus::Rejected,
            _                                        => OrderStatus::Pending,
        })
    }

    async fn account_buying_power(&self) -> Result<f64> {
        let account_id = *self.account_id.read().await;
        let resp: Value = self.post("/cashbalance/getcashbalancesnapshot",
            json!({"accountId": account_id}))
            .await?
            .json()
            .await?;

        let available = resp.get("totalCashValue")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        Ok(available)
    }
}
