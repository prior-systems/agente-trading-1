using Statistics

# ── Market environment classification ────────────────────────────────────────
# Reads the ZetaState geometry and produces a structured classification
# that the rule engine uses to select strategies.
# This is the translation layer between the field and human-readable signals.

@enum VolRegime begin
    LowVol      # IV compressed, HV low, VRP may be near zero
    NormalVol   # typical conditions
    ElevatedVol # IV high but not crisis — often best sell-vol entry
    StressVol   # inverted term structure, put skew extreme, crisis proximity
end

@enum VRPSignal begin
    StrongSell  # VRP z-score > 1.5: vol significantly overpriced
    MildSell    # VRP z-score 0.75..1.5
    Neutral     # |VRP z-score| < 0.75
    MildBuy     # VRP z-score -1.5..-0.75
    StrongBuy   # VRP z-score < -1.5: vol significantly underpriced
end

@enum SkewSignal begin
    PutHeavy    # RR25 < -0.04: strong put skew, downside fear dominant
    Balanced    # |RR25| < 0.04
    CallHeavy   # RR25 > 0.04: call skew, upside chase or squeeze
end

@enum TermSignal begin
    NormalTerm    # vol increases with tenor: calm near-term
    Humped        # peak in the middle: specific event priced in
    Inverted      # short-dated vol > long-dated: near-term stress
end

@enum FieldSignal begin
    Stable        # low curvature, high regime certainty
    Transitioning # moderate curvature or uncertain regime
    Unstable      # high curvature and/or high entropy → do not enter
end

@enum MomentumSignal begin
    StrongUp
    MildUp
    Flat
    MildDown
    StrongDown
end

# Full market environment — output of the classifier
struct MarketEnvironment
    vol_regime::VolRegime
    vrp::VRPSignal
    skew::SkewSignal
    term::TermSignal
    field::FieldSignal
    momentum::MomentumSignal

    # Raw values for sizing and fine-tuning
    vrp_zscore::Float64
    iv_percentile::Float64
    regime_entropy::Float64
    curvature::Float64
    atm_iv::Float64
    hurst::Float64

    # Ambiguity: should the LLM review this?
    needs_llm::Bool
    ambiguity_reason::Vector{String}
end

# ── Classifier ────────────────────────────────────────────────────────────────

function classify(z::ZetaState)::MarketEnvironment
    reasons = String[]

    # Vol regime
    vol_regime = if z.iv_percentile < 0.20
        LowVol
    elseif z.iv_percentile < 0.50
        NormalVol
    elseif z.iv_percentile < 0.80
        ElevatedVol
    else
        StressVol
    end

    # VRP signal
    vrp = if z.vrp_zscore > 1.5
        StrongSell
    elseif z.vrp_zscore > 0.75
        MildSell
    elseif z.vrp_zscore > -0.75
        Neutral
    elseif z.vrp_zscore > -1.5
        MildBuy
    else
        StrongBuy
    end

    # Skew signal
    skew = if z.skew_25d < -0.04
        PutHeavy
    elseif z.skew_25d > 0.04
        CallHeavy
    else
        Balanced
    end

    # Term structure
    term = if z.term_slope > 0.03
        Inverted
    elseif z.term_slope < -0.01
        NormalTerm
    else
        Humped
    end

    # Field stability
    field = if z.curvature < 0.5 && z.regime_entropy < 0.4
        Stable
    elseif z.curvature > 1.5 || z.regime_entropy > 0.75
        Unstable
    else
        Transitioning
    end

    # Momentum
    mom = if z.price_momentum_21d > 0.05
        StrongUp
    elseif z.price_momentum_21d > 0.02
        MildUp
    elseif z.price_momentum_21d < -0.05
        StrongDown
    elseif z.price_momentum_21d < -0.02
        MildDown
    else
        Flat
    end

    # ── Ambiguity detection ───────────────────────────────────────────────────

    # Conflicting VRP + field stability
    if (vrp == StrongSell || vrp == StrongBuy) && field == Unstable
        push!(reasons, "Strong vol signal but field is unstable — transition risk conflicts with entry")
    end

    # Extreme skew with neutral VRP
    if (skew == PutHeavy || skew == CallHeavy) && vrp == Neutral
        push!(reasons, "Skew is extreme but VRP is neutral — skew play without vol edge is ambiguous")
    end

    # Inverted term structure with low IV
    if term == Inverted && vol_regime == LowVol
        push!(reasons, "Inverted term + low IV: near-term event not priced in IV rank — calendar effects possible")
    end

    # Regime genuinely uncertain (HMM near-uniform)
    if z.regime_entropy > 0.70
        push!(reasons, "Regime probabilities near-uniform (entropy=$(round(z.regime_entropy, digits=2))) — no dominant regime")
    end

    # Rough vol + sell signal: rough vol surfaces mean realized > model
    if z.hurst < 0.25 && (vrp == StrongSell || vrp == MildSell)
        push!(reasons, "Hurst=$(round(z.hurst, digits=2)) — rough vol regime may erode sell-vol edge faster than expected")
    end

    # Momentum conflicts with skew direction
    if mom == StrongUp && skew == PutHeavy
        push!(reasons, "Strong upward momentum but put skew elevated — divergence between price and vol sentiment")
    end
    if mom == StrongDown && skew == CallHeavy
        push!(reasons, "Strong downward momentum but call skew elevated — unusual configuration")
    end

    needs_llm = !isempty(reasons) || field == Unstable

    MarketEnvironment(
        vol_regime, vrp, skew, term, field, mom,
        z.vrp_zscore, z.iv_percentile, z.regime_entropy,
        z.curvature, z.atm_iv, z.hurst,
        needs_llm, reasons
    )
end

function Base.show(io::IO, env::MarketEnvironment)
    println(io, "MarketEnvironment:")
    println(io, "  Vol:    $(env.vol_regime) | IV pct: $(round(env.iv_percentile*100, digits=0))th")
    println(io, "  VRP:    $(env.vrp) (z=$(round(env.vrp_zscore, digits=2)))")
    println(io, "  Skew:   $(env.skew) | Term: $(env.term)")
    println(io, "  Field:  $(env.field) (curvature=$(round(env.curvature, digits=3)), entropy=$(round(env.regime_entropy, digits=2)))")
    println(io, "  Mom:    $(env.momentum) | Hurst: $(round(env.hurst, digits=3))")
    if env.needs_llm
        println(io, "  ⚠ LLM review: $(join(env.ambiguity_reason, "; "))")
    end
end
