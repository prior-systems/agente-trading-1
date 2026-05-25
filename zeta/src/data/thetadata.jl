using HTTP, JSON3, Dates

# ThetaData v3 — local Theta Terminal
# The Theta Terminal desktop app must be running for these calls to work.
# No authentication in HTTP requests; the terminal handles auth internally.
# In Docker: set THETADATA_BASE=http://host.docker.internal:25503/v3
const THETADATA_BASE = get(ENV, "THETADATA_BASE", "http://127.0.0.1:25503/v3")

struct ThetaDataClient
    base_url::String
end

ThetaDataClient() = ThetaDataClient(THETADATA_BASE)

# ── Response types ────────────────────────────────────────────────────────────

struct OptionGreeks
    ms_of_day::Int64
    date::Date
    # 1st order
    delta::Float64
    theta::Float64       # per day, divide by 252 for per-year
    vega::Float64        # divide by 100 for dollar vega per 1pt IV move
    rho::Float64         # divide by 100
    implied_vol::Float64
    iv_error::Float64
    underlying_price::Float64
end

struct QuoteGreeks  # 2nd order, on quotes
    ms_of_day::Int64
    date::Date
    bid::Float64
    ask::Float64
    gamma::Float64
    vanna::Float64   # dDelta/dVol = dVega/dSpot
    charm::Float64   # dDelta/dTime (delta bleed)
    vomma::Float64   # dVega/dVol (vol convexity)
    veta::Float64    # dVega/dTime
    implied_vol::Float64
    iv_error::Float64
    underlying_price::Float64
end

struct TradeGreeks2  # 2nd order, on trades
    ms_of_day::Int64
    date::Date
    size::Int64
    price::Float64
    gamma::Float64
    vanna::Float64
    charm::Float64
    vomma::Float64
    veta::Float64
    implied_vol::Float64
    underlying_price::Float64
end

struct OpenInterest
    ms_of_day::Int64
    date::Date
    open_interest::Int64
end

struct OptionContract
    root::String
    expiration::Date
    strike::Float64  # in dollars (API returns 1/10th cent → /1000)
    right::Symbol    # :call or :put
end

# ── HTTP helpers ──────────────────────────────────────────────────────────────

function _get(client::ThetaDataClient, path::String, params::Dict)
    url = "$(client.base_url)$(path)"
    # Local terminal — no auth header; request JSON explicitly
    all_params = merge(Dict("use_csv" => "false"), params)
    resp = HTTP.get(url; query=all_params, headers=["Accept" => "application/json"])
    body = JSON3.read(String(resp.body))
    return body
end

# Parse array-of-arrays response into typed structs.
# ThetaData returns: {"header": {"format": [...]}, "response": [[...], ...]}
function _parse_response(body, T::Type, mapping::Vector{Symbol})
    fmt = [Symbol(f) for f in body.header.format]
    rows = body.response
    result = T[]
    for row in rows
        d = Dict(fmt[i] => row[i] for i in eachindex(fmt))
        push!(result, _construct(T, d, mapping))
    end
    return result
end

function _construct(::Type{OptionGreeks}, d, _)
    date = Date(string(d[:date]), "yyyymmdd")
    OptionGreeks(
        d[:ms_of_day], date,
        d[:delta], d[:theta], d[:vega] / 100, d[:rho] / 100,
        d[:implied_vol], d[:iv_error], d[:underlying_price]
    )
end

function _construct(::Type{QuoteGreeks}, d, _)
    date = Date(string(d[:date]), "yyyymmdd")
    QuoteGreeks(
        d[:ms_of_day], date,
        d[:bid], d[:ask],
        d[:gamma], d[:vanna], d[:charm], d[:vomma], d[:veta],
        d[:implied_vol], d[:iv_error], d[:underlying_price]
    )
end

function _construct(::Type{TradeGreeks2}, d, _)
    date = Date(string(d[:date]), "yyyymmdd")
    TradeGreeks2(
        d[:ms_of_day], date, d[:size], d[:price],
        d[:gamma], d[:vanna], d[:charm], d[:vomma], d[:veta],
        d[:implied_vol], d[:underlying_price]
    )
end

# ── Public API ────────────────────────────────────────────────────────────────

function fetch_option_greeks(
    client::ThetaDataClient,
    contract::OptionContract,
    start_date::Date,
    end_date::Date
)::Vector{OptionGreeks}
    params = Dict(
        "root"       => contract.root,
        "exp"        => Dates.format(contract.expiration, "yyyymmdd"),
        "strike"     => string(round(Int, contract.strike * 1000)),
        "right"      => contract.right == :call ? "C" : "P",
        "start_date" => Dates.format(start_date, "yyyymmdd"),
        "end_date"   => Dates.format(end_date, "yyyymmdd"),
    )
    body = _get(client, "/hist/option/greeks", params)
    return _parse_response(body, OptionGreeks, Symbol[])
end

function fetch_quote_greeks(
    client::ThetaDataClient,
    contract::OptionContract,
    start_date::Date,
    end_date::Date
)::Vector{QuoteGreeks}
    params = Dict(
        "root"       => contract.root,
        "exp"        => Dates.format(contract.expiration, "yyyymmdd"),
        "strike"     => string(round(Int, contract.strike * 1000)),
        "right"      => contract.right == :call ? "C" : "P",
        "start_date" => Dates.format(start_date, "yyyymmdd"),
        "end_date"   => Dates.format(end_date, "yyyymmdd"),
    )
    body = _get(client, "/hist/option/greeks_second_order", params)
    return _parse_response(body, QuoteGreeks, Symbol[])
end

function fetch_trade_greeks(
    client::ThetaDataClient,
    contract::OptionContract,
    start_date::Date,
    end_date::Date
)::Vector{TradeGreeks2}
    params = Dict(
        "root"       => contract.root,
        "exp"        => Dates.format(contract.expiration, "yyyymmdd"),
        "strike"     => string(round(Int, contract.strike * 1000)),
        "right"      => contract.right == :call ? "C" : "P",
        "start_date" => Dates.format(start_date, "yyyymmdd"),
        "end_date"   => Dates.format(end_date, "yyyymmdd"),
    )
    body = _get(client, "/hist/option/trade_greeks_second_order", params)
    return _parse_response(body, TradeGreeks2, Symbol[])
end

# Returns all strikes × expirations for a root on a given date.
# Used to build the full vol surface.
function fetch_option_chain(
    client::ThetaDataClient,
    root::String,
    date::Date
)
    params = Dict(
        "root" => root,
        "date" => Dates.format(date, "yyyymmdd"),
    )
    body = _get(client, "/bulk_snapshot/option/greeks", params)
    return body
end

function fetch_open_interest(
    client::ThetaDataClient,
    contract::OptionContract,
    date::Date
)::Int64
    params = Dict(
        "root"   => contract.root,
        "exp"    => Dates.format(contract.expiration, "yyyymmdd"),
        "strike" => string(round(Int, contract.strike * 1000)),
        "right"  => contract.right == :call ? "C" : "P",
        "date"   => Dates.format(date, "yyyymmdd"),
    )
    body = _get(client, "/at_time/option/open_interest", params)
    # response: [[ms_of_day, open_interest, date]]
    return Int64(body.response[1][2])
end

function fetch_index_price(
    client::ThetaDataClient,
    symbol::String,  # e.g. "SPX", "VIX", "RUT"
    start_date::Date,
    end_date::Date
)
    params = Dict(
        "root"       => symbol,
        "start_date" => Dates.format(start_date, "yyyymmdd"),
        "end_date"   => Dates.format(end_date, "yyyymmdd"),
    )
    body = _get(client, "/hist/stock/eod", params)
    return body
end
