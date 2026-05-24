using ZetaField
using Dates, Logging, JSON3

# ── Config from environment ───────────────────────────────────────────────────

const THETADATA_KEY    = ENV["THETADATA_API_KEY"]
const DATABENTO_KEY    = ENV["DATABENTO_API_KEY"]
const EQUITY_ROOTS     = split(get(ENV, "EQUITY_ROOTS", "SPY,QQQ"), ",")
const CME_SYMBOLS      = split(get(ENV, "CME_SYMBOLS",  "ES.FUT,NQ.FUT"), ",")
const PORTFOLIO_VALUE  = parse(Float64, get(ENV, "PORTFOLIO_VALUE", "100000"))
const FIELD_INTERVAL   = parse(Int, get(ENV, "FIELD_INTERVAL_SEC", "60"))  # seconds
const LOG_LEVEL        = get(ENV, "LOG_LEVEL", "info")

# ── Logging setup ─────────────────────────────────────────────────────────────

global_logger(ConsoleLogger(
    stderr,
    LOG_LEVEL == "debug" ? Logging.Debug : Logging.Info
))

# ── Graceful shutdown ─────────────────────────────────────────────────────────

const _RUNNING = Ref(true)

function setup_signal_handlers!()
    # SIGTERM from systemd ExecStop
    ccall(:signal, Ptr{Cvoid}, (Cint, Ptr{Cvoid}),
          15, @cfunction(_ -> (_RUNNING[] = false; nothing), Cvoid, (Cint,)))
    # SIGINT from Ctrl+C in development
    ccall(:signal, Ptr{Cvoid}, (Cint, Ptr{Cvoid}),
          2,  @cfunction(_ -> (_RUNNING[] = false; nothing), Cvoid, (Cint,)))
end

# ── Per-symbol state ──────────────────────────────────────────────────────────

mutable struct SymbolState
    root::String
    prices::Vector{Float64}
    returns::Vector{Float64}
    iv_history::Vector{Float64}
    garch_params::Union{GARCH11Params, Nothing}
    hmm_model::Union{RegimeDetector, Nothing}
    last_updated::DateTime
end

SymbolState(root::String) = SymbolState(
    root, Float64[], Float64[], Float64[],
    nothing, nothing, DateTime(0)
)

# ── Initialization ────────────────────────────────────────────────────────────

function load_history!(state::SymbolState, td::ThetaDataClient)
    @info "Loading price history for $(state.root)"
    end_date   = today()
    start_date = end_date - Month(13)   # 13 months for GARCH + rolling windows

    # Fetch daily OHLC for the underlying
    raw = fetch_index_price(td, state.root, start_date, end_date)
    # raw.response: [[ms_of_day, price, date], ...]
    prices = [Float64(r[2]) for r in raw.response if !isnothing(r[2])]
    isempty(prices) && @warn "No price history for $(state.root)" && return

    state.prices  = prices
    state.returns = log_returns(prices)

    # Fit GARCH on full history
    length(state.returns) ≥ 60 && (state.garch_params = garch11_fit(state.returns))

    # Fit HMM on rolling HV as observations
    hvs = rolling_hv(prices, 21)
    valid_hvs = filter(!isnan, hvs)
    length(valid_hvs) ≥ 90 && (state.hmm_model = RegimeDetector(valid_hvs, 3, 63))

    # IV history from ATM options (last 252 trading days)
    # Approximated from index price history for now — refined later with option chain
    state.iv_history = valid_hvs  # placeholder; replaced with actual IV on first chain fetch

    state.last_updated = now()
    @info "$(state.root): $(length(prices)) price points, GARCH fitted, HMM ready"
end

# ── Field computation ─────────────────────────────────────────────────────────

function compute_field(
    state::SymbolState,
    td::ThetaDataClient,
    portfolio_greeks::NamedTuple,
    open_risk::Float64,
)::Union{Tuple{ZetaState, StrategyProposal, Vector{StrikeCandidate}}, Nothing}

    isempty(state.prices) && return nothing

    # 1. Fetch option chain snapshot (all strikes × expirations)
    chain_raw = try
        fetch_option_chain(td, state.root, today())
    catch e
        @warn "Option chain fetch failed for $(state.root)" exception=e
        return nothing
    end

    # Parse chain into typed candidates (fixes TD-001: named-field access)
    chain_candidates = parse_chain_snapshot(chain_raw, state.root, today())

    # 2. Build vol surface from chain data
    slices = _build_slices_from_chain(chain_raw, state)
    isempty(slices) && return nothing
    surface = VolSurface(slices, today())

    # 3. Vol regime summary
    atm_iv_now = atm_vol(surface, 30/365)
    push!(state.iv_history, atm_iv_now)
    length(state.iv_history) > 504 && popfirst!(state.iv_history)

    vrs = vol_regime_summary(state.prices, atm_iv_now; vrp_window=63)

    # 4. Smile metrics (use 30d slice)
    idx = argmin(abs.(surface.expiries .- 30/365))
    smile = smile_metrics(slices[min(idx, length(slices))], surface.params[min(idx, length(slices))])

    # 5. Term structure
    ts = term_structure(surface)

    # 6. Regime
    regime_state = if !isnothing(state.hmm_model)
        # Update HMM with latest observation
        push!(state.hmm_model.obs_history, vrs.garch_current)
        current_regime(state.hmm_model)
    else
        RegimeState([0.33, 0.33, 0.34], 2, 1.0, [:low_vol, :normal_vol, :stress_vol])
    end

    # 7. Assemble ZetaState
    z = build_zeta_state(
        timestamp       = now(),
        symbol          = state.root,
        vol_summary     = vrs,
        smile           = smile,
        term            = ts,
        regime          = regime_state,
        prices          = state.prices,
        portfolio_greeks = portfolio_greeks,
        iv_history      = state.iv_history,
    )

    # 8. Run rule engine
    proposal = run_rule_engine(z, PORTFOLIO_VALUE; open_risk=open_risk)

    return (z, proposal, chain_candidates)
end

# ── Build SmileSlice objects from ThetaData chain response ────────────────────

function _build_slices_from_chain(chain_raw, state::SymbolState)::Vector{SmileSlice}
    # chain_raw.response is a list of records per strike/expiration
    # Group by expiration, build SmileSlice per tenor
    by_exp = Dict{Date, Vector{Tuple{Float64,Float64,Float64,Float64,Float64}}}()
    # (strike, mid_iv, forward, weight, delta)

    for rec in chain_raw.response
        # ThetaData chain: [ms_of_day, bid, ask, ..., implied_vol, ..., underlying_price, date, strike, right, exp]
        # Exact fields depend on endpoint — using positional indices from doc
        try
            exp_str = string(rec[end])   # YYYYMMDD
            exp_date = Date(exp_str, "yyyymmdd")
            dte = Dates.value(exp_date - today())
            (dte < 7 || dte > 180) && continue   # skip very near/far expirations

            strike       = Float64(rec[end-1]) / 1000.0
            implied_vol  = Float64(rec[11])
            underlying   = Float64(rec[13])
            bid          = Float64(rec[2])
            ask          = Float64(rec[3])

            (implied_vol ≤ 0 || bid ≤ 0 || ask ≤ bid) && continue

            # Weight: inverse of bid-ask spread (tighter = more reliable)
            weight = 1.0 / max(ask - bid, 0.01)

            entry = (strike, implied_vol, underlying, weight, 0.0)
            push!(get!(by_exp, exp_date, []), entry)
        catch _
            continue
        end
    end

    slices = SmileSlice[]
    for (exp_date, entries) in sort(collect(by_exp), by=x->x[1])
        length(entries) < 5 && continue   # need at least 5 strikes for SVI fit
        T        = Dates.value(exp_date - today()) / 365.0
        strikes  = [e[1] for e in entries]
        ivs      = [e[2] for e in entries]
        forward  = mean([e[3] for e in entries])
        weights  = [e[4] for e in entries]
        push!(slices, SmileSlice(T, strikes, ivs, forward, weights))
    end
    return slices
end

# ── Main loop ─────────────────────────────────────────────────────────────────

function main()
    @info "ZetaField Engine starting" roots=EQUITY_ROOTS interval_sec=FIELD_INTERVAL
    setup_signal_handlers!()

    # Initialize data clients
    td = ThetaDataClient(THETADATA_KEY)
    db = DatabentoClient(DATABENTO_KEY)

    # Initialize ZMQ
    init_zmq!()
    atexit(close_zmq!)

    # Load history for all symbols
    states = Dict(root => SymbolState(String(root)) for root in EQUITY_ROOTS)
    for (root, state) in states
        try
            load_history!(state, td)
        catch e
            @error "Failed to load history for $root" exception=e
        end
    end

    # Placeholder portfolio Greeks — will come from Rust OMS via health endpoint
    portfolio_greeks = (delta=0.0, gamma=0.0, theta=0.0, vega=0.0, vanna=0.0, charm=0.0)
    open_risk = 0.0

    heartbeat_counter = 0

    @info "ZetaField Engine ready — entering main loop"

    while _RUNNING[]
        loop_start = time()

        for (root, state) in states
            _RUNNING[] || break
            try
                result = compute_field(state, td, portfolio_greeks, open_risk)
                if !isnothing(result)
                    z, proposal, candidates = result
                    send_signal(z, proposal, candidates)
                    @info "Field computed" symbol=root strategy=string(proposal.candidate.type) needs_llm=proposal.needs_llm passes=proposal.passes_limits candidates=length(candidates)
                end
            catch e
                @error "Field computation failed for $root" exception=(e, catch_backtrace())
            end
        end

        # Heartbeat every 60s
        heartbeat_counter += 1
        heartbeat_counter % (60 ÷ max(FIELD_INTERVAL, 1)) == 0 && send_heartbeat()

        # Sleep until next interval, accounting for computation time
        elapsed = time() - loop_start
        sleep_time = max(0.0, FIELD_INTERVAL - elapsed)
        elapsed > FIELD_INTERVAL && @warn "Field computation took longer than interval" elapsed_sec=round(elapsed, digits=1)

        # Sleep in small chunks so SIGTERM is handled quickly
        slept = 0.0
        while _RUNNING[] && slept < sleep_time
            sleep(min(1.0, sleep_time - slept))
            slept += 1.0
        end
    end

    @info "ZetaField Engine shutting down gracefully"
end

main()
