# agente-trading-1

Sistema de trading autónomo para opciones y futuros basado en el Zeta Field.

## Stack

- **Julia** (`zeta/`) — Zeta Field Engine: vol surface, Greeks, regime detection, field geometry
- **Rust** (`executor/`) — Execution layer: real-time data feeds, OMS, broker integration
- **Agents** (`agents/`) — LLM agents para estrategia de alto nivel (consumen el Zeta State)

## Data sources

| Source | Coverage | Greeks |
|--------|----------|--------|
| ThetaData v2 | Equity options, indices (SPX/VIX/RUT) | Pre-calculados hasta 3rd order |
| Databento GLBX.MDP3 | CME futures + options on futures | Raw market data — calcular con Black-76 |

## Architecture

```
ThetaData WS ──→ Rust executor ──→ OMS (positions + live Greeks)
Databento WS ──→ Rust executor ──→ OFI (order flow imbalance)
                                         ↓
                              Julia ZetaField Engine
                              ├── Vol surface (SVI)
                              ├── GARCH(1,1) + HMM regimes
                              ├── Black-76 Greeks (CME)
                              └── ZetaState embedding
                                         ↓
                              LLM Strategy Agents
                              (consume zeta_context_string())
```

## Key concepts

- **VRP** (Variance Risk Premium): IV² - HV²_realized. Positive = sell vol edge.
- **Zeta Field**: geometric state of market — vol regime, smile, order flow, momentum.
- **Dynamic hedging**: portfolio Greeks as moving targets; re-hedge on delta deviation.
- **Heteroscedasticity**: GARCH(1,1) + HMM handle vol clustering and regime transitions.
- **Smile as intelligence**: 25d risk reversal = fear/greed; butterfly = tail pricing; term structure shape = event risk.

## Running

```bash
# Julia engine (REPL)
cd zeta && julia --project=. -e 'using ZetaField'

# Rust executor
cd executor && cargo run --release
```

## Environment variables

Copy `.env.example` to `.env` and fill in API keys.
