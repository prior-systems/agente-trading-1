using Statistics, LinearAlgebra

# ── Rolling historical volatility ─────────────────────────────────────────────

function log_returns(prices::Vector{Float64})::Vector{Float64}
    [log(prices[i] / prices[i-1]) for i in 2:length(prices)]
end

# Standard rolling HV (Yang-Zhang or close-to-close)
function rolling_hv(
    prices::Vector{Float64},
    window::Int;
    annualization::Float64 = 252.0
)::Vector{Float64}
    rets = log_returns(prices)
    n = length(rets)
    hvs = fill(NaN, n)
    for i in window:n
        hvs[i] = std(rets[i-window+1:i]) * √annualization
    end
    return hvs
end

# Multiple windows simultaneously — useful for zeta field embedding
function rolling_hv_multi(
    prices::Vector{Float64},
    windows::Vector{Int} = [5, 10, 21, 63];
    annualization::Float64 = 252.0
)::Matrix{Float64}
    n = length(prices) - 1
    result = fill(NaN, n, length(windows))
    rets = log_returns(prices)
    for (j, w) in enumerate(windows)
        for i in w:n
            result[i, j] = std(rets[i-w+1:i]) * √annualization
        end
    end
    return result
end

# ── Variance risk premium ─────────────────────────────────────────────────────
# VRP = IV² - HV²_realized  (in variance space, not vol space)
# Positive VRP → market prices vol above realized → selling premium has edge

function variance_risk_premium(
    implied_vols::Vector{Float64},   # IV at each date
    realized_hvs::Vector{Float64};   # realized HV over matched horizon
    lag::Int = 0                     # shift to align forward realized vol
)::Vector{Float64}
    n = min(length(implied_vols), length(realized_hvs))
    vrp = fill(NaN, n)
    for i in (1 + lag):n
        iv = implied_vols[i - lag]
        rv = realized_hvs[i]
        vrp[i] = iv^2 - rv^2   # variance difference (signed)
    end
    return vrp
end

# ── GARCH(1,1) ────────────────────────────────────────────────────────────────
# σ²_t = ω + α*ε²_{t-1} + β*σ²_{t-1}
# Captures volatility clustering (heterocedasticidad estructurada)

struct GARCH11Params
    ω::Float64    # unconditional variance floor
    α::Float64    # ARCH term (shock sensitivity)
    β::Float64    # GARCH term (persistence)
    σ²_uncon::Float64  # long-run variance = ω / (1 - α - β)
    log_likelihood::Float64
end

function garch11_fit(returns::Vector{Float64})::GARCH11Params
    n = length(returns)
    σ²_init = var(returns)

    # Negative log-likelihood for GARCH(1,1)
    function neg_ll(params)
        ω_raw, α_raw, β_raw = params
        # Parameter constraints via softplus / sigmoid transforms
        ω = exp(ω_raw) * 1e-6 + 1e-8
        α = 1 / (1 + exp(-α_raw)) * 0.3
        β = 1 / (1 + exp(-β_raw)) * 0.97
        α + β ≥ 1.0 && return 1e10

        σ² = σ²_init
        ll  = 0.0
        for r in returns
            σ² = ω + α * r^2 + β * σ²
            σ² ≤ 0 && return 1e10
            ll += log(σ²) + r^2 / σ²
        end
        return ll / 2
    end

    using Optim
    res = optimize(neg_ll, [log(σ²_init) - 13, 0.0, 1.5],
                   NelderMead(), Optim.Options(iterations=5000))
    p = Optim.minimizer(res)
    ω = exp(p[1]) * 1e-6 + 1e-8
    α = 1 / (1 + exp(-p[2])) * 0.3
    β = 1 / (1 + exp(-p[3])) * 0.97

    σ²_uncon = ω / max(1 - α - β, 1e-8)
    GARCH11Params(ω, α, β, σ²_uncon, -Optim.minimum(res))
end

# Filter conditional variances given fitted GARCH(1,1) parameters
function garch11_filter(params::GARCH11Params, returns::Vector{Float64})::Vector{Float64}
    σ²s = fill(NaN, length(returns))
    σ²  = params.σ²_uncon  # initialize at unconditional
    for (i, r) in enumerate(returns)
        σ² = params.ω + params.α * r^2 + params.β * σ²
        σ²s[i] = σ²
    end
    return σ²s
end

# GARCH-implied annualized vol path
function garch11_vol_path(params::GARCH11Params, returns::Vector{Float64};
                           annualization::Float64=252.0)::Vector{Float64}
    √.(garch11_filter(params, returns) .* annualization)
end

# ── Hurst exponent (R/S analysis) ────────────────────────────────────────────
# H < 0.5 → anti-persistent / mean-reverting
# H = 0.5 → random walk
# H > 0.5 → persistent / trending
# Realized vol empirically has H ≈ 0.1 (rough vol)

function hurst_exponent(series::Vector{Float64}; min_window::Int=10)::Float64
    n = length(series)
    max_power = floor(Int, log2(n)) - 1
    windows = [2^p for p in 3:max_power]
    filter!(w -> w ≥ min_window && w ≤ n ÷ 2, windows)
    isempty(windows) && return 0.5

    log_n = Float64[]
    log_rs = Float64[]

    for w in windows
        rs_vals = Float64[]
        for start in 1:w:(n - w + 1)
            chunk = series[start:start+w-1]
            μ = mean(chunk)
            deviations = cumsum(chunk .- μ)
            R = maximum(deviations) - minimum(deviations)
            S = std(chunk)
            S > 0 && push!(rs_vals, R / S)
        end
        isempty(rs_vals) && continue
        push!(log_n, log(w))
        push!(log_rs, log(mean(rs_vals)))
    end

    length(log_n) < 2 && return 0.5
    # OLS slope = Hurst exponent
    x̄ = mean(log_n)
    ȳ = mean(log_rs)
    H = sum((log_n .- x̄) .* (log_rs .- ȳ)) / sum((log_n .- x̄).^2)
    return clamp(H, 0.0, 1.0)
end

# ── Vol regime summary ────────────────────────────────────────────────────────

struct VolRegimeSummary
    hv_5d::Float64
    hv_21d::Float64
    hv_63d::Float64
    garch_current::Float64      # GARCH conditional vol right now
    garch_longrun::Float64      # GARCH unconditional (long-run) vol
    garch_persistence::Float64  # α + β: how quickly shocks decay
    hurst::Float64              # roughness of the vol path
    vrp::Float64                # IV² - HV²_realized (latest)
    vrp_zscore::Float64         # VRP vs its own rolling distribution
end

function vol_regime_summary(
    prices::Vector{Float64},
    atm_iv::Float64;
    vrp_window::Int = 63
)::VolRegimeSummary
    rets = log_returns(prices)
    n = length(rets)

    hvs = rolling_hv_multi(prices, [5, 21, 63])
    hv5  = isnan(hvs[n, 1]) ? std(rets[max(1,n-4):n]) * √252 : hvs[n, 1]
    hv21 = isnan(hvs[n, 2]) ? std(rets[max(1,n-20):n]) * √252 : hvs[n, 2]
    hv63 = isnan(hvs[n, 3]) ? std(rets[max(1,n-62):n]) * √252 : hvs[n, 3]

    gp  = garch11_fit(rets)
    vols = garch11_vol_path(gp, rets)
    g_now = vols[end]

    H = n ≥ 30 ? hurst_exponent(log.(prices[max(1,n-252+1):end])) : 0.5

    vrp_now = atm_iv^2 - hv21^2
    vrp_hist = [atm_iv^2 - hvs[i,2]^2 for i in 1:n if !isnan(hvs[i,2])]
    vrp_z = isempty(vrp_hist) ? 0.0 :
            (vrp_now - mean(vrp_hist)) / max(std(vrp_hist), 1e-8)

    VolRegimeSummary(hv5, hv21, hv63, g_now, √(gp.σ²_uncon * 252),
                     gp.α + gp.β, H, vrp_now, vrp_z)
end
