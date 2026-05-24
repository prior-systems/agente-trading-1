# ADR-0006: LLM agent en Rust con Anthropic tool_use forzado

**Estado:** Aceptado  
**Fecha:** 2026-05-24

## Contexto

El rule engine en Julia genera estrategias claras para el 80% de los casos. El 20% restante tiene señales ambiguas (campo inestable + VRP fuerte, skew extremo sin VRP, eventos macro no modelados). Esos casos necesitan razonamiento contextual.

Opciones para el agente: Python (LangChain, Swarms), Julia llamando API, Rust llamando API directamente.

## Decisión

Agente LLM implementado en **Rust** llamando directamente a la **Anthropic Messages API** con `tool_choice: forced` para forzar output estructurado.

Modelo: `claude-opus-4-7`

## Por qué

**Rust en lugar de Python:**  
La API de Anthropic es HTTP + JSON. No hay nada en Python que haga eso mejor — solo hay frameworks encima que en este caso no aportan nada. El agente necesita: recibir ZetaState context → llamar API → parsear JSON → pasar al OMS. Los tres pasos los hace Rust mejor (tipado en compile time con serde, sin IPC adicional al OMS que ya está en Rust).

**tool_use forzado (`tool_choice: forced`):**  
Sin tool forcing, el modelo puede responder con texto libre que requiere parsing frágil. Con `tool_choice: {"type": "tool", "name": "submit_decision"}` el modelo DEBE llamar la herramienta con JSON válido o la llamada falla. El schema de `StrategyDecision` se valida en serde al parsear — si el modelo alucina un campo, `serde_json::from_value` retorna error.

**El agente NO tiene autonomía total:**  
- Hard limits (max delta, max vega, max loss/trade) los impone Julia siempre
- El agente solo puede: aprobar, cambiar sizing (`sizing_adjustment`), cambiar urgency, bloquear
- No puede cambiar los hard constraints ni inventar estrategias fuera del enum `StrategyType`
- Si `passes_limits=false` en la propuesta de Julia, el agente no puede aprobar igualmente

**Fast path sin LLM:**  
`needs_llm=false` → el executor construye el `StrategyDecision` directamente desde la propuesta de Julia sin llamar a Anthropic. Zero costo de API para el 80% de señales claras.

## Consecuencias

- Costo de API solo para señales ambiguas → estimar ~10-50 calls/día en operación normal
- Latencia de Anthropic API (~1-3s) en el slow path — aceptable para opciones (no HFT)
- Token usage logueado en cada llamada para tracking de costos
- El prompt del sistema tiene constraints explícitos — si el modelo los ignora, `serde` lo detecta
- Modelo actual: `claude-opus-4-7`. Si se depreca, cambiar la constante `MODEL` en `anthropic.rs`
