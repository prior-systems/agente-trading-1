use super::{MarketEvent, OptionQuoteEvent, OptionRight};
use anyhow::Result;
use chrono::Utc;
use futures_util::StreamExt;
use serde_json::Value;
use tokio::sync::mpsc::Sender;
use tokio_tungstenite::connect_async;
use tracing::{error, info, warn};

const TD_WS_URL: &str = "wss://stream.thetadata.us/v2/stream";

pub struct ThetaDataFeed {
    api_key: String,
    tx: Sender<MarketEvent>,
}

impl ThetaDataFeed {
    pub fn new(api_key: String, tx: Sender<MarketEvent>) -> Self {
        ThetaDataFeed { api_key, tx }
    }

    pub async fn stream_options(&self, roots: &[String]) -> Result<()> {
        let url = format!("{}?token={}", TD_WS_URL, self.api_key);
        let (ws_stream, _) = connect_async(&url).await?;
        let (_, mut read) = ws_stream.split();

        // Subscribe to quote Greeks for each root (all strikes/expirations)
        info!("ThetaData WebSocket connected, subscribing to {} roots", roots.len());

        while let Some(msg) = read.next().await {
            match msg {
                Ok(tokio_tungstenite::tungstenite::Message::Text(text)) => {
                    match self.parse_quote_greeks(&text) {
                        Ok(Some(event)) => {
                            if self.tx.send(MarketEvent::OptionQuote(event)).await.is_err() {
                                break; // receiver dropped
                            }
                        }
                        Ok(None) => {}  // heartbeat or non-quote message
                        Err(e) => warn!("ThetaData parse error: {}", e),
                    }
                }
                Ok(tokio_tungstenite::tungstenite::Message::Close(_)) => {
                    info!("ThetaData WebSocket closed");
                    break;
                }
                Err(e) => {
                    error!("ThetaData WebSocket error: {}", e);
                    break;
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn parse_quote_greeks(&self, text: &str) -> Result<Option<OptionQuoteEvent>> {
        let v: Value = serde_json::from_str(text)?;

        // ThetaData streaming format: {"type": "QUOTE_GREEKS", "data": [...]}
        let msg_type = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
        if msg_type != "QUOTE_GREEKS" && msg_type != "TRADE_GREEKS" {
            return Ok(None);
        }

        let d = match v.get("data") {
            Some(d) => d,
            None => return Ok(None),
        };

        // Parse fields — ThetaData returns arrays positionally
        // Format: [ms_of_day, bid, ask, bid_size, ask_size, gamma, vanna, charm, vomma, veta,
        //          implied_vol, iv_error, ms_of_day2, underlying_price, date,
        //          root, exp, strike, right]
        let get_f64 = |key: &str| d.get(key).and_then(|v| v.as_f64()).unwrap_or(0.0);
        let get_u32 = |key: &str| d.get(key).and_then(|v| v.as_u64()).unwrap_or(0) as u32;
        let get_str = |key: &str| d.get(key).and_then(|v| v.as_str()).unwrap_or("").to_string();

        let right = match get_str("right").as_str() {
            "C" => OptionRight::Call,
            "P" => OptionRight::Put,
            _   => return Ok(None),
        };

        Ok(Some(OptionQuoteEvent {
            ts: Utc::now(),
            root: get_str("root"),
            expiration: get_str("exp"),
            strike: get_f64("strike") / 1000.0,   // 1/10th cent → dollars
            right,
            bid: get_f64("bid"),
            ask: get_f64("ask"),
            bid_size: get_u32("bid_size"),
            ask_size: get_u32("ask_size"),
            underlying_price: get_f64("underlying_price"),
            implied_vol: get_f64("implied_vol"),
            // 1st order
            delta: get_f64("delta"),
            theta: get_f64("theta"),
            vega:  get_f64("vega")  / 100.0,
            rho:   get_f64("rho")   / 100.0,
            // 2nd order
            gamma: get_f64("gamma"),
            vanna: get_f64("vanna"),
            charm: get_f64("charm"),
            vomma: get_f64("vomma"),
            veta:  get_f64("veta"),
        }))
    }
}
