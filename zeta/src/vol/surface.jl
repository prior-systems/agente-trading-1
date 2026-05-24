using Statistics, LinearAlgebra, Optim

# ── SVI (Stochastic Volatility Inspired) parametrization ─────────────────────
# Gatheral (2004): w(k) = a + b*(ρ*(k-m) + √((k-m)²+σ²))
# where:
#   k = log-moneyness = ln(K/F)
#   w = total implied variance = σ²_imp * T
#   a  = overall variance level
#   b  = wings slope (≥0)
#   ρ  = correlation / skew (-1,1)
#   m  = ATM shift
#   σ  = curvature (>0)

struct SVIParams
    a::Float64
    b::Float64
    ρ::Float64
    m::Float64
    σ::Float64
end

function svi_variance(p::SVIParams, k::Float64)::Float64
    p.a + p.b * (p.ρ * (k - p.m) + √((k - p.m)^2 + p.σ^2))
end

svi_vol(p::SVIParams, k::Float64, T::Float64)::Float64 =
    √(svi_variance(p, k) / T)

# ── Fitting ───────────────────────────────────────────────────────────────────

struct SmileSlice
    T::Float64              # time to expiry (years)
    strikes::Vector{Float64}
    implied_vols::Vector{Float64}
    forward::Float64
    weights::Vector{Float64}  # typically 1/bid-ask spread
end

function fit_svi(slice::SmileSlice; method=NelderMead())::SVIParams
    ks = log.(slice.strikes ./ slice.forward)
    ws = (slice.implied_vols .^ 2) .* slice.T   # total implied variance

    # Objective: weighted sum of squared errors in total variance space
    function obj(x)
        p = SVIParams(x[1], exp(x[2]), tanh(x[3]), x[4], exp(x[5]))
        ŵ = [svi_variance(p, k) for k in ks]
        # Penalize negative variance (arbitrage)
        any(ŵ .≤ 0) && return 1e10
        # Butterfly arbitrage check: w'' - (w'/2)²*(1 - k*w'/2w)² ≥ 0
        sum(slice.weights .* (ws .- ŵ).^2)
    end

    # Initial guess: ATM vol, small wings, slight put skew
    σ_atm = slice.implied_vols[argmin(abs.(ks))]
    x0 = [σ_atm^2 * slice.T, log(0.1), atanh(-0.3), 0.0, log(0.2)]

    res = optimize(obj, x0, method, Optim.Options(iterations=2000, g_tol=1e-8))
    x = Optim.minimizer(res)
    return SVIParams(x[1], exp(x[2]), tanh(x[3]), x[4], exp(x[5]))
end

# ── Vol surface: collection of SVI slices across expirations ─────────────────

struct VolSurface
    expiries::Vector{Float64}   # time to expiry in years, sorted
    params::Vector{SVIParams}   # one SVI per expiry
    forward_curve::Vector{Float64}
    fit_date::Date
end

function VolSurface(slices::Vector{SmileSlice}, date::Date)::VolSurface
    sorted = sort(slices, by=s -> s.T)
    params = [fit_svi(s) for s in sorted]
    VolSurface(
        [s.T for s in sorted],
        params,
        [s.forward for s in sorted],
        date
    )
end

# Interpolate vol for arbitrary (T, K) — linear interpolation in SVI parameter space
function surface_vol(surf::VolSurface, T::Float64, K::Float64)::Float64
    n = length(surf.expiries)
    n == 0 && return NaN

    # Find bracketing expiries
    idx = searchsortedfirst(surf.expiries, T)
    if idx == 1
        p = surf.params[1]
        F = surf.forward_curve[1]
        return svi_vol(p, log(K/F), surf.expiries[1])
    elseif idx > n
        p = surf.params[n]
        F = surf.forward_curve[n]
        return svi_vol(p, log(K/F), surf.expiries[n])
    else
        # Linear interpolation in total variance space
        T1, T2 = surf.expiries[idx-1], surf.expiries[idx]
        p1, p2 = surf.params[idx-1], surf.params[idx]
        F1, F2 = surf.forward_curve[idx-1], surf.forward_curve[idx]
        k1, k2 = log(K/F1), log(K/F2)
        w1 = svi_variance(p1, k1) * T1   # total variance
        w2 = svi_variance(p2, k2) * T2
        α  = (T - T1) / (T2 - T1)
        w  = (1 - α) * w1 + α * w2
        w ≤ 0 && return NaN
        return √(w / T)
    end
end

# ── Smile metrics — the smile as market intelligence ─────────────────────────

struct SmileMetrics
    T::Float64
    forward::Float64
    # ATM
    atm_vol::Float64             # vol at F = K
    atm_skew::Float64            # dσ/dK at ATM (normalized by F)
    atm_convexity::Float64       # d²σ/dK² at ATM — wings pricing
    # 25-delta metrics (standard quoting convention)
    vol_25c::Float64             # 25-delta call vol
    vol_25p::Float64             # 25-delta put vol
    risk_reversal_25::Float64    # RR25 = vol_25c - vol_25p (directional sentiment)
    butterfly_25::Float64        # BF25 = (vol_25c + vol_25p)/2 - atm_vol (tail pricing)
    # Distributional shape (read from the smile geometry)
    implied_skewness::Float64    # asymmetry of implied distribution
    implied_kurtosis::Float64    # fat-tail excess of implied distribution
    # VIX-like ATM term vol (if T ≈ 30 days)
    variance_swap_vol::Float64   # model-free implied vol (log contract approximation)
end

function atm_vol(surf::VolSurface, T::Float64)::Float64
    idx = argmin(abs.(surf.expiries .- T))
    p = surf.params[idx]
    F = surf.forward_curve[idx]
    svi_vol(p, 0.0, surf.expiries[idx])
end

# Compute 25-delta strike for a given SVI slice
function delta25_strike(p::SVIParams, T::Float64, F::Float64, is_call::Bool;
                        r::Float64=0.05)::Float64
    target_delta = is_call ? 0.25 : -0.25
    # Newton-Raphson on delta
    K = F * exp(is_call ? 0.5 : -0.5)  # initial guess
    for _ in 1:50
        σ = svi_vol(p, log(K/F), T)
        σ ≤ 0 && break
        g = black76_greeks(F, K, T, r, σ, is_call)
        err = g.delta - target_delta
        abs(err) < 1e-8 && break
        # dDelta/dK ≈ gamma * F/K (chain rule)
        K -= err / (g.gamma * F / K + 1e-12)
        K = max(K, 1e-4)
    end
    return K
end

function smile_metrics(slice::SmileSlice, p::SVIParams; r::Float64=0.05)::SmileMetrics
    T = slice.T
    F = slice.forward

    σ_atm = svi_vol(p, 0.0, T)

    # ATM local derivatives (finite difference)
    dk = 0.001
    σ_u = svi_vol(p, dk, T)
    σ_d = svi_vol(p, -dk, T)
    skew      = (σ_u - σ_d) / (2 * dk)          # dσ/dk in log-moneyness
    convexity = (σ_u - 2σ_atm + σ_d) / dk^2

    # 25-delta strikes and vols
    K_25c = delta25_strike(p, T, F, true;  r=r)
    K_25p = delta25_strike(p, T, F, false; r=r)
    σ_25c = svi_vol(p, log(K_25c/F), T)
    σ_25p = svi_vol(p, log(K_25p/F), T)
    rr25  = σ_25c - σ_25p
    bf25  = (σ_25c + σ_25p) / 2 - σ_atm

    # Implied moments (from Bakshi, Kapadia, Madan 2003 approximation)
    # Using smile curvature and skew as proxies
    impl_skew = -rr25 / σ_atm          # negative = put skew dominant (fear)
    impl_kurt =  bf25 / σ_atm * 4      # positive = fat tails

    # Variance swap vol (model-free, log-strip approximation)
    # ∫ σ²(k)/k² dk ≈ simple trapezoidal over the fitted smile
    ks  = range(-1.0, 1.0, length=200)
    ws  = [svi_variance(p, k) / T for k in ks]  # σ²(k)
    dk_ = step(ks)
    vs_vol = √(sum(ws .* exp.(-abs.(collect(ks)))) * dk_)  # weighted average

    SmileMetrics(T, F, σ_atm, skew, convexity,
                 σ_25c, σ_25p, rr25, bf25,
                 impl_skew, impl_kurt, vs_vol)
end

# ── Term structure ────────────────────────────────────────────────────────────

struct TermStructure
    expiries::Vector{Float64}
    atm_vols::Vector{Float64}
    forward_vols::Vector{Float64}  # σ_fwd(T1, T2)² = (σ_T2²*T2 - σ_T1²*T1)/(T2-T1)
    shape::Symbol  # :normal (contango), :inverted (backwardation), :humped
end

function term_structure(surf::VolSurface)::TermStructure
    Ts   = surf.expiries
    vols = [atm_vol(surf, T) for T in Ts]
    fvols = zeros(length(Ts) - 1)
    for i in 1:length(Ts)-1
        w2 = vols[i+1]^2 * Ts[i+1]
        w1 = vols[i]^2   * Ts[i]
        dT = Ts[i+1] - Ts[i]
        fvols[i] = dT > 0 ? √(max(w2 - w1, 0) / dT) : vols[i]
    end
    shape = if all(diff(vols) .> 0)
        :normal      # vol increases with tenor → calm near term
    elseif all(diff(vols) .< 0)
        :inverted    # vol decreases with tenor → near-term stress
    else
        :humped      # peak in the middle → specific event priced in
    end
    TermStructure(Ts, vols, fvols, shape)
end
