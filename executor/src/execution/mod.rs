use crate::orders::{Instrument, Order, OrderLeg, OrderSide, OrderType, TimeInForce};
use anyhow::{bail, Result};
use serde::Deserialize;
use tracing::debug;

// ── StrikeCandidate — mirrors zeta/src/data/chain.jl ────────────────────────

#[derive(Debug, Clone, Deserialize)]
pub struct StrikeCandidate {
    pub root:             String,
    pub expiration:       String,   // YYYYMMDD
    pub strike:           f64,
    pub right:            String,   // "call" or "put"
    pub dte:              i32,
    pub delta:            f64,
    pub gamma:            f64,
    pub theta:            f64,
    pub vega:             f64,
    pub implied_vol:      f64,
    pub bid:              f64,
    pub ask:              f64,
    pub mid:              f64,
    pub bid_size:         i32,
    pub ask_size:         i32,
    pub underlying_price: f64,
    pub open_interest:    i64,
    pub spread_pct:       f64,
}

impl StrikeCandidate {
    pub fn is_call(&self) -> bool { self.right == "call" }
    pub fn is_put(&self)  -> bool { self.right == "put" }

    // OCC option symbol: "ROOT YYMMDD[C/P]STRIKE"
    pub fn occ_symbol(&self) -> String {
        let right_char = if self.is_call() { 'C' } else { 'P' };
        let exp_short  = &self.expiration[2..];   // YYMMDD from YYYYMMDD
        let strike_int = (self.strike * 1000.0).round() as u64;
        format!("{:<6}{}{}{:08}", self.root, exp_short, right_char, strike_int)
    }
}

// ── Strike selection ──────────────────────────────────────────────────────────

fn closest_delta<'a>(
    candidates: &'a [StrikeCandidate],
    target_delta: f64,
    is_call: bool,
    target_dte: u32,
    dte_tol: i32,
) -> Option<&'a StrikeCandidate> {
    candidates.iter()
        .filter(|c| {
            c.is_call() == is_call
                && (c.dte - target_dte as i32).abs() <= dte_tol
                && c.spread_pct <= 0.25      // 25% max bid-ask spread
                && c.bid >= 0.05             // min $0.05 bid
                && c.open_interest >= 100    // min OI
        })
        .min_by(|a, b| {
            let da = (a.delta - target_delta).abs();
            let db = (b.delta - target_delta).abs();
            da.partial_cmp(&db).unwrap()
        })
}

// ── Order leg constructors ────────────────────────────────────────────────────

fn buy_leg(c: &StrikeCandidate, qty: u64) -> OrderLeg {
    OrderLeg {
        instrument:  Instrument::EquityOption,
        symbol:      c.occ_symbol(),
        side:        OrderSide::Buy,
        quantity:    qty,
        order_type:  OrderType::Limit,
        limit_price: Some((c.bid + c.ask) / 2.0),  // mid limit
        stop_price:  None,
    }
}

fn sell_leg(c: &StrikeCandidate, qty: u64) -> OrderLeg {
    OrderLeg {
        instrument:  Instrument::EquityOption,
        symbol:      c.occ_symbol(),
        side:        OrderSide::Sell,
        quantity:    qty,
        order_type:  OrderType::Limit,
        limit_price: Some((c.bid + c.ask) / 2.0),
        stop_price:  None,
    }
}

// ── Strategy → OrderLegs ──────────────────────────────────────────────────────

pub fn build_order(
    strategy_type: &str,
    candidates:    &[StrikeCandidate],
    contracts:     u32,
    target_dte:    u32,
    strategy_id:   &str,
    greeks:        (f64, f64, f64, f64),   // (delta, gamma, vega, theta)
) -> Result<Order> {
    let qty  = contracts as u64;
    let tol  = 7i32;  // ±7 days DTE tolerance
    let legs = match strategy_type {

        "IronCondor" => {
            // Sell OTM call spread + sell OTM put spread
            // Short strikes: ~0.16 delta (1σ)
            // Long strikes:  ~0.05 delta (2σ) — protection
            let sc = closest_delta(candidates,  0.16, true,  target_dte, tol);
            let lc = closest_delta(candidates,  0.05, true,  target_dte, tol);
            let sp = closest_delta(candidates, -0.16, false, target_dte, tol);
            let lp = closest_delta(candidates, -0.05, false, target_dte, tol);

            match (sc, lc, sp, lp) {
                (Some(sc), Some(lc), Some(sp), Some(lp)) => {
                    // Verify strikes are in correct order
                    if lc.strike <= sc.strike && lp.strike >= sp.strike {
                        vec![sell_leg(sc, qty), buy_leg(lc, qty),
                             sell_leg(sp, qty), buy_leg(lp, qty)]
                    } else {
                        bail!("IronCondor: strike ordering invalid (lc={:.0} sc={:.0} sp={:.0} lp={:.0})",
                              lc.strike, sc.strike, sp.strike, lp.strike)
                    }
                }
                _ => bail!("IronCondor: could not find all 4 strikes near {}-day expiry", target_dte),
            }
        }

        "Strangle" => {
            // Sell OTM call + put — undefined risk
            let sc = closest_delta(candidates,  0.20, true,  target_dte, tol);
            let sp = closest_delta(candidates, -0.20, false, target_dte, tol);
            match (sc, sp) {
                (Some(sc), Some(sp)) => vec![sell_leg(sc, qty), sell_leg(sp, qty)],
                _ => bail!("Strangle: could not find both strikes"),
            }
        }

        "IronButterfly" => {
            // Sell ATM straddle + buy OTM wings
            let sc = closest_delta(candidates,  0.50, true,  target_dte, tol);
            let sp = closest_delta(candidates, -0.50, false, target_dte, tol);
            let wc = closest_delta(candidates,  0.10, true,  target_dte, tol);
            let wp = closest_delta(candidates, -0.10, false, target_dte, tol);
            match (sc, sp, wc, wp) {
                (Some(sc), Some(sp), Some(wc), Some(wp)) =>
                    vec![sell_leg(sc, qty), sell_leg(sp, qty),
                         buy_leg(wc, qty),  buy_leg(wp, qty)],
                _ => bail!("IronButterfly: could not find all 4 strikes"),
            }
        }

        "LongStraddle" => {
            // Buy ATM call + put
            let bc = closest_delta(candidates,  0.50, true,  target_dte, tol);
            let bp = closest_delta(candidates, -0.50, false, target_dte, tol);
            match (bc, bp) {
                (Some(bc), Some(bp)) => vec![buy_leg(bc, qty), buy_leg(bp, qty)],
                _ => bail!("LongStraddle: could not find ATM strikes"),
            }
        }

        "LongStrangle" => {
            // Buy OTM call + put
            let bc = closest_delta(candidates,  0.30, true,  target_dte, tol);
            let bp = closest_delta(candidates, -0.30, false, target_dte, tol);
            match (bc, bp) {
                (Some(bc), Some(bp)) => vec![buy_leg(bc, qty), buy_leg(bp, qty)],
                _ => bail!("LongStrangle: could not find OTM strikes"),
            }
        }

        "Backspread" => {
            // Sell 1 ATM, buy 2 OTM — long gamma, defined max loss
            // Direction from delta sign convention: positive = call backspread
            let sc = closest_delta(candidates,  0.50, true,  target_dte, tol);
            let bc = closest_delta(candidates,  0.25, true,  target_dte, tol);
            match (sc, bc) {
                (Some(sc), Some(bc)) => vec![
                    sell_leg(sc, qty),
                    buy_leg(bc, qty * 2),
                ],
                _ => bail!("Backspread: could not find strikes"),
            }
        }

        "RiskReversal" => {
            // Buy call, sell put (bullish skew play — sell put skew)
            // Negative: sell call, buy put
            let lc = closest_delta(candidates,  0.25, true,  target_dte, tol);
            let sp = closest_delta(candidates, -0.25, false, target_dte, tol);
            match (lc, sp) {
                (Some(lc), Some(sp)) => vec![buy_leg(lc, qty), sell_leg(sp, qty)],
                _ => bail!("RiskReversal: could not find 25-delta strikes"),
            }
        }

        "DeltaHedge" => {
            // Delta hedge uses futures, not options
            // Handled separately by the futures execution path
            bail!("DeltaHedge: use futures execution path, not option chain")
        }

        "DoNothing" => {
            bail!("DoNothing: no order to build")
        }

        other => bail!("Unknown strategy type: {}", other),
    };

    let net_credit: f64 = legs.iter().map(|l| match l.side {
        OrderSide::Sell =>  l.limit_price.unwrap_or(0.0),
        OrderSide::Buy  => -l.limit_price.unwrap_or(0.0),
    }).sum::<f64>() * 100.0 * qty as f64;

    debug!(
        strategy   = strategy_type,
        contracts  = qty,
        legs       = legs.len(),
        net_credit = net_credit,
        "Order built"
    );

    Ok(Order::new(strategy_id, legs, TimeInForce::Day, greeks))
}

// ── Capital requirement estimate ─────────────────────────────────────────────
// Returns the estimated capital needed to enter the order.
// For equity options: 100 shares per contract.
// For futures: no multiplier (margin is tracked separately by the FCM).
// Debit orders → net debit is the max loss.
// Credit orders → conservative estimate is 2× the credit received as margin.

pub fn required_capital(order: &Order) -> f64 {
    let is_futures = order.legs.iter()
        .any(|l| matches!(l.instrument, Instrument::Future | Instrument::FutureOption));
    let multiplier = if is_futures { 1.0 } else { 100.0 };

    let net_debit: f64 = order.legs.iter().map(|l| {
        let price = l.limit_price.unwrap_or(0.0);
        let cost  = price * l.quantity as f64 * multiplier;
        match l.side {
            crate::orders::OrderSide::Buy  =>  cost,
            crate::orders::OrderSide::Sell => -cost,
        }
    }).sum();

    if net_debit >= 0.0 {
        net_debit           // debit trade — cost is the max loss
    } else {
        net_debit.abs() * 2.0  // credit trade — rough margin: 2× premium received
    }
}

// ── Greeks aggregate for the proposed order ───────────────────────────────────

pub fn estimate_order_greeks(
    strategy_type: &str,
    candidates:    &[StrikeCandidate],
    contracts:     u32,
    target_dte:    u32,
) -> (f64, f64, f64, f64) {
    let qty  = contracts as f64 * 100.0;  // 100 shares per contract
    let tol  = 7i32;

    let sum_greeks = |deltas: &[(f64, bool, bool)]| -> (f64, f64, f64, f64) {
        deltas.iter().fold((0.0, 0.0, 0.0, 0.0), |acc, (tgt_delta, is_call, is_long)| {
            if let Some(c) = closest_delta(candidates, *tgt_delta, *is_call, target_dte, tol) {
                let sign = if *is_long { 1.0 } else { -1.0 };
                (acc.0 + sign * c.delta * qty,
                 acc.1 + sign * c.gamma * qty,
                 acc.2 + sign * c.vega  * qty,
                 acc.3 + sign * c.theta * qty)
            } else {
                acc
            }
        })
    };

    match strategy_type {
        "IronCondor" => sum_greeks(&[
            ( 0.16, true,  false), ( 0.05, true,  true),
            (-0.16, false, false), (-0.05, false, true),
        ]),
        "Strangle"      => sum_greeks(&[( 0.20, true, false), (-0.20, false, false)]),
        "LongStraddle"  => sum_greeks(&[( 0.50, true, true),  (-0.50, false, true)]),
        "LongStrangle"  => sum_greeks(&[( 0.30, true, true),  (-0.30, false, true)]),
        "RiskReversal"  => sum_greeks(&[( 0.25, true, true),  (-0.25, false, false)]),
        _ => (0.0, 0.0, 0.0, 0.0),
    }
}
