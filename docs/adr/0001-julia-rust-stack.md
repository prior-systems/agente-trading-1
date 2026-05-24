# ADR-0001: Julia + Rust como stack principal

**Estado:** Aceptado  
**Fecha:** 2026-05-24

## Contexto

Sistema de trading autónomo para opciones y futuros. Requiere:
- Matemáticas de alta precisión (vol surface, GARCH, HMM, Greeks)
- Ejecución en tiempo real con WebSocket a múltiples feeds
- Latencia baja pero no HFT (opciones tienen spreads de cents, no microsegundos)
- Stack sin GC pauses en el path crítico de ejecución

Opciones evaluadas: Python, Python+Rust, Julia+Rust, C++.

## Decisión

**Julia** para el Zeta Field Engine (cálculo, matemáticas, señales).  
**Rust** para el Execution Engine (datos en tiempo real, OMS, broker, agente LLM).

## Por qué

**Julia vs Python para el engine:**
- Julia es compilado JIT, performance comparable a C para álgebra lineal
- Ecosistema científico nativo: `Distributions.jl`, `Optim.jl`, `Statistics.jl`
- Sin GIL — paralelismo real con `@threads`
- GARCH, HMM, SVI fitting son código matemático puro — Julia gana claramente
- Python añadiría NumPy/SciPy como dependencias externas para lo mismo

**Rust para ejecución:**
- Zero-cost abstractions — sin overhead en el path de órdenes
- Sin GC — no hay pauses inesperados cuando el mercado se mueve
- `tokio` para async nativo — WebSocket a ThetaData y Databento en paralelo
- `serde` + `reqwest` para la Anthropic API — tipado en compile time
- OMS con `DashMap` — concurrent hashmap sin locks visibles

**vs C++:**
- Rust tiene las mismas garantías de performance con memory safety
- Ecosistema de crates moderno vs dependencias C++ fragmentadas
- `cargo` vs CMake — productividad real

## Consecuencias

- La AI puede no tener el mismo conocimiento de Julia/Rust que de Python → más cuidado en sesiones de AI
- Dos lenguajes requieren IPC explícito (ver ADR-0002)
- Deploy requiere compilar Rust en el servidor y pre-compilar Julia packages
- No hay frameworks de agentes listos para Julia/Rust → construimos los nuestros (ver ADR-0006)
