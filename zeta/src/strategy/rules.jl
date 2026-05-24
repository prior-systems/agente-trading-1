using Dates

# ── Strategy types ────────────────────────────────────────────────────────────

@enum StrategyType begin
    # Sell vol — positive VRP
    IronCondor          # sell OTM call + put spreads — defined risk, best for stable field
    Strangle            # sell OTM call + put — undefined risk, higher premium
    IronButterfly       # sell ATM straddle + buy wings — high premium, tighter range
    # Buy vol — negative VRP / high gamma expectation
    LongStraddle        # buy ATM call + put — pure vol long, delta-neutral
    LongStrangle        # buy OTM call + put — cheaper, needs big move
    Backspread          # sell 1 ATM + buy 2 OTM — long gamma, capped downside
    # Skew plays
    RiskReversal        # buy OTM call + sell OTM put (or reverse) — skew mean reversion
    # Futures specific
    FuturesLong         # long futures contract — directional + delta hedge tool
    FuturesShort        # short futures contract
    FuturesCalendar     # long far + short near — roll / basis play
    # Hedge / reduce
    DeltaHedge          # re-balance delta only, no new structure
    DoNothing           # field unstable or no edge — hold cash
end

# ── Candidate strategy with target parameters ─────────────────────────────────

struct StrategyCandidateLegs
    # Options legs (for equity options via ThetaData)
    long_call_delta::Float64    # target delta for long call leg (0 = not used)
    short_call_delta::Float64   # target delta for short call leg
    long_put_delta::Float64     # target delta for long put leg (negative for puts)
    short_put_delta::Float64    # target delta for short put leg
    # Expiration targeting
    near_dte::Int               # days to expiry for near leg (0 = not used)
    far_dte::Int                # days to expiry for far leg (0 = not used)
end

struct StrategyCandidate
    type::StrategyType
    legs::StrategyCandidateLegs
    rationale::String           # human-readable reason from the field
    edge_score::Float64         # 0..1, estimated edge strength
    confidence::Float64         # 0..1, rule confidence (1 = mathematically clear)
    # Risk profile
    max_profit::Symbol          # :limited or :unlimited
    max_loss::Symbol            # :limited or :unlimited
    # Greeks profile at entry
    target_delta::Float64       # expected net delta (should be near 0 for neutral)
    target_vega::Float64        # expected net vega (negative=short vol, positive=long vol)
    target_theta::Float64       # expected net theta (positive=time decay in your favor)
end

# ── Rule engine ───────────────────────────────────────────────────────────────

function select_candidates(env::MarketEnvironment, z::ZetaState)::Vector{StrategyCandidate}
    candidates = StrategyCandidate[]

    # ── HARD STOP: unstable field ─────────────────────────────────────────────
    if env.field == Unstable
        return [_do_nothing("Field geometry unstable — curvature=$(round(env.curvature, digits=2)), entropy=$(round(env.regime_entropy, digits=2))")]
    end

    # ── SELL VOL: positive VRP ────────────────────────────────────────────────
    if env.vrp ∈ (StrongSell, MildSell)
        edge = _vrp_edge_score(env.vrp_zscore)

        if env.field == Stable && env.term ∈ (NormalTerm, Humped)

            if env.vol_regime ∈ (ElevatedVol, NormalVol)
                # Iron Condor: best when field is stable and wings are priced rich
                # Sell at ~0.16 delta (1σ), buy protection at ~0.05 delta (2σ)
                bf_premium = z.butterfly_25d > 0.02   # wings are expensive
                push!(candidates, StrategyCandidate(
                    IronCondor,
                    StrategyCandidateLegs(0.05, 0.16, -0.05, -0.16, 30, 0),
                    "VRP=$(round(env.vrp_zscore, digits=2))σ. Stable field. Wings " *
                    (bf_premium ? "expensive (BF25=$(round(z.butterfly_25d*100, digits=2))%) — iron condor captures wing premium" :
                                  "fairly priced — iron condor for defined risk"),
                    edge * (bf_premium ? 1.1 : 0.9),
                    env.field == Stable ? 0.85 : 0.65,
                    :limited, :limited,
                    0.0, -1.0, 1.0
                ))
            end

            if env.vol_regime == ElevatedVol && z.atm_iv > 0.30
                # Strangle: higher IV = more premium, field stable = can tolerate undefined risk
                push!(candidates, StrategyCandidate(
                    Strangle,
                    StrategyCandidateLegs(0.0, 0.20, 0.0, -0.20, 45, 0),
                    "High IV ($(round(z.atm_iv*100, digits=1))%) + stable field. Strangle captures more premium than iron condor.",
                    edge * 1.15,
                    0.70,  # lower confidence — undefined risk needs more conviction
                    :limited, :unlimited,
                    0.0, -1.5, 1.2
                ))
            end

        elseif env.field == Transitioning
            # Transitioning field: tighter strikes, defined risk only
            push!(candidates, StrategyCandidate(
                IronCondor,
                StrategyCandidateLegs(0.05, 0.12, -0.05, -0.12, 21, 0),
                "VRP attractive but field transitioning. Tighter strikes, shorter DTE for faster theta decay.",
                edge * 0.75,
                0.60,
                :limited, :limited,
                0.0, -0.7, 0.8
            ))
        end
    end

    # ── BUY VOL: negative VRP ─────────────────────────────────────────────────
    if env.vrp ∈ (StrongBuy, MildBuy)
        edge = _vrp_edge_score(env.vrp_zscore)   # score treats both tails

        if env.momentum == Flat || abs(z.price_momentum_5d) < 0.02
            # No directional bias → straddle (pure vol long)
            push!(candidates, StrategyCandidate(
                LongStraddle,
                StrategyCandidateLegs(0.0, 0.0, 0.0, 0.0, 30, 0),
                "VRP=$(round(env.vrp_zscore, digits=2))σ (vol underpriced vs realized). Flat momentum → delta-neutral vol long.",
                edge,
                0.75,
                :unlimited, :limited,
                0.0, 1.5, -1.0
            ))
        else
            # Directional bias → strangle skewed or backspread
            call_heavy = z.price_momentum_21d > 0
            push!(candidates, StrategyCandidate(
                Backspread,
                StrategyCandidateLegs(
                    call_heavy ? 0.30 : 0.0,   # buy 2x OTM calls (if bullish)
                    call_heavy ? 0.0  : 0.0,
                    call_heavy ? 0.0  : -0.30,  # buy 2x OTM puts (if bearish)
                    call_heavy ? 0.0  : 0.0,
                    21, 0
                ),
                "Vol underpriced + directional momentum $(round(z.price_momentum_21d*100, digits=1))%. " *
                "Backspread: long gamma with directional tilt.",
                edge * 0.9,
                0.65,
                :unlimited, :limited,
                call_heavy ? 0.3 : -0.3, 1.2, -0.8
            ))
        end
    end

    # ── SKEW PLAYS ────────────────────────────────────────────────────────────
    if env.skew == PutHeavy && abs(z.skew_25d) > 0.06 && env.vrp == Neutral
        # Put skew extreme without VRP signal → skew mean reversion
        # Risk reversal: sell expensive puts, buy cheaper calls
        push!(candidates, StrategyCandidate(
            RiskReversal,
            StrategyCandidateLegs(0.25, 0.0, 0.0, -0.25, 30, 0),
            "25d RR=$(round(z.skew_25d*100, digits=2))%. Put skew extreme without VRP support. " *
            "Risk reversal sells overpriced downside protection.",
            _skew_edge_score(z.skew_25d),
            0.60,
            :unlimited, :unlimited,
            0.4, 0.3, 0.1
        ))
    end

    if env.skew == CallHeavy && z.skew_25d > 0.06 && env.vrp == Neutral
        push!(candidates, StrategyCandidate(
            RiskReversal,
            StrategyCandidateLegs(0.0, 0.25, -0.25, 0.0, 30, 0),
            "25d RR=$(round(z.skew_25d*100, digits=2))%. Call skew extreme. " *
            "Reverse risk reversal: sell calls, buy puts.",
            _skew_edge_score(z.skew_25d),
            0.60,
            :unlimited, :unlimited,
            -0.4, 0.3, 0.1
        ))
    end

    # ── INVERTED TERM STRUCTURE ───────────────────────────────────────────────
    if env.term == Inverted && env.vrp ∈ (Neutral, MildSell)
        # Near-term stress event priced in → calendar spread
        # Sell near (expensive IV), buy far (cheaper IV) — captures term normalization
        push!(candidates, StrategyCandidate(
            FuturesCalendar,
            StrategyCandidateLegs(0.0, 0.0, 0.0, 0.0, 14, 45),
            "Term structure inverted (slope=$(round(z.term_slope*100, digits=2))%). " *
            "Calendar spread: short front month, long back month. Profits from term normalization.",
            0.60,
            0.65,
            :limited, :limited,
            0.0, 0.5, 0.2
        ))
    end

    # ── DELTA HEDGE (portfolio management, not new structure) ─────────────────
    if abs(z.portfolio_delta) > 0.10
        push!(candidates, StrategyCandidate(
            DeltaHedge,
            StrategyCandidateLegs(0.0, 0.0, 0.0, 0.0, 0, 0),
            "Portfolio delta=$(round(z.portfolio_delta, digits=3)) exceeds tolerance. Futures hedge required.",
            1.0,   # always high confidence — this is maintenance, not speculation
            1.0,
            :limited, :limited,
            0.0, 0.0, 0.0
        ))
    end

    # ── NEUTRAL: no clear edge ────────────────────────────────────────────────
    if isempty(candidates)
        push!(candidates, _do_nothing(
            "No significant edge detected. VRP=$(round(env.vrp_zscore, digits=2))σ, " *
            "field=$(env.field), vol=$(env.vol_regime)."
        ))
    end

    # Sort by edge_score × confidence (combined signal quality)
    sort!(candidates, by=c -> c.edge_score * c.confidence, rev=true)
    return candidates
end

# ── Edge score helpers ────────────────────────────────────────────────────────

function _vrp_edge_score(vrp_z::Float64)::Float64
    # Map VRP z-score to 0..1 edge score
    # Saturates at z = ±3
    clamp(abs(vrp_z) / 3.0, 0.0, 1.0)
end

function _skew_edge_score(rr25::Float64)::Float64
    # 25d risk reversal: extreme > 6%, saturates at 10%
    clamp(abs(rr25) / 0.10, 0.0, 1.0)
end

function _do_nothing(reason::String)::StrategyCandidate
    StrategyCandidate(
        DoNothing,
        StrategyCandidateLegs(0, 0, 0, 0, 0, 0),
        reason,
        0.0, 1.0,
        :limited, :limited,
        0.0, 0.0, 0.0
    )
end

# ── Summary for logging ───────────────────────────────────────────────────────

function Base.show(io::IO, c::StrategyCandidate)
    score = round(c.edge_score * c.confidence, digits=3)
    println(io, "  [$(c.type)] score=$(score) | edge=$(round(c.edge_score,digits=2)) | conf=$(round(c.confidence,digits=2))")
    println(io, "  $(c.rationale)")
    println(io, "  Risk: profit=$(c.max_profit) / loss=$(c.max_loss) | Vega=$(c.target_vega > 0 ? "+" : "")$(c.target_vega)")
end
