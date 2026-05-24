using Statistics, LinearAlgebra, Dates

# ── Zeta Field State ──────────────────────────────────────────────────────────
# The complete geometric state of the market at a point in time.
# This is the embedding vector that all agents consume as their primary input.
# It encodes: vol regime, smile geometry, order flow, momentum, roughness.

struct ZetaState
    timestamp::DateTime
    symbol::String

    # ── Vol layer ─────────────────────────────────────────────────────────────
    atm_iv::Float64              # current ATM implied vol
    hv_5d::Float64               # realized vol 5-day
    hv_21d::Float64              # realized vol 21-day
    hv_63d::Float64              # realized vol 63-day
    garch_vol::Float64           # GARCH conditional vol
    vrp::Float64                 # variance risk premium (IV² - HV²_21d)
    vrp_zscore::Float64          # VRP standardized vs trailing distribution
    iv_percentile::Float64       # IV rank vs past year (0-1)

    # ── Smile geometry (market intelligence layer) ────────────────────────────
    skew_25d::Float64            # 25-delta risk reversal (put - call vol diff)
    butterfly_25d::Float64       # 25-delta butterfly (wing pricing vs ATM)
    atm_skew::Float64            # local slope of smile at ATM
    atm_convexity::Float64       # local curvature of smile at ATM
    term_slope::Float64          # (30d vol - 90d vol): inverted = near-term stress
    implied_skewness::Float64    # distributional asymmetry from smile
    implied_kurtosis::Float64    # excess kurtosis from smile

    # ── Greeks layer (moving targets) ─────────────────────────────────────────
    portfolio_delta::Float64     # net delta of current positions
    portfolio_gamma::Float64     # net gamma
    portfolio_theta::Float64     # net theta (daily)
    portfolio_vega::Float64      # net vega
    portfolio_vanna::Float64     # net vanna (vol-spot correlation risk)
    portfolio_charm::Float64     # net charm (delta bleed)

    # ── Regime layer ──────────────────────────────────────────────────────────
    regime_probs::Vector{Float64}  # P(regime_k) for K regimes
    regime_entropy::Float64        # uncertainty of regime (0=certain, 1=uniform)
    hurst::Float64                 # roughness of recent vol path

    # ── Order flow layer (from Databento MBO, CME only) ───────────────────────
    ofi::Float64                 # order flow imbalance [-1, 1]
    cancel_ratio::Float64        # HFT aggression proxy
    bid_ask_vol::Float64         # vol of bid-ask spread (microstructure noise)

    # ── Momentum layer ────────────────────────────────────────────────────────
    price_momentum_5d::Float64   # 5-day log return
    price_momentum_21d::Float64  # 21-day log return
    vol_momentum::Float64        # (current garch_vol - garch_longrun) / garch_longrun

    # ── Field geometry (derived) ──────────────────────────────────────────────
    curvature::Float64           # rate of change of the field (|∇ζ|)
    tension::Float64             # vol of vol / ATM vol (field stress measure)
end

# ── Constructor from component modules ────────────────────────────────────────

function build_zeta_state(;
    timestamp::DateTime,
    symbol::String,
    vol_summary::VolRegimeSummary,
    smile::SmileMetrics,
    term::TermStructure,
    regime::RegimeState,
    ofi_metrics::Union{OrderFlowMetrics, Nothing} = nothing,
    prices::Vector{Float64} = Float64[],
    portfolio_greeks::NamedTuple = (
        delta=0.0, gamma=0.0, theta=0.0, vega=0.0, vanna=0.0, charm=0.0
    ),
    iv_history::Vector{Float64} = Float64[],
)::ZetaState

    # IV percentile (IV rank)
    iv_pct = if length(iv_history) ≥ 2
        count(x -> x < vol_summary.hv_21d, iv_history) / length(iv_history)
    else
        0.5
    end

    # Term slope: short vol minus long vol
    term_slope = if length(term.expiries) ≥ 2
        t_short = term.atm_vols[1]
        t_long  = term.atm_vols[end]
        t_short - t_long   # positive = inverted (stress), negative = normal
    else
        0.0
    end

    # Price momentum
    mom_5d = mom_21d = 0.0
    if length(prices) ≥ 22
        mom_5d  = log(prices[end] / prices[end-5])
        mom_21d = log(prices[end] / prices[end-21])
    end

    # Vol momentum: how far current GARCH is from its long-run mean
    vol_mom = vol_summary.garch_longrun > 0 ?
              (vol_summary.garch_current - vol_summary.garch_longrun) / vol_summary.garch_longrun : 0.0

    # Order flow metrics
    ofi_val    = isnothing(ofi_metrics) ? 0.0 : ofi_metrics.ofi
    cancel_r   = isnothing(ofi_metrics) ? 0.0 : ofi_metrics.cancel_ratio
    bav        = 0.0  # bid-ask vol: computed separately from tick data

    # Field curvature: L2 norm of key gradient components
    # High curvature = rapid change in field geometry = transition risk
    curvature = √(
        vol_summary.vrp_zscore^2 +
        smile.risk_reversal_25^2 * 4 +     # skew weighted higher
        smile.butterfly_25^2 * 4 +
        regime.transition_risk^2
    )

    # Tension: vol-of-vol relative to ATM vol
    # High tension = uncertain vol regime = field is "stretched"
    tension = vol_summary.hv_5d > 0 ?
              abs(vol_summary.garch_current - vol_summary.hv_21d) / vol_summary.hv_21d : 0.0

    ZetaState(
        timestamp, symbol,
        smile.atm_vol, vol_summary.hv_5d, vol_summary.hv_21d, vol_summary.hv_63d,
        vol_summary.garch_current, vol_summary.vrp, vol_summary.vrp_zscore, iv_pct,
        smile.risk_reversal_25, smile.butterfly_25, smile.atm_skew, smile.atm_convexity,
        term_slope, smile.implied_skewness, smile.implied_kurtosis,
        portfolio_greeks.delta, portfolio_greeks.gamma, portfolio_greeks.theta,
        portfolio_greeks.vega, portfolio_greeks.vanna, portfolio_greeks.charm,
        regime.probabilities, regime.transition_risk, vol_summary.hurst,
        ofi_val, cancel_r, bav,
        mom_5d, mom_21d, vol_mom,
        curvature, tension
    )
end

# ── Field trajectory (time series of ZetaState) ───────────────────────────────

# Encode ZetaState as a flat Float64 vector for numerical processing
function to_vector(z::ZetaState)::Vector{Float64}
    vcat(
        z.atm_iv, z.hv_5d, z.hv_21d, z.hv_63d, z.garch_vol,
        z.vrp, z.vrp_zscore, z.iv_percentile,
        z.skew_25d, z.butterfly_25d, z.atm_skew, z.atm_convexity,
        z.term_slope, z.implied_skewness, z.implied_kurtosis,
        z.portfolio_delta, z.portfolio_gamma, z.portfolio_theta,
        z.portfolio_vega, z.portfolio_vanna, z.portfolio_charm,
        z.regime_probs, z.regime_entropy, z.hurst,
        z.ofi, z.cancel_ratio,
        z.price_momentum_5d, z.price_momentum_21d, z.vol_momentum,
        z.curvature, z.tension
    )
end

struct ZetaTrajectory
    states::Vector{ZetaState}
    vectors::Matrix{Float64}  # n × d, each row is a ZetaState vector
end

function ZetaTrajectory(states::Vector{ZetaState})
    isempty(states) && return ZetaTrajectory(states, zeros(0, 0))
    vecs = hcat([to_vector(s) for s in states]...)'  # n × d
    ZetaTrajectory(states, vecs)
end

# Rate of change of the field — velocity in state space
function field_trajectory(traj::ZetaTrajectory)::Matrix{Float64}
    n, d = size(traj.vectors)
    n ≤ 1 && return zeros(0, d)
    diff(traj.vectors, dims=1)   # (n-1) × d
end

# Curvature of the trajectory — second derivative
# High curvature signals regime transition approaching
function field_curvature(traj::ZetaTrajectory)::Vector{Float64}
    vel = field_trajectory(traj)
    size(vel, 1) ≤ 1 && return Float64[]
    acc = diff(vel, dims=1)                            # (n-2) × d
    [norm(acc[i, :]) for i in 1:size(acc, 1)]
end

# ── Zeta Field interpretation for agents ──────────────────────────────────────
# Converts the geometric state into a structured summary string
# This is the primary input to LLM agents

function zeta_context_string(z::ZetaState)::String
    regime_idx = argmax(z.regime_probs)
    regime_pct = round(z.regime_probs[regime_idx] * 100, digits=1)
    regime_names = ["Low-Vol", "Normal", "Stress", "Crisis"]
    regime_label = regime_idx ≤ length(regime_names) ?
                   regime_names[regime_idx] : "Regime-$regime_idx"

    vrp_dir = z.vrp > 0 ? "premium (sell vol edge)" : "discount (buy vol edge)"
    term_dir = z.term_slope > 0 ? "inverted (near-term stress)" : "normal (calm near-term)"
    skew_dir = z.skew_25d < 0 ? "put-heavy (downside fear)" : "call-heavy (upside chase)"

    """
    === ZETA FIELD STATE [$(z.symbol)] @ $(z.timestamp) ===

    VOL REGIME:
      ATM IV: $(round(z.atm_iv*100, digits=1))%  |  HV-5d: $(round(z.hv_5d*100, digits=1))%  |  HV-21d: $(round(z.hv_21d*100, digits=1))%  |  HV-63d: $(round(z.hv_63d*100, digits=1))%
      GARCH conditional: $(round(z.garch_vol*100, digits=1))%
      VRP: $(round(z.vrp*10000, digits=1))bps² → $(vrp_dir)  |  VRP z-score: $(round(z.vrp_zscore, digits=2))σ
      IV Percentile: $(round(z.iv_percentile*100, digits=0))th

    SMILE GEOMETRY:
      25d Risk Reversal: $(round(z.skew_25d*100, digits=2))% → $(skew_dir)
      25d Butterfly: $(round(z.butterfly_25d*100, digits=2))% (wing premium)
      ATM Skew: $(round(z.atm_skew, digits=3))  |  ATM Convexity: $(round(z.atm_convexity, digits=3))
      Term Structure: $(term_dir) [slope: $(round(z.term_slope*100, digits=2))%]
      Impl. Skewness: $(round(z.implied_skewness, digits=3))  |  Impl. Kurtosis: $(round(z.implied_kurtosis, digits=3))

    REGIME:
      Most likely: $(regime_label) ($(regime_pct)%)
      Regime probs: $(join(round.(z.regime_probs .* 100, digits=1), "% / "))%
      Transition risk: $(round(z.regime_entropy*100, digits=1))%  |  Hurst: $(round(z.hurst, digits=3))

    PORTFOLIO GREEKS:
      Δ: $(round(z.portfolio_delta, digits=4))  |  Γ: $(round(z.portfolio_gamma, digits=6))
      Θ: $(round(z.portfolio_theta, digits=2))/day  |  V: $(round(z.portfolio_vega, digits=2))
      Vanna: $(round(z.portfolio_vanna, digits=4))  |  Charm: $(round(z.portfolio_charm, digits=6))/day

    ORDER FLOW (CME):
      OFI: $(round(z.ofi, digits=3))  |  Cancel ratio: $(round(z.cancel_ratio, digits=2))

    FIELD GEOMETRY:
      Curvature: $(round(z.curvature, digits=4))  |  Tension: $(round(z.tension, digits=4))
      Momentum 5d: $(round(z.price_momentum_5d*100, digits=2))%  |  21d: $(round(z.price_momentum_21d*100, digits=2))%
      Vol momentum: $(round(z.vol_momentum*100, digits=1))% vs long-run
    """
end
