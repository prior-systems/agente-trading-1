using Distributions, Statistics

# Black-76 model for European options on futures
# Used for CME options where ThetaData doesn't provide pre-calculated Greeks

const _N  = Normal(0, 1)
const _ϕ  = x -> pdf(_N, x)
const _Φ  = x -> cdf(_N, x)

struct Black76Greeks
    price::Float64
    delta::Float64    # ∂V/∂F
    gamma::Float64    # ∂²V/∂F²
    theta::Float64    # ∂V/∂T (per calendar day)
    vega::Float64     # ∂V/∂σ (per 1pt of vol, i.e. 100%)
    rho::Float64      # ∂V/∂r
    # 2nd order
    vanna::Float64    # ∂²V/∂F∂σ = ∂delta/∂σ = ∂vega/∂F
    vomma::Float64    # ∂²V/∂σ²  (vol convexity / volga)
    charm::Float64    # ∂delta/∂T (delta bleed per day)
    veta::Float64     # ∂vega/∂T  (vega bleed per day)
    # 3rd order
    speed::Float64    # ∂³V/∂F³  = ∂gamma/∂F
    zomma::Float64    # ∂²gamma/∂σ = ∂vanna/∂F
    color::Float64    # ∂gamma/∂T (gamma bleed per day)
    ultima::Float64   # ∂³V/∂σ³  = ∂vomma/∂σ
end

# ── Core pricing ──────────────────────────────────────────────────────────────

function _d1d2(F::Float64, K::Float64, T::Float64, σ::Float64)
    σ√T = σ * √T
    d1  = (log(F / K) + 0.5 * σ^2 * T) / σ√T
    d2  = d1 - σ√T
    return d1, d2, σ√T
end

function black76_price(
    F::Float64,   # futures price
    K::Float64,   # strike
    T::Float64,   # time to expiry in years
    r::Float64,   # risk-free rate (continuous)
    σ::Float64,   # implied vol
    is_call::Bool
)::Float64
    T ≤ 0 && return max(is_call ? F - K : K - F, 0.0)
    d1, d2, _ = _d1d2(F, K, T, σ)
    df = exp(-r * T)
    if is_call
        return df * (F * _Φ(d1) - K * _Φ(d2))
    else
        return df * (K * _Φ(-d2) - F * _Φ(-d1))
    end
end

function black76_greeks(
    F::Float64,
    K::Float64,
    T::Float64,
    r::Float64,
    σ::Float64,
    is_call::Bool
)::Black76Greeks
    if T ≤ 1e-10
        # At expiry: only intrinsic, all Greeks → 0 except delta
        intrinsic = is_call ? max(F - K, 0.0) : max(K - F, 0.0)
        δ = is_call ? (F > K ? 1.0 : 0.0) : (K > F ? -1.0 : 0.0)
        return Black76Greeks(intrinsic, δ, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0)
    end

    d1, d2, σ√T = _d1d2(F, K, T, σ)
    df   = exp(-r * T)
    nd1  = _ϕ(d1)
    nd2  = _ϕ(d2)
    Nd1  = _Φ(d1)
    Nd2  = _Φ(d2)
    Nm1  = _Φ(-d1)
    Nm2  = _Φ(-d2)

    # ── 1st order ─────────────────────────────────────────────────────────────
    price = is_call ? df * (F * Nd1 - K * Nd2) : df * (K * Nm2 - F * Nm1)
    delta = is_call ? df * Nd1 : -df * Nm1
    gamma = df * nd1 / (F * σ√T)
    # theta: time decay per calendar day (365 days/year convention)
    theta_annual = is_call ?
        df * (-F * nd1 * σ / (2 * √T) + r * F * Nd1 - r * K * Nd2) :
        df * (-F * nd1 * σ / (2 * √T) - r * F * Nm1 + r * K * Nm2)
    theta = theta_annual / 365.0
    vega  = df * F * nd1 * √T           # per unit of σ (e.g. 0.01 = 1 vol pt)
    rho   = is_call ? -T * price : -T * price  # ∂V/∂r for Black-76 = -T*V

    # ── 2nd order ─────────────────────────────────────────────────────────────
    vanna  = -df * nd1 * d2 / σ          # = vega * (1 - d1/σ√T) / F
    vomma  = vega * d1 * d2 / σ          # volga
    charm  = -df * nd1 * (2*r*T - d2*σ√T) / (2 * T * σ√T)
    charm  = charm / 365.0               # per calendar day
    veta   = df * F * nd1 * √T * (r - d1*(σ/(2*√T)) + (1-d1*d2)/(2*T))
    veta   = veta / 365.0               # per calendar day

    # ── 3rd order ─────────────────────────────────────────────────────────────
    speed  = -gamma / F * (d1 / σ√T + 1)
    zomma  = gamma * (d1 * d2 - 1) / σ
    color  = -df * nd1 / (2 * F * T * σ√T) *
             (2*r*T + 1 + d1*(2*r*T - d2*σ√T) / σ√T)
    color  = color / 365.0
    ultima = -vega / σ^2 * (d1*d2*(1 - d1*d2) + d1^2 + d2^2)

    return Black76Greeks(
        price, delta, gamma, theta, vega, rho,
        vanna, vomma, charm, veta,
        speed, zomma, color, ultima
    )
end

# ── Implied vol solver (Newton-Raphson) ───────────────────────────────────────

function implied_vol_black76(
    market_price::Float64,
    F::Float64,
    K::Float64,
    T::Float64,
    r::Float64,
    is_call::Bool;
    tol::Float64 = 1e-8,
    max_iter::Int = 100
)::Float64
    T ≤ 0 && return NaN
    intrinsic = is_call ? max(F - K, 0.0) : max(K - F, 0.0)
    (market_price ≤ intrinsic * exp(-r * T) + 1e-10) && return NaN

    # Initial guess: simple approximation
    σ = sqrt(2π / T) * market_price / (F * exp(-r * T))
    σ = clamp(σ, 0.001, 10.0)

    for _ in 1:max_iter
        g = black76_greeks(F, K, T, r, σ, is_call)
        diff = g.price - market_price
        abs(diff) < tol && return σ
        g.vega < 1e-12 && return NaN
        σ -= diff / g.vega
        σ = clamp(σ, 1e-6, 10.0)
    end
    return σ
end

# ── Greeks from ThetaData for equity options ──────────────────────────────────
# ThetaData provides Greeks directly; this converts their raw fields
# and adds the 3rd-order greeks they don't provide (speed, color, ultima)

struct EquityGreeks
    source::Symbol    # :thetadata or :black76
    delta::Float64
    gamma::Float64
    theta::Float64    # per day
    vega::Float64
    rho::Float64
    implied_vol::Float64
    underlying_price::Float64
    # 2nd order (from ThetaData or computed)
    vanna::Float64
    charm::Float64
    vomma::Float64
    veta::Float64
    # 3rd order (always computed, ThetaData doesn't provide these)
    speed::Float64
    zomma::Float64
    color::Float64
    ultima::Float64
end

# Enrich ThetaData's output with 3rd-order Greeks via Black-Scholes
function enrich_greeks(
    tq::QuoteGreeks,
    K::Float64,
    T::Float64,
    r::Float64,
    is_call::Bool
)::EquityGreeks
    F = tq.underlying_price
    σ = tq.implied_vol
    bg = black76_greeks(F, K, T, r, σ, is_call)  # for 3rd order
    EquityGreeks(
        :thetadata,
        # 1st order from ThetaData (more accurate, uses actual tick)
        (tq.bid + tq.ask) / 2 |> _ -> black76_greeks(F, K, T, r, σ, is_call).delta,
        tq.gamma, tq.charm, tq.vomma,  # ThetaData 2nd order
        0.0,                            # rho not in QuoteGreeks
        tq.implied_vol, tq.underlying_price,
        tq.vanna, tq.charm, tq.vomma, tq.veta,
        bg.speed, bg.zomma, bg.color, bg.ultima  # 3rd order from Black-Scholes
    )
end
