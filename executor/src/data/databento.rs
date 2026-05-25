use super::{FuturesMBP1Event, FuturesTradeEvent, MarketEvent, Side};
use anyhow::Result;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tokio::sync::mpsc::Sender;
use tokio_tungstenite::connect_async;
use tracing::{error, info, warn};

const DB_LIVE_URL: &str = "wss://live.databento.com/v0/live";

pub struct DatabentoFeed {
    api_key: String,
    tx: Sender<MarketEvent>,
}

#[derive(Serialize)]
struct AuthMessage<'a> {
    auth:    &'a str,
    dataset: &'a str,
}

#[derive(Serialize)]
struct SubscribeMessage<'a> {
    action:   &'a str,
    schema:   &'a str,
    symbols:  &'a str,
    encoding: &'a str,   // request JSON-encoded records
}

// Databento DBN record — fields for mbp-1 + trades
#[derive(Debug, Deserialize)]
struct DBNRecord {
    #[serde(rename = "hd")]
    header: DBNHeader,
    // Trade price/size (also present in mbp-1 as last aggressor)
    #[serde(default)]
    price: i64,
    #[serde(default)]
    size: u64,
    #[serde(default)]
    side: Option<String>,
    // MBP-1: best bid/ask (level 0)
    #[serde(default, rename = "bid_px_00")]
    bid_px: i64,
    #[serde(default, rename = "ask_px_00")]
    ask_px: i64,
    #[serde(default, rename = "bid_sz_00")]
    bid_sz: u32,
    #[serde(default, rename = "ask_sz_00")]
    ask_sz: u32,
    #[serde(default)]
    sequence: u64,
}

#[derive(Debug, Deserialize)]
struct DBNHeader {
    #[serde(rename = "ts_event")]
    ts_event: i64,
    #[serde(rename = "instrument_id")]
    instrument_id: u64,
    rtype: u8,   // 1=mbp-1, 4=trades
}

// Per-instrument state for incremental OFI from MBP-1 ticks
struct BookState {
    bid_sz: u32,
    ask_sz: u32,
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

        if let Some(Ok(Message::Text(resp))) = read.next().await {
            let v: serde_json::Value = serde_json::from_str(&resp)?;
            if v.get("type").and_then(|t| t.as_str()) != Some("auth_response") {
                anyhow::bail!("Databento auth failed: {}", resp);
            }
            info!("Databento authenticated");
        }

        // Subscribe to mbp-1 (L1 bid/ask — Standard plan live) + trades
        // MBO is only available in historical (≤1 month back), not live
        let sym_str = symbols.join(",");
        for schema in &["mbp-1", "trades"] {
            let sub = serde_json::to_string(&SubscribeMessage {
                action:   "subscribe",
                schema,
                symbols:  &sym_str,
                encoding: "json",
            })?;
            write.send(Message::Text(sub.into())).await?;
        }
        info!("Databento subscribed: mbp-1 + trades for {} symbols", symbols.len());

        // OFI: track previous best bid/ask sizes per instrument
        let mut book: HashMap<u64, BookState> = HashMap::new();

        while let Some(msg) = read.next().await {
            match msg {
                Ok(Message::Text(text)) => {
                    match self.parse_record(&text, &mut book) {
                        Ok(Some(event)) => {
                            if self.tx.send(event).await.is_err() {
                                break;
                            }
                        }
                        Ok(None) => {}
                        Err(e) => warn!("Databento parse error: {} | raw: {}", e, &text[..text.len().min(100)]),
                    }
                }
                Ok(Message::Binary(bytes)) => {
                    // Databento may send binary DBN frames; log and skip until
                    // we add a proper DBN decoder
                    warn!("Databento binary frame ({} bytes) — DBN decoder not implemented", bytes.len());
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

    fn parse_record(&self, text: &str, book: &mut HashMap<u64, BookState>) -> Result<Option<MarketEvent>> {
        let rec: DBNRecord = serde_json::from_str(text)?;
        let hd = &rec.header;

        let side = rec.side.as_deref()
            .and_then(|s| s.chars().next())
            .map(|c| Side::try_from(c).unwrap_or(Side::None))
            .unwrap_or(Side::None);

        match hd.rtype {
            4 => {
                // Trade record
                Ok(Some(MarketEvent::FuturesTrade(FuturesTradeEvent {
                    ts_ns:         hd.ts_event,
                    instrument_id: hd.instrument_id,
                    raw_symbol:    String::new(),
                    price:         rec.price as f64 / 1e9,
                    size:          rec.size,
                    side,
                    sequence:      rec.sequence,
                })))
            }
            1 => {
                // MBP-1: best bid/ask snapshot + incremental OFI
                let bid_px = rec.bid_px as f64 / 1e9;
                let ask_px = rec.ask_px as f64 / 1e9;
                let bid_sz = rec.bid_sz;
                let ask_sz = rec.ask_sz;

                // OFI = Δbid_sz - Δask_sz (positive = net buying pressure)
                let ofi = match book.get(&hd.instrument_id) {
                    Some(prev) => {
                        (bid_sz as i64 - prev.bid_sz as i64)
                        - (ask_sz as i64 - prev.ask_sz as i64)
                    }
                    None => 0,
                };

                book.insert(hd.instrument_id, BookState { bid_sz, ask_sz });

                Ok(Some(MarketEvent::FuturesMBP1(FuturesMBP1Event {
                    ts_ns:         hd.ts_event,
                    instrument_id: hd.instrument_id,
                    bid_px,
                    ask_px,
                    bid_sz,
                    ask_sz,
                    ofi,
                    sequence:      rec.sequence,
                })))
            }
            _ => Ok(None),
        }
    }
}
