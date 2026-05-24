using Dates, Statistics

# ── Option chain parsing — ThetaData v2 named-field format ───────────────────
# ThetaData response structure:
#   body.header.format → ["ms_of_day", "bid", "ask", ...] (field names)
#   body.response      → [[val1, val2, ...], ...]           (rows)
#
# We always use header.format to index into rows — never positional constants.
# Fix for TD-001.

struct StrikeCandidate
    root::String
    expiration::Date
    strike::Float64         # in dollars
    right::Symbol           # :call or :put
    dte::Int                # days to expiration
    # Greeks (from ThetaData — 1st order)
    delta::Float64
    gamma::Float64
    theta::Float64
    vega::Float64
    implied_vol::Float64
    # Quote
    bid::Float64
    ask::Float64
    mid::Float64
    bid_size::Int
    ask_size::Int
    # Market info
    underlying_price::Float64
    open_interest::Int
    # Derived
    spread_pct::Float64     # (ask - bid) / mid — liquidity filter
end

function StrikeCandidate(root, exp, strike, right, dte, delta, gamma, theta, vega,
                          iv, bid, ask, bid_size, ask_size, underlying, oi)
    mid        = (bid + ask) / 2
    spread_pct = mid > 0 ? (ask - bid) / mid : Inf
    StrikeCandidate(root, exp, strike, right, dte, delta, gamma, theta, vega,
                    iv, bid, ask, mid, bid_size, ask_size, underlying, oi, spread_pct)
end

# ── Parse ThetaData chain snapshot ───────────────────────────────────────────

function parse_chain_snapshot(body, root::String, today::Date)::Vector{StrikeCandidate}
    candidates = StrikeCandidate[]

    # The snapshot endpoint returns one record per contract
    # body may be a single-level or nested response depending on the endpoint used.
    # We support both the bulk and per-contract formats.

    records = if haskey(body, :header) && haskey(body, :response)
        # Standard v2 format: header + response array
        _parse_v2_format(body, root, today)
    elseif body isa AbstractVector
        # Bulk snapshot: array of per-contract bodies
        vcat([_parse_v2_format(rec, root, today) for rec in body]...)
    else
        @warn "Unknown chain snapshot format for $root"
        StrikeCandidate[]
    end

    return records
end

function _parse_v2_format(body, root::String, today::Date)::Vector{StrikeCandidate}
    candidates = StrikeCandidate[]

    !haskey(body, :header) && return candidates
    !haskey(body, :response) && return candidates

    # Build field index from header.format
    fmt = [Symbol(f) for f in body.header.format]
    idx = Dict(name => i for (i, name) in enumerate(fmt))

    # Per-contract metadata from header
    exp_str = get(body.header, :exp, nothing)
    isnothing(exp_str) && return candidates

    exp_date = try
        Date(string(exp_str), "yyyymmdd")
    catch
        return candidates
    end

    dte = Dates.value(exp_date - today)
    (dte < 1 || dte > 365) && return candidates

    strike_raw = get(body.header, :strike, nothing)
    isnothing(strike_raw) && return candidates
    strike = Float64(strike_raw) / 1000.0   # 1/10th cent → dollars

    right_str = get(body.header, :right, "")
    right = right_str == "C" ? :call : right_str == "P" ? :put : return candidates

    get_field(row, name, default=0.0) = haskey(idx, name) ? row[idx[name]] : default
    get_int(row, name, default=0)     = Int(round(get_field(row, name, Float64(default))))

    for row in body.response
        length(row) < length(fmt) && continue

        bid            = Float64(get_field(row, :bid))
        ask            = Float64(get_field(row, :ask))
        (bid <= 0 || ask <= bid) && continue   # stale or crossed quote

        implied_vol    = Float64(get_field(row, :implied_vol))
        iv_error       = Float64(get_field(row, :iv_error, 0.0))
        (implied_vol <= 0 || iv_error > 0.05) && continue   # bad IV calc

        delta          = Float64(get_field(row, :delta))
        gamma          = Float64(get_field(row, :gamma))
        theta          = Float64(get_field(row, :theta))
        vega           = Float64(get_field(row, :vega)) / 100.0
        underlying     = Float64(get_field(row, :underlying_price))
        bid_size       = get_int(row, :bid_size)
        ask_size       = get_int(row, :ask_size)

        # Open interest from a separate snapshot call if available
        oi = get_int(row, :open_interest, 0)

        push!(candidates, StrikeCandidate(
            root, exp_date, strike, right, dte,
            delta, gamma, theta, vega, implied_vol,
            bid, ask, bid_size, ask_size, underlying, oi
        ))
    end

    return candidates
end

# ── Filtering and selection helpers ──────────────────────────────────────────

# Minimum liquidity requirements for a tradeable option
function is_liquid(c::StrikeCandidate;
                   max_spread_pct::Float64 = 0.20,   # 20% bid-ask spread max
                   min_bid::Float64 = 0.05,           # minimum $0.05 bid
                   min_oi::Int = 100)::Bool
    c.spread_pct <= max_spread_pct &&
    c.bid >= min_bid &&
    c.open_interest >= min_oi
end

# Find the strike closest to a target delta within a given expiration bucket
function closest_to_delta(
    candidates::Vector{StrikeCandidate},
    target_delta::Float64,
    right::Symbol,
    target_dte::Int;
    dte_tolerance::Int = 7,   # ±7 days from target DTE
    liquidity_filter::Bool = true
)::Union{StrikeCandidate, Nothing}

    filtered = filter(candidates) do c
        c.right == right &&
        abs(c.dte - target_dte) <= dte_tolerance &&
        (!liquidity_filter || is_liquid(c))
    end

    isempty(filtered) && return nothing

    # Score by delta proximity
    best = argmin(c -> abs(c.delta - target_delta), filtered)
    return filtered[best]
end

# For a given expiration, return the full smile (sorted by strike)
function smile_slice(candidates::Vector{StrikeCandidate}, exp::Date, right::Symbol)
    filter(c -> c.expiration == exp && c.right == right, candidates) |>
    cs -> sort(cs, by=c -> c.strike)
end

# Available expirations sorted by DTE
function available_expirations(candidates::Vector{StrikeCandidate})::Vector{Date}
    unique(c.expiration for c in candidates) |> sort
end

# Choose the expiration closest to target DTE
function best_expiration(candidates::Vector{StrikeCandidate}, target_dte::Int)::Union{Date, Nothing}
    exps = available_expirations(candidates)
    isempty(exps) && return nothing
    exps[argmin(e -> abs(Dates.value(e - today()) - target_dte), exps)]
end
