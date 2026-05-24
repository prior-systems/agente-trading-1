# agente-trading-1

Sistema de trading autónomo para opciones y futuros basado en el Zeta Field.

## Stack

| Capa | Lenguaje | Responsabilidad |
|------|----------|-----------------|
| Zeta Field Engine | Julia (`zeta/`) | Vol surface, Greeks, GARCH, HMM, field geometry, rule engine |
| Execution Engine | Rust (`executor/`) | Data feeds WS, OMS, LLM agent, broker, IPC receiver |
| IPC | ZeroMQ PUSH/PULL | Julia PUSH → Rust PULL, `ipc:///tmp/zeta.sock` |
| LLM Agent | Rust → Anthropic API | `claude-opus-4-7`, tool_use forzado, solo para señales ambiguas |

## Estado del proyecto — actualizado 2026-05-24

### Completado ✓
- `zeta/src/data/thetadata.jl` — ThetaData v2 REST client (equity options Greeks 1st/2nd order)
- `zeta/src/data/databento.jl` — Databento GLBX.MDP3 client (CME MBO/trades/definitions + OFI)
- `zeta/src/greeks/black76.jl` — Black-76 pricing + Greeks 1st/2nd/3rd order + IV solver
- `zeta/src/vol/surface.jl` — SVI vol surface fitting + smile metrics (RR25, BF25, skew, term structure)
- `zeta/src/vol/hv.jl` — Rolling HV, GARCH(1,1), Hurst exponent, VRP
- `zeta/src/regime/detector.jl` — HMM (Baum-Welch), regime como distribución probabilística
- `zeta/src/field/geometry.jl` — ZetaState (30+ dims), build_zeta_state(), zeta_context_string()
- `zeta/src/strategy/classifier.jl` — MarketEnvironment, 6 patrones de ambigüedad
- `zeta/src/strategy/rules.jl` — select_candidates(): IronCondor, Strangle, Straddle, etc.
- `zeta/src/strategy/sizing.jl` — Fractional Kelly, hard limits, run_rule_engine()
- `zeta/src/ipc.jl` — ZMQ PUSH, send_signal(), send_heartbeat()
- `zeta/src/server.jl` — Main loop: carga historia → GARCH/HMM → field → ZMQ
- `executor/src/data/` — ThetaData WS + Databento WS, tipos canónicos de eventos
- `executor/src/orders/mod.rs` — OMS con live Greeks tracking, delta hedge trigger
- `executor/src/orders/persistence.rs` — Append-only event log, replay/reconstruct en startup
- `executor/src/broker/alpaca.rs` — Alpaca Markets (paper + live), multi-leg orders
- `executor/src/agent/` — Anthropic client, decision tool schema, prompt builder
- `executor/src/ipc.rs` — ZMQ PULL bind, ZetaSignal deserialización
- `executor/src/main.rs` — Event loop: data feeds + ZMQ + LLM agent routing
- `deploy/` — systemd units, setup.sh, status.sh, on_executor_stop.sh
- `docs/adr/` — ADR-0001 al ADR-0008

### En progreso 🔧
- Option chain → strikes concretos para órdenes (falta el último eslabón)
  `StrategyDecision` → `OrderLeg` con strikes/expirations reales del chain

### Pendiente 📋
- IBKR integration (actualmente solo Alpaca)
- Backtesting framework con datos históricos de ThetaData
- Monitoring: exportar métricas del OMS a Grafana/Prometheus
- SOC2 controls: encriptación en reposo del OMS log, acceso formal

---

## Deuda técnica conocida

| ID | Archivo | Descripción | Severidad |
|----|---------|-------------|-----------|
| TD-001 | `server.jl:_build_slices_from_chain()` | Usa índices posicionales del response de ThetaData. Frágil si cambia el orden de campos. Migrar a named fields. | Alta |
| TD-002 | `black76.jl:enrich_greeks()` | Recalcula delta con Black-Scholes en lugar de usar el delta que ya viene de ThetaData | Media |
| TD-003 | `surface.jl:delta25_strike()` | Newton-Raphson sin fallback explícito. Puede no converger en condiciones extremas | Media |
| TD-004 | `surface.jl` | No verifica arbitrage de calendario entre slices (total variance debe ser monótona en T) | Media |
| TD-005 | `sizing.jl:_zscore_to_win_prob()` | Mapeo VRP z-score → probabilidad de win sin calibración histórica. Necesita backtesting. | Alta |
| TD-006 | `persistence.rs` | `sizing_adjustment` del LLM no queda registrado en el event log del OMS | Baja |
| TD-007 | `main.rs` | `oms_td` y `oms_db` se crean pero no se usan (variables muertas) | Baja |

---

## Arquitectura de datos

```
ThetaData WS ──→ Rust executor ──→ OMS (live Greeks por posición)
Databento WS ──→ Rust executor ──→ OFI (order flow imbalance buffer)

Julia server loop (cada 60s):
  ThetaData REST → option chain snapshot
  → SVI fitting por expiration
  → VolSurface + SmileMetrics + TermStructure
  → GARCH conditional vol + VRP z-score
  → HMM regime probabilities
  → ZetaState (30+ dimensiones)
  → run_rule_engine() → StrategyProposal
  → ZMQ PUSH → Rust

Rust main loop:
  ZMQ PULL recv ZetaSignal
  → if needs_llm: Anthropic API → StrategyDecision
  → if approved: [pendiente] chain snapshot → OrderLegs → OMS → Alpaca
  → EventLog.append()
```

## Flujo de una señal

```
1. Julia computa ZetaState cada 60s
2. run_rule_engine() → StrategyProposal (ej: IronCondor, 2 contratos, $2000 riesgo)
3. ZMQ PUSH → Rust recibe ZetaSignal

4a. needs_llm=false (campo estable, señal clara):
    → Decision construida directamente, sin llamada API
    → approved=true → [ejecutar]

4b. needs_llm=true (ambigüedad detectada):
    → Anthropic API: zeta_context_string() + propuesta + preguntas específicas
    → Model responde via submit_decision tool
    → StrategyDecision parseada por serde
    → Si approved=true → [ejecutar]

5. [Pendiente] StrategyDecision → strikes reales del chain → OrderLeg[] → OMS.submit()
6. EventLog.append(ev_submitted(order))
7. Broker confirma → EventLog.append(ev_filled(...))
```

## Conceptos clave

- **VRP** (Variance Risk Premium): `IV² - HV²_realized`. Positivo = edge para vender vol.
- **Zeta Field**: estado geométrico del mercado — no reacciona a precio, modela la geometría subyacente.
- **Greeks como blancos móviles**: el hedge correcto cambia con cada tick. El OMS trackea Greeks en tiempo real.
- **Régimen como distribución**: HMM da `P(low_vol)=0.72, P(normal)=0.21, P(stress)=0.07`. La incertidumbre es información.
- **Smile como inteligencia**: RR25 negativo = miedo bajista; BF25 alto = colas priceadas caras; term invertida = stress inminente.

## Comandos

```bash
# Julia engine (desarrollo)
cd zeta && julia --project=. src/server.jl

# Rust executor (desarrollo)
cd executor && RUST_LOG=debug cargo run

# Deploy en servidor
sudo bash deploy/setup.sh
systemctl start zeta.target

# Status
bash deploy/scripts/status.sh
journalctl -fu zeta-engine
journalctl -fu zeta-executor

# Ver OMS log
tail -f /var/log/trading/oms.jsonl | python3 -m json.tool
```

## Variables de entorno requeridas

Ver `.env.example`. Mínimo requerido para arrancar:
- `THETADATA_API_KEY`
- `DATABENTO_API_KEY`
- `ANTHROPIC_API_KEY`
- `ALPACA_KEY_ID` + `ALPACA_SECRET_KEY`

## Decisiones de arquitectura

Ver `docs/adr/` para el razonamiento detrás de cada decisión principal:
- [ADR-0001](docs/adr/0001-julia-rust-stack.md) — Por qué Julia + Rust
- [ADR-0002](docs/adr/0002-zeromq-ipc.md) — ZeroMQ vs HTTP vs shared memory
- [ADR-0003](docs/adr/0003-thetadata-databento-split.md) — Split de fuentes de datos
- [ADR-0004](docs/adr/0004-zeta-field-heteroscedasticity.md) — El Zeta Field concept
- [ADR-0005](docs/adr/0005-single-server-systemd.md) — Por qué no K8s ni Docker
- [ADR-0006](docs/adr/0006-anthropic-tool-use-agent.md) — LLM agent en Rust
- [ADR-0007](docs/adr/0007-hybrid-rule-engine-llm.md) — Híbrido rule engine + LLM
- [ADR-0008](docs/adr/0008-svi-vol-surface.md) — SVI para vol surface

## SOC2 — estado actual

| Control | Estado | Notas |
|---------|--------|-------|
| Audit trail (Processing Integrity) | ✓ Implementado | OMS append-only event log |
| Secrets fuera del código | ✓ Implementado | `.env` separado, no en repo |
| Principle of least privilege | ✓ Parcial | Usuario `trading` dedicado |
| Encriptación en reposo | ✗ Pendiente | `oms.jsonl` en texto plano |
| Encriptación en tránsito | ✓ Parcial | HTTPS a APIs externas; ZMQ ipc:// local OK |
| Incident response documentado | ✗ Pendiente | Solo `on_executor_stop.sh` |
| Change management | ✗ Pendiente | Sin PR review requerido para deploy |
| Vendor assessment | ✗ Pendiente | Anthropic, ThetaData, Databento, Alpaca |
| Vulnerability scanning | ✗ Pendiente | `cargo audit`, `julia pkg audit` |
