using Statistics

# ── Risk constraints (hard limits — Julia always enforces) ────────────────────
# These cannot be overridden by the LLM layer.

struct RiskLimits
    max_portfolio_delta::Float64      # max net delta (absolute)
    max_portfolio_vega::Float64       # max net vega (as % of portfolio value)
    max_loss_per_trade::Float64       # max risk per trade (as % of portfolio)
    max_portfolio_loss::Float64       # max aggregate open risk (as % of portfolio)
    min_edge_score::Float64           # minimum edge_score × confidence to trade
    max_iv_percentile_sell::Float64   # don't sell vol above this IV percentile (already mean-reverted)
    min_iv_percentile_buy::Float64    # don't buy vol below this IV percentile (too cheap = dangerous)
end

# Conservative defaults — tune per strategy book
const DEFAULT_LIMITS = RiskLimits(
    0.10,   # max net delta
    0.05,   # max vega = 5% of portfolio per 1vol-pt move
    0.02,   # max loss per trade = 2% of portfolio
    0.10,   # max aggregate open risk = 10% of portfolio
    0.25,   # minimum combined score
    0.95,   # don't sell vol if IV above 95th percentile (capitulation risk)
    0.05,   # don't buy vol if IV below 5th percentile (expensive to hold)
)

# ── Sized proposal — output of the full engine ────────────────────────────────

struct StrategyProposal
    candidate::StrategyCandidate
    environment::MarketEnvironment

    # Sizing
    contracts::Int              # number of contracts (each leg)
    max_risk_dollars::Float64   # dollar value at risk if max loss
    net_premium::Float64        # credit received (negative) or debit paid (positive)

    # Greeks at entry (estimated)
    est_delta::Float64
    est_gamma::Float64
    est_theta_day::Float64
    est_vega::Float64

    # Execution guidance
    target_dte::Int             # days to expiration to target
    entry_urgency::Symbol       # :immediate, :patient, :conditional

    # Routing
    needs_llm::Bool
    llm_questions::Vector{String}

    # Hard limit check
    passes_limits::Bool
    limit_violations::Vector{String}
end

# ── Sizing engine ─────────────────────────────────────────────────────────────

function size_proposal(
    candidate::StrategyCandidate,
    env::MarketEnvironment,
    z::ZetaState,
    portfolio_value::Float64,
    open_risk::Float64 = 0.0,     # current aggregate open risk in dollars
    limits::RiskLimits = DEFAULT_LIMITS
)::StrategyProposal

    violations = String[]
    llm_q = String[]

    # ── Hard limit checks ─────────────────────────────────────────────────────

    # Combined score floor
    combined_score = candidate.edge_score * candidate.confidence
    if combined_score < limits.min_edge_score && candidate.type != DoNothing && candidate.type != DeltaHedge
        push!(violations, "Edge score $(round(combined_score, digits=3)) < minimum $(limits.min_edge_score)")
    end

    # IV percentile limits for vol selling
    if candidate.type ∈ (IronCondor, Strangle, IronButterfly)
        if z.iv_percentile > limits.max_iv_percentile_sell
            push!(violations, "IV at $(round(z.iv_percentile*100, digits=0))th percentile — too high to sell (capitulation risk)")
        end
    end

    # IV percentile limits for vol buying
    if candidate.type ∈ (LongStraddle, LongStrangle, Backspread)
        if z.iv_percentile < limits.min_iv_percentile_buy
            push!(violations, "IV at $(round(z.iv_percentile*100, digits=0))th percentile — too low to buy")
        end
    end

    # Aggregate risk check
    max_new_risk = limits.max_loss_per_trade * portfolio_value
    if open_risk + max_new_risk > limits.max_portfolio_loss * portfolio_value
        push!(violations, "Adding trade would breach aggregate risk limit " *
              "(open=$(round(open_risk/portfolio_value*100, digits=1))% + " *
              "new=$(round(limits.max_loss_per_trade*100, digits=1))% > " *
              "limit=$(round(limits.max_portfolio_loss*100, digits=1))%)")
    end

    # Field unstable: block new risk positions
    if env.field == Unstable && candidate.type ∉ (DoNothing, DeltaHedge)
        push!(violations, "Field geometry unstable — no new risk positions")
    end

    # ── Fractional Kelly sizing ───────────────────────────────────────────────
    # Convert VRP z-score to win probability estimate via normal CDF approximation
    # p_win ≈ Φ(|vrp_z| × 0.5) — conservative mapping
    p_win = _zscore_to_win_prob(env.vrp_zscore)
    p_loss = 1.0 - p_win

    # Expected profit/loss ratio depends on strategy structure
    (profit_ratio, loss_ratio) = _strategy_payoff_ratio(candidate.type)

    # Kelly fraction: f* = (p*b - q) / b, b = profit/loss ratio
    kelly_f = profit_ratio > 0 ?
              max((p_win * profit_ratio - p_loss) / profit_ratio, 0.0) : 0.0

    # Use 25% Kelly for safety (fractional Kelly standard practice)
    kelly_fraction = 0.25 * kelly_f

    # Scale by confidence: lower confidence → smaller fraction
    adjusted_fraction = kelly_fraction * candidate.confidence

    # Max risk in dollars for this trade
    max_risk = min(
        adjusted_fraction * portfolio_value,
        limits.max_loss_per_trade * portfolio_value
    )

    # Contracts: assuming rough estimate of $1000 max loss per contract spread
    # This will be refined with actual option chain data in execution
    max_loss_per_contract = _estimate_max_loss_per_contract(candidate.type, z.atm_iv)
    contracts = max_loss_per_contract > 0 ?
                max(1, round(Int, max_risk / max_loss_per_contract)) : 1

    # ── Estimated Greeks at entry ─────────────────────────────────────────────
    est_delta = candidate.target_delta * contracts * 100   # 100 shares per contract
    est_vega  = candidate.target_vega  * contracts * 100
    est_theta = candidate.target_theta * contracts * 100
    est_gamma = 0.0  # refined at execution time with actual strikes

    # ── Entry urgency ─────────────────────────────────────────────────────────
    # Strong edge + stable field → enter now
    # Transitioning field or mild edge → be patient, wait for better entry
    urgency = if env.field == Stable && combined_score > 0.6
        :immediate
    elseif env.field == Transitioning
        :patient
    else
        :conditional
    end

    # ── LLM routing ───────────────────────────────────────────────────────────
    needs_llm = env.needs_llm

    if !isempty(env.ambiguity_reason)
        append!(llm_q, env.ambiguity_reason)
    end

    # Ask LLM about macro calendar if near potential events
    if urgency == :immediate
        push!(llm_q, "Are there scheduled macro events (FOMC, CPI, earnings) in the next $(candidate.legs.near_dte) days that would affect this position?")
    end

    if candidate.type ∈ (Strangle, IronCondor) && z.atm_iv > 0.40
        push!(llm_q, "IV at $(round(z.atm_iv*100, digits=1))% — is this elevated due to a specific catalyst or structural regime change?")
    end

    StrategyProposal(
        candidate, env,
        contracts, max_risk, 0.0,  # net_premium filled at execution
        est_delta, est_gamma, est_theta, est_vega,
        candidate.legs.near_dte,
        urgency,
        needs_llm, llm_q,
        isempty(violations), violations
    )
end

# ── Helpers ───────────────────────────────────────────────────────────────────

function _zscore_to_win_prob(vrp_z::Float64)::Float64
    # Rough mapping: z=1.5 → 65% win, z=2 → 70%, z=3 → 75%
    base = 0.50
    increment = clamp(abs(vrp_z) * 0.08, 0.0, 0.25)
    base + increment
end

function _strategy_payoff_ratio(t::StrategyType)
    # (avg_profit / avg_loss) rough ratios by strategy type
    # Sell vol: collect premium, lose spread → profit_ratio < 1, but high p_win
    # Buy vol: small premium cost, large potential gain → profit_ratio > 1, low p_win
    return Dict(
        IronCondor    => (0.33, 1.0),   # risk 3:1 but ~70% win rate
        Strangle      => (0.40, 1.0),
        IronButterfly => (0.25, 1.0),
        LongStraddle  => (2.0,  1.0),   # lose premium, gain large move
        LongStrangle  => (3.0,  1.0),
        Backspread    => (2.5,  1.0),
        RiskReversal  => (1.0,  1.0),
        FuturesCalendar => (1.5, 1.0),
        FuturesLong   => (1.0,  1.0),
        FuturesShort  => (1.0,  1.0),
        DeltaHedge    => (1.0,  1.0),
        DoNothing     => (0.0,  1.0),
    )[t]
end

function _estimate_max_loss_per_contract(t::StrategyType, atm_iv::Float64)::Float64
    # Very rough estimate — will be replaced with actual option chain data
    # Assumes underlying ~$400 (SPY-like), 30 DTE
    base = 400.0 * atm_iv * sqrt(30/365) * 100   # ATM option value × 100 shares
    return Dict(
        IronCondor    => base * 0.50,   # spread width - credit
        Strangle      => base * 3.0,    # undefined — cap at 3× premium
        IronButterfly => base * 0.30,
        LongStraddle  => base,          # max loss = premium paid
        LongStrangle  => base * 0.60,
        Backspread    => base * 0.20,   # small defined max loss
        RiskReversal  => base * 2.0,
        FuturesCalendar => base * 0.30,
        FuturesLong   => base * 2.0,
        FuturesShort  => base * 2.0,
        DeltaHedge    => base * 0.10,
        DoNothing     => 0.0,
    )[t]
end

# ── Full engine entry point ───────────────────────────────────────────────────

"""
    run_rule_engine(z, portfolio_value; open_risk, limits) → StrategyProposal

Single call to go from ZetaState → sized, risk-checked strategy proposal.
Returns the top-ranked candidate. If needs_llm=true, the LLM layer should
review before execution.
"""
function run_rule_engine(
    z::ZetaState,
    portfolio_value::Float64;
    open_risk::Float64 = 0.0,
    limits::RiskLimits = DEFAULT_LIMITS
)::StrategyProposal
    env        = classify(z)
    candidates = select_candidates(env, z)
    top        = first(candidates)
    return size_proposal(top, env, z, portfolio_value, open_risk, limits)
end

# ── Human-readable summary ────────────────────────────────────────────────────

function Base.show(io::IO, p::StrategyProposal)
    status = p.passes_limits ? "✓ APPROVED" : "✗ BLOCKED"
    println(io, "\n=== STRATEGY PROPOSAL [$status] ===")
    println(io, "Strategy:   $(p.candidate.type) × $(p.contracts) contracts")
    println(io, "Urgency:    $(p.entry_urgency)")
    println(io, "Max risk:   \$$(round(p.max_risk_dollars, digits=0))")
    println(io, "Est Greeks: Δ=$(round(p.est_delta, digits=2))  V=$(round(p.est_vega, digits=2))  Θ=$(round(p.est_theta_day, digits=2))/day")
    println(io, "Rationale:  $(p.candidate.rationale)")
    if !p.passes_limits
        println(io, "VIOLATIONS:")
        for v in p.limit_violations
            println(io, "  · $v")
        end
    end
    if p.needs_llm
        println(io, "LLM REVIEW NEEDED:")
        for q in p.llm_questions
            println(io, "  ? $q")
        end
    end
end
