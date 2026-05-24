use super::{FuturesMBOEvent, FuturesTradeEvent, MBOAction, MarketEvent, Side};
use anyhow::Result;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::Sender;
use tokio_tungstenite::connect_async;
use tracing::{error, info, warn};

const DB_LIVE_URL: &str = "wss://live.databento.com/v0/live";

pub struct DatabentoFeed {
    api_key: String,
    tx: Sender<MarketEvent>,
}

// Databento live API auth + subscription messages
#[derive(Serialize)]
struct AuthMessage<'a> {
    auth:    &'a str,
    dataset: &'a str,
}

#[derive(Serialize)]
struct SubscribeMessage<'a> {
    action:  &'a str,
    schema:  &'a str,
    symbols: &'a str,
}

// Databento DBN record — subset of fields we care about
#[derive(Debug, Deserialize)]
struct DBNRecord {
    #[serde(rename = "hd")]
    header: DBNHeader,
    #[serde(default)]
    price: i64,       // fixed-point: divide by 1e9 for float
    #[serde(default)]
    size: u64,
    #[serde(default)]
    side: Option<String>,
    #[serde(default)]
    action: Option<String>,
    #[serde(default)]
    order_id: Option<u64>,
    #[serde(default)]
    sequence: u64,
}

#[derive(Debug, Deserialize)]
struct DBNHeader {
    #[serde(rename = "ts_event")]
    ts_event: i64,
    #[serde(rename = "instrument_id")]
    instrument_id: u64,
    rtype: u8,   // 0=mbo, 1=mbp-1, 2=mbp-10, 4=trades
}

impl DatabentoFeed {
    pub fn new(api_key: String, tx: Sender<MarketEvent>) -> Self {
        DatabentoFeed { api_key, tx }
    }

    pub async fn stream_futures(&self, symbols: &[String]) -> Result<()> {
        let (ws_stream, _) = connect_async(DB_LIVE_URL).await?;
        let (mut write, mut read) = ws_stream.split();

        use futures_util::SinkExt;
        use tokio_tungstenite::tungstenite::Message;

        // Auth
        let auth_msg = serde_json::to_string(&AuthMessage {
            auth:    &self.api_key,
            dataset: "GLBX.MDP3",
        })?;
        write.send(Message::Text(auth_msg.into())).await?;

        // Wait for auth ack
        if let Some(Ok(Message::Text(resp))) = read.next().await {
            let v: serde_json::Value = serde_json::from_str(&resp)?;
            if v.get("type").and_then(|t| t.as_str()) != Some("auth_response") {
                anyhow::bail!("Databento auth failed: {}", resp);
            }
            info!("Databento authenticated");
        }

        // Subscribe to MBO (most granular) + trades
        let sym_str = symbols.join(",");
        for schema in &["mbo", "trades"] {
            let sub = serde_json::to_string(&SubscribeMessage {
                action: "subscribe",
                schema,
                symbols: &sym_str,
            })?;
            write.send(Message::Text(sub.into())).await?;
        }
        info!("Databento subscribed to {} symbols", symbols.len());

        while let Some(msg) = read.next().await {
            match msg {
                Ok(Message::Text(text)) => {
                    match self.parse_record(&text) {
                        Ok(Some(event)) => {
                            if self.tx.send(event).await.is_err() {
                                break;
                            }
                        }
                        Ok(None) => {}
                        Err(e) => warn!("Databento parse error: {} | raw: {}", e, &text[..text.len().min(100)]),
                    }
                }
                Ok(Message::Close(_)) => {
                    info!("Databento WebSocket closed");
                    break;
                }
                Err(e) => {
                    error!("Databento WebSocket error: {}", e);
                    break;
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn parse_record(&self, text: &str) -> Result<Option<MarketEvent>> {
        let rec: DBNRecord = serde_json::from_str(text)?;
        let hd = &rec.header;

        let side = rec.side.as_deref()
            .and_then(|s| s.chars().next())
            .map(|c| Side::try_from(c).unwrap_or(Side::None))
            .unwrap_or(Side::None);

        match hd.rtype {
            4 => {   // trades
                Ok(Some(MarketEvent::FuturesTrade(FuturesTradeEvent {
                    ts_ns: hd.ts_event,
                    instrument_id: hd.instrument_id,
                    raw_symbol: String::new(),   // enriched separately from definition cache
                    price: rec.price as f64 / 1e9,
                    size: rec.size,
                    side,
                    sequence: rec.sequence,
                })))
            }
            0 => {   // mbo
                let action = rec.action.as_deref()
                    .and_then(|s| s.chars().next())
                    .map(|c| MBOAction::try_from(c).unwrap_or(MBOAction::Add))
                    .unwrap_or(MBOAction::Add);

                Ok(Some(MarketEvent::FuturesMBO(FuturesMBOEvent {
                    ts_ns: hd.ts_event,
                    instrument_id: hd.instrument_id,
                    order_id: rec.order_id.unwrap_or(0),
                    price: rec.price as f64 / 1e9,
                    size: rec.size,
                    side,
                    action,
                    sequence: rec.sequence,
                })))
            }
            _ => Ok(None),
        }
    }
}
