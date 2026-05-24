using ZMQ, JSON3, Dates, Logging

# ── ZetaField → Executor IPC via ZeroMQ PUSH/PULL ────────────────────────────
# Julia PUSH connects to Rust PULL (Rust binds — it's the stable endpoint)
# Transport: ipc:///tmp/zeta.sock (Unix socket, same machine, fastest)
# Fallback:  tcp://127.0.0.1:5555 (if running in Docker containers)

const IPC_ENDPOINT = get(ENV, "ZMQ_ENDPOINT", "ipc:///tmp/zeta.sock")

# Thread-safe ZMQ context — one per process
const _ZMQ_CTX    = Ref{ZMQ.Context}()
const _ZMQ_SOCKET = Ref{ZMQ.Socket}()
const _ZMQ_LOCK   = ReentrantLock()

function init_zmq!()
    ctx    = ZMQ.Context()
    socket = ZMQ.Socket(ctx, ZMQ.PUSH)

    # Don't block on send if Rust is not ready — drop the message
    ZMQ.set_option(socket, ZMQ.SNDHWM, 100)      # high-water mark: 100 messages
    ZMQ.set_option(socket, ZMQ.SNDTIMEO, 100)    # 100ms send timeout
    ZMQ.set_option(socket, ZMQ.LINGER, 0)        # don't wait on close

    ZMQ.connect(socket, IPC_ENDPOINT)
    @info "ZMQ PUSH connected to $IPC_ENDPOINT"

    _ZMQ_CTX[]    = ctx
    _ZMQ_SOCKET[] = socket
end

function close_zmq!()
    if isassigned(_ZMQ_SOCKET)
        ZMQ.close(_ZMQ_SOCKET[])
        ZMQ.close(_ZMQ_CTX[])
        @info "ZMQ socket closed"
    end
end

# ── ZetaSignal — the message sent to Rust ────────────────────────────────────

struct ZetaSignalMsg
    timestamp::String
    symbol::String
    zeta_context::String
    needs_llm::Bool
    llm_questions::Vector{String}
    proposal::Dict{String, Any}
    chain_candidates::Vector{Dict{String, Any}}
end

function _proposal_dict(p::StrategyProposal)
    Dict{String, Any}(
        "strategy_type"    => string(p.candidate.type),
        "contracts"        => p.contracts,
        "max_risk_dollars" => p.max_risk_dollars,
        "est_delta"        => p.est_delta,
        "est_vega"         => p.est_vega,
        "est_theta_day"    => p.est_theta_day,
        "target_dte"       => p.target_dte,
        "entry_urgency"    => string(p.entry_urgency),
        "rationale"        => p.candidate.rationale,
        "passes_limits"    => p.passes_limits,
        "limit_violations" => p.limit_violations,
    )
end

# ── Candidate serialisation ───────────────────────────────────────────────────

function _candidate_dict(c::StrikeCandidate)::Dict{String, Any}
    Dict{String, Any}(
        "root"             => c.root,
        "expiration"       => Dates.format(c.expiration, "yyyymmdd"),  # YYYYMMDD for OCC symbol
        "strike"           => c.strike,
        "right"            => string(c.right),   # :call → "call", :put → "put"
        "dte"              => c.dte,
        "delta"            => c.delta,
        "gamma"            => c.gamma,
        "theta"            => c.theta,
        "vega"             => c.vega,
        "implied_vol"      => c.implied_vol,
        "bid"              => c.bid,
        "ask"              => c.ask,
        "mid"              => c.mid,
        "bid_size"         => c.bid_size,
        "ask_size"         => c.ask_size,
        "underlying_price" => c.underlying_price,
        "open_interest"    => c.open_interest,
        "spread_pct"       => c.spread_pct,
    )
end

# ── Send ──────────────────────────────────────────────────────────────────────

function send_signal(z::ZetaState, proposal::StrategyProposal, candidates::Vector{StrikeCandidate})
    !isassigned(_ZMQ_SOCKET) && @warn "ZMQ not initialized — call init_zmq!()" && return

    msg = ZetaSignalMsg(
        string(z.timestamp),
        z.symbol,
        zeta_context_string(z),
        proposal.needs_llm,
        proposal.llm_questions,
        _proposal_dict(proposal),
        [_candidate_dict(c) for c in candidates],
    )

    payload = JSON3.write(msg)

    lock(_ZMQ_LOCK) do
        try
            ZMQ.send(_ZMQ_SOCKET[], ZMQ.Message(payload))
            @debug "ZetaSignal sent" strategy=string(proposal.candidate.type) needs_llm=proposal.needs_llm bytes=length(payload)
        catch e
            # EAGAIN = Rust not ready (socket full or timeout)
            @warn "ZMQ send failed — executor may not be running" exception=e
        end
    end
end

# Heartbeat — sent every minute so Rust knows Julia is alive
function send_heartbeat()
    !isassigned(_ZMQ_SOCKET) && return
    payload = JSON3.write(Dict("type" => "heartbeat", "ts" => string(now())))
    lock(_ZMQ_LOCK) do
        try
            ZMQ.send(_ZMQ_SOCKET[], ZMQ.Message(payload))
        catch _
        end
    end
end
