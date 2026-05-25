#!/usr/bin/env python3
"""
Envía un ZetaSignal de prueba al executor vía ZMQ.
Uso: python3 test_signal.py [--strategy IronCondor|Strangle|LongStraddle]

El executor debe estar corriendo con DRY_RUN=true antes de ejecutar este script.
"""

import zmq, json, sys, time, datetime

ENDPOINT = "ipc:///tmp/zeta.sock"
STRATEGY = sys.argv[2] if len(sys.argv) > 2 else (sys.argv[1] if len(sys.argv) > 1 else "IronCondor")

# Fecha de expiración ~30 DTE desde hoy
exp = (datetime.date.today() + datetime.timedelta(days=30)).strftime("%Y%m%d")
dte = 30
underlying = 530.0

# Chain candidates: suficiente para cualquier estrategia
# Calls
calls = [
    {"delta":  0.50, "strike": 530.0, "right": "call"},
    {"delta":  0.30, "strike": 537.0, "right": "call"},
    {"delta":  0.20, "strike": 542.0, "right": "call"},
    {"delta":  0.16, "strike": 545.0, "right": "call"},
    {"delta":  0.10, "strike": 548.0, "right": "call"},
    {"delta":  0.05, "strike": 552.0, "right": "call"},
]
# Puts
puts = [
    {"delta": -0.50, "strike": 530.0, "right": "put"},
    {"delta": -0.30, "strike": 523.0, "right": "put"},
    {"delta": -0.20, "strike": 518.0, "right": "put"},
    {"delta": -0.16, "strike": 515.0, "right": "put"},
    {"delta": -0.10, "strike": 512.0, "right": "put"},
    {"delta": -0.05, "strike": 508.0, "right": "put"},
]

def make_candidate(c):
    strike = c["strike"]
    delta  = c["delta"]
    bid    = round(abs(delta) * 8 + 0.05, 2)   # rough price proxy
    ask    = round(bid * 1.10, 2)
    return {
        "root":             "SPY",
        "expiration":       exp,
        "strike":           strike,
        "right":            c["right"],
        "dte":              dte,
        "delta":            delta,
        "gamma":            0.03,
        "theta":            -0.05,
        "vega":             0.12,
        "implied_vol":      0.18,
        "bid":              bid,
        "ask":              ask,
        "mid":              round((bid + ask) / 2, 2),
        "bid_size":         150,
        "ask_size":         120,
        "underlying_price": underlying,
        "open_interest":    5000,
        "spread_pct":       round((ask - bid) / ask, 3),
    }

candidates = [make_candidate(c) for c in calls + puts]

signal = {
    "timestamp":     datetime.datetime.utcnow().isoformat() + "Z",
    "symbol":        "SPY",
    "zeta_context":  (
        "VRP z-score: 2.1 (elevated, edge for vol selling). "
        "Regime: low_vol=0.68 normal=0.24 stress=0.08. "
        "ATM IV: 0.18, HV30: 0.14. RR25: -0.02. BF25: 0.005. "
        "Term structure: normal. No macro events next 7 days."
    ),
    "needs_llm":     False,
    "llm_questions": [],
    "proposal": {
        "strategy_type":    STRATEGY,
        "contracts":        1,
        "max_risk_dollars": 700.0,
        "est_delta":        0.0,
        "est_vega":         -55.0,
        "est_theta_day":    18.0,
        "target_dte":       30,
        "entry_urgency":    "normal",
        "rationale":        "High VRP with stable low-vol regime — sell vol.",
        "passes_limits":    True,
        "limit_violations": [],
    },
    "chain_candidates": candidates,
}

ctx = zmq.Context()
sock = ctx.socket(zmq.PUSH)
sock.connect(ENDPOINT)
time.sleep(0.3)   # let the connection establish

payload = json.dumps(signal)
sock.send_string(payload)
print(f"✓ Señal enviada — estrategia: {STRATEGY}, candidatos: {len(candidates)}")
print(f"  Endpoint: {ENDPOINT}")
print(f"  Expiration: {exp} ({dte} DTE)")
sock.close()
ctx.term()
