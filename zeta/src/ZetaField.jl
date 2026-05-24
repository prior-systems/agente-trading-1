module ZetaField

using Dates, Statistics, LinearAlgebra, Logging

include("data/thetadata.jl")
include("data/databento.jl")
include("data/chain.jl")
include("greeks/black76.jl")
include("vol/surface.jl")
include("vol/hv.jl")
include("regime/detector.jl")
include("field/geometry.jl")
include("strategy/classifier.jl")
include("strategy/rules.jl")
include("strategy/sizing.jl")
include("ipc.jl")

export
    # Data clients
    ThetaDataClient, fetch_option_greeks, fetch_quote_greeks, fetch_trade_greeks,
    fetch_option_chain, fetch_index_price, fetch_open_interest,
    DatabentoClient, fetch_futures_definitions, fetch_mbo, fetch_trades_hist,

    # Option chain parsing
    StrikeCandidate, parse_chain_snapshot, closest_to_delta, is_liquid,
    available_expirations, best_expiration, smile_slice,

    # Black-76
    black76_price, black76_greeks, implied_vol_black76, Black76Greeks,

    # Vol surface
    SVIParams, fit_svi, svi_variance, smile_metrics, VolSurface,
    atm_vol, skew_25d, risk_reversal_25d, butterfly_25d, term_structure,

    # Historical vol
    rolling_hv, garch11_fit, GARCH11Params, variance_risk_premium,
    hurst_exponent,

    # Regime detection
    RegimeState, RegimeDetector, fit_hmm, current_regime, regime_probabilities,

    # Zeta field
    ZetaState, build_zeta_state, field_curvature, field_trajectory,

    # IPC
    init_zmq!, close_zmq!, send_signal, send_heartbeat,

    # Strategy rule engine
    MarketEnvironment, classify,
    StrategyType, StrategyCandidate, select_candidates,
    RiskLimits, DEFAULT_LIMITS, StrategyProposal, size_proposal,
    run_rule_engine

end
