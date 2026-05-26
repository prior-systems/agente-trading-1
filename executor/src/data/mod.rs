pub mod thetadata;
pub mod databento;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// ── Canonical market events ───────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OptionQuoteEvent {
    pub ts: DateTime<Utc>,
    pub root: String,
    pub expiration: String,   // YYYYMMDD
    pub strike: f64,
    pub right: OptionRight,
    pub bid: f64,
    pub ask: f64,
    pub bid_size: u32,
    pub ask_size: u32,
    pub underlying_price: f64,
    pub implied_vol: f64,
    // 1st order Greeks (from ThetaData)
    pub delta: f64,
    pub theta: f64,
    pub vega: f64,
    pub rho: f64,
    // 2nd order Greeks (from ThetaData)
    pub gamma: f64,
    pub vanna: f64,
    pub charm: f64,
    pub vomma: f64,
    pub veta: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FuturesTradeEvent {
    pub ts_ns: i64,           // nanoseconds since epoch
    pub instrument_id: u64,
    pub raw_symbol: String,   // e.g. "ESM5"
    pub price: f64,
    pub size: u64,
    pub side: Side,
    pub sequence: u64,
}

// L1 best bid/ask snapshot from Databento mbp-1 schema (Standard plan live data)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FuturesMBP1Event {
    pub ts_ns: i64,
    pub instrument_id: u64,
    pub bid_px: f64,
    pub ask_px: f64,
    pub bid_sz: u32,
    pub ask_sz: u32,
    // Incremental OFI: Δbid_sz - Δask_sz vs previous tick
    pub ofi: i64,
    pub sequence: u64,
}

// Kept for historical data (MBO available in Databento 1-month history, not live)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FuturesMBOEvent {
    pub ts_ns: i64,
    pub instrument_id: u64,
    pub order_id: u64,
    pub price: f64,
    pub size: u64,
    pub side: Side,
    pub action: MBOAction,
    pub sequence: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MarketEvent {
    OptionQuote(OptionQuoteEvent),
    FuturesTrade(FuturesTradeEvent),
    FuturesMBP1(FuturesMBP1Event),
    FuturesMBO(FuturesMBOEvent),    // historical only
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]   // Julia sends "call" / "put"
pub enum OptionRight { Call, Put }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Side { Bid, Ask, None }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MBOAction { Add, Cancel, Modify, Trade, Fill, Clear }

impl TryFrom<char> for MBOAction {
    type Error = anyhow::Error;
    fn try_from(c: char) -> anyhow::Result<Self> {
        match c {
            'A' => Ok(MBOAction::Add),
            'C' => Ok(MBOAction::Cancel),
            'M' => Ok(MBOAction::Modify),
            'T' => Ok(MBOAction::Trade),
            'F' => Ok(MBOAction::Fill),
            'R' => Ok(MBOAction::Clear),
            _   => Err(anyhow::anyhow!("Unknown MBO action: {}", c)),
        }
    }
}

impl TryFrom<char> for Side {
    type Error = anyhow::Error;
    fn try_from(c: char) -> anyhow::Result<Self> {
        match c {
            'B' => Ok(Side::Bid),
            'A' => Ok(Side::Ask),
            'N' | 'Z' => Ok(Side::None),
            _   => Err(anyhow::anyhow!("Unknown side: {}", c)),
        }
    }
}
