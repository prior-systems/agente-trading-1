using HTTP, JSON3, Dates

# Databento Historical API — CME Globex (GLBX.MDP3)
# Docs: https://databento.com/docs
const DATABENTO_BASE = "https://hist.databento.com/v0"

struct DatabentoClient
    api_key::String
end

# ── Instrument definition (from `definition` schema) ─────────────────────────

struct FuturesDefinition
    instrument_id::Int64
    raw_symbol::String          # e.g. "ESM4", "ESM4 C5000"
    asset::String               # product root, e.g. "ES"
    instrument_class::Char      # 'F'=future, 'O'=option on future, 'S'=stock
    expiration::DateTime
    # options-specific (zero/nothing for futures)
    strike_price::Float64
    strike_price_currency::String
    underlying::String          # underlying instrument raw_symbol
    call_put::Char              # 'C', 'P', or '\0' for futures
    # contract specs
    contract_multiplier::Float64
    min_price_increment::Float64
    min_price_increment_amount::Float64
    currency::String
    exchange::String
end

# ── Market data records ───────────────────────────────────────────────────────

struct TradeRecord
    ts_event::Int64          # nanoseconds since epoch
    instrument_id::Int64
    price::Float64           # converted from fixed-point (divide by 1e9)
    size::Int64
    side::Char               # 'A'=ask aggressor, 'B'=bid aggressor, 'N'=none
    action::Char             # 'T'=trade
    sequence::Int64
end

struct MBORecord
    ts_event::Int64
    instrument_id::Int64
    order_id::Int64
    price::Float64
    size::Int64
    side::Char               # 'A'=ask, 'B'=bid
    action::Char             # 'A'=add, 'C'=cancel, 'M'=modify, 'T'=trade, 'F'=fill
    sequence::Int64
end

struct MBP1Record
    ts_event::Int64
    instrument_id::Int64
    bid_px::Float64
    ask_px::Float64
    bid_sz::Int64
    ask_sz::Int64
    bid_ct::Int64            # number of orders at bid
    ask_ct::Int64
    action::Char
    side::Char
    sequence::Int64
end

struct OHLCVRecord
    ts_event::Int64
    instrument_id::Int64
    open::Float64
    high::Float64
    low::Float64
    close::Float64
    volume::Int64
end

# Statistics schema — settlement, open interest, volume
struct StatisticsRecord
    ts_event::Int64
    instrument_id::Int64
    stat_type::Int32         # 1=open_price, 2=settlement, 3=high_price, 4=low_price
                             # 5=open_interest, 6=volume, 7=prior_settlement
    price::Float64
    quantity::Int64
    sequence::Int64
end

# ── HTTP helper ───────────────────────────────────────────────────────────────

function _get(client::DatabentoClient, path::String, params::Dict)
    url = "$(DATABENTO_BASE)$(path)"
    # Databento uses HTTP Basic auth: api_key as username, empty password
    creds = HTTP.base64encode("$(client.api_key):")
    headers = ["Authorization" => "Basic $creds",
               "Accept" => "application/json"]
    resp = HTTP.get(url; query=params, headers=headers)
    return JSON3.read(String(resp.body))
end

# ── Instrument definitions ────────────────────────────────────────────────────

function fetch_futures_definitions(
    client::DatabentoClient,
    symbols::Vector{String},  # e.g. ["ES.FUT", "NQ.FUT"] — continuous or specific
    date::Date;
    dataset::String = "GLBX.MDP3"
)::Vector{FuturesDefinition}
    params = Dict(
        "dataset"    => dataset,
        "symbols"    => join(symbols, ","),
        "schema"     => "definition",
        "start"      => Dates.format(date, "yyyy-mm-dd") * "T00:00:00",
        "end"        => Dates.format(date + Day(1), "yyyy-mm-dd") * "T00:00:00",
        "encoding"   => "json",
        "pretty_px"  => "true",
        "pretty_ts"  => "false",
    )
    body = _get(client, "/timeseries.get_range", params)
    defs = FuturesDefinition[]
    for rec in body
        push!(defs, FuturesDefinition(
            rec.instrument_id,
            rec.raw_symbol,
            get(rec, :asset, ""),
            first(get(rec, :instrument_class, "F")),
            DateTime(get(rec, :expiration, 0) ÷ 10^9 |> unix2datetime),
            get(rec, :strike_price, 0.0),
            get(rec, :strike_price_currency, ""),
            get(rec, :underlying, ""),
            first(get(rec, :call_put, "\0")),
            get(rec, :contract_multiplier, 1.0),
            get(rec, :min_price_increment, 0.0),
            get(rec, :min_price_increment_amount, 0.0),
            get(rec, :currency, "USD"),
            get(rec, :exchange, "GLBX"),
        ))
    end
    return defs
end

# ── Market data ───────────────────────────────────────────────────────────────

function fetch_mbo(
    client::DatabentoClient,
    symbols::Vector{String},
    start_dt::DateTime,
    end_dt::DateTime;
    dataset::String = "GLBX.MDP3"
)::Vector{MBORecord}
    params = Dict(
        "dataset"   => dataset,
        "symbols"   => join(symbols, ","),
        "schema"    => "mbo",
        "start"     => Dates.format(start_dt, "yyyy-mm-ddTHH:MM:SS"),
        "end"       => Dates.format(end_dt, "yyyy-mm-ddTHH:MM:SS"),
        "encoding"  => "json",
        "pretty_px" => "true",
    )
    body = _get(client, "/timeseries.get_range", params)
    return [MBORecord(
        r.ts_event, r.instrument_id, r.order_id,
        r.price, r.size, first(r.side), first(r.action), r.sequence
    ) for r in body]
end

function fetch_trades_hist(
    client::DatabentoClient,
    symbols::Vector{String},
    start_dt::DateTime,
    end_dt::DateTime;
    dataset::String = "GLBX.MDP3"
)::Vector{TradeRecord}
    params = Dict(
        "dataset"   => dataset,
        "symbols"   => join(symbols, ","),
        "schema"    => "trades",
        "start"     => Dates.format(start_dt, "yyyy-mm-ddTHH:MM:SS"),
        "end"       => Dates.format(end_dt, "yyyy-mm-ddTHH:MM:SS"),
        "encoding"  => "json",
        "pretty_px" => "true",
    )
    body = _get(client, "/timeseries.get_range", params)
    return [TradeRecord(
        r.ts_event, r.instrument_id, r.price,
        r.size, first(r.side), first(r.action), r.sequence
    ) for r in body]
end

function fetch_ohlcv(
    client::DatabentoClient,
    symbols::Vector{String},
    start_dt::DateTime,
    end_dt::DateTime;
    dataset::String = "GLBX.MDP3",
    schema::String = "ohlcv-1m"   # ohlcv-1s, ohlcv-1m, ohlcv-1h, ohlcv-1d
)::Vector{OHLCVRecord}
    params = Dict(
        "dataset"   => dataset,
        "symbols"   => join(symbols, ","),
        "schema"    => schema,
        "start"     => Dates.format(start_dt, "yyyy-mm-ddTHH:MM:SS"),
        "end"       => Dates.format(end_dt, "yyyy-mm-ddTHH:MM:SS"),
        "encoding"  => "json",
        "pretty_px" => "true",
    )
    body = _get(client, "/timeseries.get_range", params)
    return [OHLCVRecord(
        r.ts_event, r.instrument_id,
        r.open, r.high, r.low, r.close, r.volume
    ) for r in body]
end

function fetch_statistics(
    client::DatabentoClient,
    symbols::Vector{String},
    date::Date;
    dataset::String = "GLBX.MDP3"
)::Vector{StatisticsRecord}
    params = Dict(
        "dataset"   => dataset,
        "symbols"   => join(symbols, ","),
        "schema"    => "statistics",
        "start"     => Dates.format(date, "yyyy-mm-dd") * "T00:00:00",
        "end"       => Dates.format(date + Day(1), "yyyy-mm-dd") * "T00:00:00",
        "encoding"  => "json",
        "pretty_px" => "true",
    )
    body = _get(client, "/timeseries.get_range", params)
    return [StatisticsRecord(
        r.ts_event, r.instrument_id, r.stat_type,
        get(r, :price, 0.0), get(r, :quantity, 0), r.sequence
    ) for r in body]
end

# ── Order flow imbalance ──────────────────────────────────────────────────────
# Two flavors:
#   compute_ofi_from_mbo  — for historical MBO data (available up to 1 month back)
#   compute_ofi_from_mbp1 — for live L1 data (Standard plan) and MBP-1 history

struct OrderFlowMetrics
    period_start::Int64
    period_end::Int64
    instrument_id::Int64
    buy_volume::Int64
    sell_volume::Int64
    ofi::Float64        # order flow imbalance: (buy - sell) / (buy + sell)
    add_count::Int64    # MBO-only: orders added (0 for MBP-1 derived)
    cancel_count::Int64 # MBO-only: orders cancelled
    cancel_ratio::Float64
end

# MBO-based OFI (historical data only — MBO not available in live Standard plan)
function compute_ofi_from_mbo(records::Vector{MBORecord}, instrument_id::Int64)::OrderFlowMetrics
    buy_vol = sell_vol = add_ct = cancel_ct = 0
    t_start = typemax(Int64)
    t_end   = typemin(Int64)
    for r in records
        r.instrument_id != instrument_id && continue
        t_start = min(t_start, r.ts_event)
        t_end   = max(t_end, r.ts_event)
        if r.action == 'T' || r.action == 'F'
            r.side == 'B' ? (buy_vol  += r.size) : (sell_vol += r.size)
        elseif r.action == 'A'
            add_ct += 1
        elseif r.action == 'C'
            cancel_ct += 1
        end
    end
    total = buy_vol + sell_vol
    ofi   = total > 0 ? (buy_vol - sell_vol) / total : 0.0
    cr    = add_ct > 0 ? cancel_ct / add_ct : 0.0
    return OrderFlowMetrics(t_start, t_end, instrument_id,
                            buy_vol, sell_vol, ofi, add_ct, cancel_ct, cr)
end

# MBP-1-based OFI (works for both live L1 and historical MBP-1 data).
# Formula: OFI ≈ Σ(Δbid_sz - Δask_sz) across consecutive ticks.
# Positive → net bid-side pressure (buying); negative → ask-side pressure.
function compute_ofi_from_mbp1(records::Vector{MBP1Record}, instrument_id::Int64)::OrderFlowMetrics
    t_start = typemax(Int64)
    t_end   = typemin(Int64)
    cumulative_ofi = 0
    prev_bid_sz = prev_ask_sz = -1

    for r in records
        r.instrument_id != instrument_id && continue
        t_start = min(t_start, r.ts_event)
        t_end   = max(t_end, r.ts_event)

        if prev_bid_sz >= 0
            delta_bid = r.bid_sz - prev_bid_sz
            delta_ask = r.ask_sz - prev_ask_sz
            cumulative_ofi += delta_bid - delta_ask
        end
        prev_bid_sz = r.bid_sz
        prev_ask_sz = r.ask_sz
    end

    n = count(r -> r.instrument_id == instrument_id, records)
    ofi_normalized = n > 1 ? clamp(cumulative_ofi / (n * 100.0), -1.0, 1.0) : 0.0

    return OrderFlowMetrics(t_start, t_end, instrument_id,
                            0, 0, ofi_normalized, 0, 0, 0.0)
end

# Backward-compatible alias
const compute_ofi = compute_ofi_from_mbo
