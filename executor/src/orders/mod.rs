pub mod persistence;

use crate::data::MarketEvent;
use anyhow::Result;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use uuid::Uuid;

// ── Order types ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OrderSide { Buy, Sell }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OrderType { Market, Limit, Stop, StopLimit }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TimeInForce { Day, GTC, IOC, FOK }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Instrument { EquityOption, Future, FutureOption }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OrderStatus {
    Pending,
    Submitted,
    PartiallyFilled,
    Filled,
    Cancelled,
    Rejected,
}

// A single leg of a potentially multi-leg order
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderLeg {
    pub instrument:  Instrument,
    pub symbol:      String,       // raw symbol: e.g. "AAPL 250117C00200000" or "ESM5"
    pub side:        OrderSide,
    pub quantity:    u64,
    pub order_type:  OrderType,
    pub limit_price: Option<f64>,
    pub stop_price:  Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Order {
    pub id:          Uuid,
    pub created_at:  DateTime<Utc>,
    pub strategy_id: String,          // which strategy generated this order
    pub legs:        Vec<OrderLeg>,   // 1 = single, >1 = spread/combo
    pub tif:         TimeInForce,
    pub status:      OrderStatus,
    pub filled_qty:  u64,
    pub avg_fill_px: f64,
    pub broker_id:   Option<String>,  // ID assigned by the broker
    // Greeks at time of order — for audit and P&L attribution
    pub delta_at_entry:  f64,
    pub gamma_at_entry:  f64,
    pub vega_at_entry:   f64,
    pub theta_at_entry:  f64,
}

impl Order {
    pub fn new(
        strategy_id: impl Into<String>,
        legs: Vec<OrderLeg>,
        tif: TimeInForce,
        greeks: (f64, f64, f64, f64),
    ) -> Self {
        Order {
            id: Uuid::new_v4(),
            created_at: Utc::now(),
            strategy_id: strategy_id.into(),
            legs,
            tif,
            status: OrderStatus::Pending,
            filled_qty: 0,
            avg_fill_px: 0.0,
            broker_id: None,
            delta_at_entry: greeks.0,
            gamma_at_entry: greeks.1,
            vega_at_entry:  greeks.2,
            theta_at_entry: greeks.3,
        }
    }
}

// ── Position tracking ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Position {
    pub symbol:      String,
    pub quantity:    i64,          // signed: positive=long, negative=short
    pub avg_cost:    f64,
    pub instrument:  Instrument,
    // Live Greeks (updated on each market event)
    pub delta:  f64,
    pub gamma:  f64,
    pub theta:  f64,
    pub vega:   f64,
    pub vanna:  f64,
    pub charm:  f64,
}

impl Position {
    pub fn portfolio_contribution(&self) -> GreeksContribution {
        let sign = self.quantity as f64;
        GreeksContribution {
            delta: sign * self.delta,
            gamma: sign * self.gamma,
            theta: sign * self.theta,
            vega:  sign * self.vega,
            vanna: sign * self.vanna,
            charm: sign * self.charm,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct GreeksContribution {
    pub delta: f64,
    pub gamma: f64,
    pub theta: f64,
    pub vega:  f64,
    pub vanna: f64,
    pub charm: f64,
}

// ── Order Management System ───────────────────────────────────────────────────

pub struct OrderManagementSystem {
    orders:    Arc<DashMap<Uuid, Order>>,
    positions: Arc<DashMap<String, Position>>,
}

impl OrderManagementSystem {
    pub fn new() -> Self {
        OrderManagementSystem {
            orders:    Arc::new(DashMap::new()),
            positions: Arc::new(DashMap::new()),
        }
    }

    pub async fn on_equity_event(&self, event: MarketEvent) -> Result<()> {
        match event {
            MarketEvent::OptionQuote(q) => {
                // Update live Greeks for open equity option positions
                let key = format!("{}_{}_{:.0}_{:?}", q.root, q.expiration, q.strike, q.right);
                if let Some(mut pos) = self.positions.get_mut(&key) {
                    pos.delta = q.delta;
                    pos.gamma = q.gamma;
                    pos.theta = q.theta;
                    pos.vega  = q.vega;
                    pos.vanna = q.vanna;
                    pos.charm = q.charm;
                }
            }
            _ => {}
        }
        Ok(())
    }

    pub async fn on_futures_event(&self, event: MarketEvent) -> Result<()> {
        match event {
            MarketEvent::FuturesTrade(t) => {
                // Update mark price for futures positions
                let key = format!("FUT_{}", t.instrument_id);
                if let Some(mut pos) = self.positions.get_mut(&key) {
                    // Mark to market — futures delta = 1 per contract
                    let _ = t.price;  // used for P&L calculation (not shown)
                    let _ = pos.delta;
                }
            }
            MarketEvent::FuturesMBO(mbo) => {
                // Order flow tracking for zeta field — not updating positions
                let _ = mbo;
            }
            _ => {}
        }
        Ok(())
    }

    // Portfolio-level Greeks (sum across all positions)
    pub fn portfolio_greeks(&self) -> GreeksContribution {
        self.positions.iter().fold(GreeksContribution::default(), |mut acc, pos| {
            let c = pos.portfolio_contribution();
            acc.delta += c.delta;
            acc.gamma += c.gamma;
            acc.theta += c.theta;
            acc.vega  += c.vega;
            acc.vanna += c.vanna;
            acc.charm += c.charm;
            acc
        })
    }

    // Delta exposure: should we hedge?
    // Returns the required hedge quantity in the underlying
    pub fn delta_hedge_required(&self, target_delta: f64, delta_tolerance: f64) -> Option<f64> {
        let pg = self.portfolio_greeks();
        let delta_error = pg.delta - target_delta;
        if delta_error.abs() > delta_tolerance {
            Some(-delta_error)   // buy/sell underlying to neutralize
        } else {
            None
        }
    }

    pub fn submit(&self, order: Order) -> Uuid {
        let id = order.id;
        self.orders.insert(id, order);
        id
    }

    pub fn get_order(&self, id: &Uuid) -> Option<Order> {
        self.orders.get(id).map(|o| o.clone())
    }

    pub fn open_orders(&self) -> Vec<Order> {
        self.orders.iter()
            .filter(|o| matches!(o.status, OrderStatus::Pending | OrderStatus::Submitted | OrderStatus::PartiallyFilled))
            .map(|o| o.clone())
            .collect()
    }
}
