# Sistema — Arquitectura y Flujo de Datos

## 1. Componentes del sistema

```mermaid
graph TD
    subgraph EXT["Fuentes externas"]
        TD_REST["ThetaData REST\nchain snapshot · Greeks · IV"]
        TD_WS["ThetaData WS\ncotizaciones en vivo"]
        DB_WS["Databento GLBX.MDP3\nCME futures MBO/trades"]
        ANTHROPIC["Anthropic API\nclaude-opus-4-7"]
        ALPACA["Alpaca Markets\npaper / live broker"]
    end

    subgraph JULIA["zeta-engine.service — Julia"]
        direction TB
        CHAIN["data/chain.jl\nparse_chain_snapshot()\nStrikeCandidate[]"]
        B76["greeks/black76.jl\nBlack-76 · Greeks 1/2/3 orden\nIV solver Newton-Raphson"]
        SVI["vol/surface.jl\nSVI fitting Nelder-Mead\nSmileMetrics · TermStructure"]
        HV_MOD["vol/hv.jl\nrolling HV · GARCH(1,1)\nHurst · VRP z-score"]
        HMM["regime/detector.jl\nHMM Baum-Welch\nP(low|normal|stress)"]
        GEO["field/geometry.jl\nZetaState 30+ dims\nzeta_context_string()"]
        RULES["strategy/\nclassifier → rules → sizing\nFractional Kelly · hard limits"]
        IPC_J["ipc.jl\nZMQ PUSH\nJSON ZetaSignal"]

        CHAIN --> GEO
        B76 --> GEO
        SVI --> GEO
        HV_MOD --> GEO
        HMM --> GEO
        GEO --> RULES
        RULES --> IPC_J
    end

    subgraph ZMQ_IPC["ZeroMQ ipc:///tmp/zeta.sock"]
        ZMQ["PUSH → PULL\nZetaSignal JSON\nchain_candidates incluidos"]
    end

    subgraph RUST["zeta-executor.service — Rust"]
        direction TB
        IPC_R["ipc.rs\nZMQ PULL · ZetaSignal deserialize"]
        AGENT["agent/\nAnthropicClient\ntool_use forzado"]
        EXEC["execution/mod.rs\nbuild_order()\nclosest_delta() · mid limit"]
        OMS["orders/mod.rs\nOMS DashMap\nportfolio Greeks live"]
        PERSIST["orders/persistence.rs\nEventLog append-only\noms.jsonl · replay/reconstruct"]
        BROKER["broker/alpaca.rs\nMLO multi-leg orders\npaper + live"]
        TD_FEED["data/thetadata.rs\nWS feed → MarketEvent"]
        DB_FEED["data/databento.rs\nWS feed → OFI buffer"]

        IPC_R --> AGENT
        IPC_R --> EXEC
        AGENT --> EXEC
        EXEC --> OMS
        OMS --> PERSIST
        OMS --> BROKER
        TD_FEED --> OMS
        DB_FEED --> OMS
    end

    TD_REST -->|HTTPS REST · 60s| JULIA
    TD_WS   -->|WSS| TD_FEED
    DB_WS   -->|WSS| DB_FEED
    IPC_J   --> ZMQ
    ZMQ     --> IPC_R
    AGENT   -->|HTTPS| ANTHROPIC
    BROKER  -->|HTTPS| ALPACA
```

---

## 2. Flujo completo de una señal

```mermaid
sequenceDiagram
    autonumber
    participant TD  as ThetaData REST
    participant JE  as Julia Engine
    participant ZMQ as ZeroMQ IPC
    participant RE  as Rust Executor
    participant ANT as Anthropic API
    participant OMS as OMS DashMap
    participant ALP as Alpaca Markets

    Note over JE: cada 60 segundos por símbolo

    JE  ->> TD  : fetch_option_chain(root, today)
    TD -->> JE  : chain snapshot (strikes × expirations)

    JE  ->> JE  : parse_chain_snapshot() → StrikeCandidate[]
    JE  ->> JE  : _build_slices_from_chain() → SmileSlice[]
    JE  ->> JE  : fit_svi() por expiration → VolSurface
    JE  ->> JE  : garch11_fit() → σ²_cond · VRP z-score
    JE  ->> JE  : current_regime() → P(low|normal|stress)
    JE  ->> JE  : build_zeta_state() → ZetaState (30+ dims)
    JE  ->> JE  : run_rule_engine() → StrategyProposal

    JE  ->> ZMQ : PUSH ZetaSignal<br/>(zeta_context + proposal + chain_candidates)
    ZMQ ->> RE  : PULL ZetaSignal

    alt needs_llm = false — señal clara (80% de los casos)
        RE  ->> RE  : echo proposal → StrategyDecision<br/>confidence = 0.85, sin llamada API
    else needs_llm = true — ambigüedad detectada (20%)
        RE  ->> ANT : Messages API<br/>tool_choice: forced → submit_decision
        ANT -->> RE : StrategyDecision JSON validado por serde
    end

    Note over RE: si approved=false o contracts=0 → log y abort

    RE  ->> RE  : apply sizing_adjustment (LLM) → final_contracts
    RE  ->> RE  : execution::build_order()<br/>closest_delta() · liquidity filters<br/>→ OrderLeg[] con OCC symbols
    RE  ->> OMS : submit(order) → staged en DashMap
    RE  ->> ALP : AlpacaBroker::submit_order() [próximo paso]
    ALP -->> RE : broker_id
    RE  ->> OMS : update status → Submitted
    RE  ->> OMS : EventLog.append(ev_submitted)

    Note over OMS: fills llegan vía WS feed
    ALP -->> RE : fill event (WS)
    RE  ->> OMS : on_equity_event() → update Greeks live
    RE  ->> OMS : EventLog.append(ev_filled)
```

---

## 3. Infraestructura de despliegue

```mermaid
graph TB
    subgraph SRV["Servidor único — Ubuntu 22.04"]
        subgraph TARGET["systemd zeta.target\n(ambos servicios como unidad atómica)"]
            ENG["zeta-engine.service\nUsuario: trading\nJulia 1.11 --threads=4 --heap-size-hint=4G\nRestarts: 3 en 300s"]
            EXE["zeta-executor.service\nUsuario: trading\nRust release binary\nTimeoutStop: 30s"]
        end

        SOCK["Unix socket\nipc:///tmp/zeta.sock\n(PrivateTmp=false para acceso compartido)"]
        LOGDIR["/var/log/trading/oms.jsonl\nappend-only · logrotate 7d"]
        ENV[".env — secrets\nno en repositorio"]
        SETUP["deploy/setup.sh\nbootstrap: Julia · Rust · usuario · logrotate\npre-compila paquetes Julia"]
        STOP["on_executor_stop.sh\nlog evento stop → oms.jsonl\nTelegram alert si configurado"]

        ENG  -->|"ZMQ PUSH"| SOCK
        SOCK -->|"ZMQ PULL"| EXE
        EXE  --> LOGDIR
        ENG  -. lee .-> ENV
        EXE  -. lee .-> ENV
        EXE  -. ExecStopPost .-> STOP
    end

    subgraph EXT["APIs externas — HTTPS/WSS"]
        TD["ThetaData\nREST + WS"]
        DB["Databento\nGLBX.MDP3 WSS"]
        ANT["Anthropic\n~10-50 calls/día\n(solo señales ambiguas)"]
        ALP["Alpaca Markets\npaper → live"]
    end

    ENG -->|"HTTPS REST · 60s"| TD
    EXE -->|"WSS"| TD
    EXE -->|"WSS"| DB
    EXE -->|"HTTPS · ~1-3s latencia"| ANT
    EXE -->|"HTTPS"| ALP

    subgraph PENDING["Pendiente"]
        PROM["Prometheus\nexport métricas OMS"]
        GRAF["Grafana\ndashboard P&L · Greeks · régimen"]
    end

    EXE -.->|"pendiente"| PROM
    PROM -.-> GRAF
```

---

## 4. ZetaState — anatomía del campo (30+ dimensiones)

```mermaid
graph LR
    subgraph VOL["Capa de volatilidad"]
        V1["atm_iv\nIV implícita ATM 30d"]
        V2["hv_5d · hv_21d · hv_63d\nHV histórica rolling"]
        V3["garch_vol\nσ² condicional GARCH(1,1)"]
        V4["vrp · vrp_zscore\nIV² − HV²_realizada"]
        V5["iv_percentile\n0-100 vs historia 1 año"]
    end

    subgraph SMILE["Capa de smile"]
        S1["skew_25d\nRR25 = vol_25c − vol_25p\nnegativo = miedo bajista"]
        S2["butterfly_25d\nBF25 = (25c+25p)/2 − ATM\nalto = colas caras"]
        S3["atm_skew · atm_convexity\ndσ/dk · d²σ/dk² en ATM"]
        S4["term_slope\n(IV_60d − IV_30d)\nshape: normal|inverted|humped"]
        S5["implied_skewness\nimplied_kurtosis\nmomentos de la distribución implícita"]
    end

    subgraph GREEKS["Capa de Greeks del portfolio"]
        G1["portfolio_delta · gamma\nexposición neta a precio y convexidad"]
        G2["portfolio_theta · vega\ndecaimiento y exposición a vol"]
        G3["portfolio_vanna · charm\nGreeks de 2° orden cruzados"]
    end

    subgraph REGIME["Capa de régimen"]
        R1["regime_probs\nP(low_vol) · P(normal) · P(stress)"]
        R2["regime_entropy\nShannon entropy — incertidumbre"]
        R3["hurst\nH≈0.1 = rough vol (anti-persistente)\nH≈0.5 = random walk"]
    end

    subgraph FLOW["Capa de flujo de órdenes"]
        F1["ofi\n(buy_vol − sell_vol)/(total)\nDatabento MBO"]
        F2["cancel_ratio\ncancelaciones/total órdenes"]
        F3["bid_ask_vol\ndesequilibrio bid vs ask"]
    end

    subgraph MOM["Capa de momentum"]
        M1["price_momentum_5d · 21d\nretorno log normalizado"]
        M2["vol_momentum\nvelocidad de cambio de IV"]
    end

    subgraph GEO["Geometría del campo"]
        C1["curvature\nd²ZetaState/dt² — aceleración del campo"]
        C2["tension\n‖∇ZetaState‖ — gradiente total"]
    end

    VOL --> ZETA
    SMILE --> ZETA
    GREEKS --> ZETA
    REGIME --> ZETA
    FLOW --> ZETA
    MOM --> ZETA
    GEO --> ZETA

    ZETA(["ZetaState\n30+ dims"])

    ZETA --> RE["Rule Engine\nclassifier → rules → sizing\n→ StrategyProposal"]
    ZETA --> LLM["LLM context\nzeta_context_string()\n→ Anthropic prompt"]
```

---

## 5. Árbol de decisión del rule engine

```mermaid
flowchart TD
    ZS["ZetaState recibido"]
    ZS --> UNSTABLE{field == Unstable?}
    UNSTABLE -->|sí| DN["DoNothing\n(hard stop)"]
    UNSTABLE -->|no| LIMITS{passes_limits?}
    LIMITS -->|no| BLOCKED["Bloqueado\npor hard limits\nlog + abort"]
    LIMITS -->|sí| AMB{needs_llm?}

    AMB -->|"no — campo claro (80%)"| FAST["Fast path\nStrategyDecision directo\nsin llamada API"]
    AMB -->|"sí — ambigüedad (20%)"| SLOW["Slow path\nAnthropic API\ntool_use forzado"]

    FAST --> BUILD
    SLOW --> BUILD

    BUILD["execution::build_order()\nclosest_delta() por leg\nfiltros: spread≤25% · bid≥$0.05 · OI≥100\nmid limit price"]

    BUILD --> OMS["OMS.submit()\n→ staged en DashMap"]
    OMS --> BROKER["AlpacaBroker::submit_order()\n[próximo paso]"]

    subgraph PATTERNS["6 patrones que activan needs_llm"]
        P1["VRP fuerte + campo inestable"]
        P2["Skew extremo sin VRP support"]
        P3["Término invertido + IV baja"]
        P4["HMM entropy > 0.70"]
        P5["Hurst < 0.25 + señal venta vol"]
        P6["Momentum contradice skew"]
    end

    AMB -.->|detecta| PATTERNS
```
