# ADR-0007: Híbrido rule engine Julia + LLM overlay para estrategia

**Estado:** Aceptado  
**Fecha:** 2026-05-24

## Contexto

Necesitamos seleccionar estrategias de opciones (iron condor, straddle, etc.) y dimensionar posiciones dado el estado del campo. Opciones: rule engine puro, ML model, LLM puro, híbrido.

## Decisión

**Julia rule engine** para el 80% de casos claros (matemáticamente deterministas).  
**LLM overlay** solo cuando el rule engine detecta ambigüedad.

El rule engine tiene **veto permanente** via hard limits — el LLM no puede sobreescribir restricciones de riesgo.

## Por qué

**Rule engine puro es insuficiente porque:**  
Los mercados tienen contexto que no cabe en el ZetaState numérico: FOMC en 3 días, earnings sorpresa, crisis geopolítica intraday. El rule engine no sabe qué es el FOMC.

**LLM puro es peligroso porque:**  
Los LLMs pueden alucinar precios, inventar narrativas, y no tienen acceso al estado real del portfolio. Sin constraints matemáticos, pueden proponer posiciones que violan límites de riesgo.

**El híbrido resuelve ambos:**  
- El campo matemático (GARCH, HMM, VRP, smile) es determinista y confiable → rule engine
- El contexto narrativo/macro es exactamente donde los LLMs añaden valor → LLM overlay
- Los hard limits son invariantes matemáticos → Julia los impone siempre, antes y después del LLM

**La ambigüedad se detecta automáticamente:**  
El classifier detecta 6 patrones de conflicto que activan `needs_llm=true`:
1. VRP fuerte pero campo inestable
2. Skew extremo sin VRP support
3. Término invertido con IV baja
4. Régimen HMM near-uniform (entropy > 0.70)
5. Rough vol (Hurst < 0.25) + señal de venta de vol
6. Momentum contradice skew

**Fractional Kelly sizing:**  
El tamaño no es fijo — escala con la fuerza de la señal (VRP z-score → win probability → Kelly fraction × 0.25). El LLM puede aplicar un `sizing_adjustment` multiplicativo pero dentro del máximo calculado por Julia.

## Consecuencias

- El rule engine necesita mantenimiento cuando cambian las condiciones de mercado
  (ej: umbral VRP_zscore > 1.5 puede necesitar calibración por régimen)
- Las preguntas al LLM son específicas, no abiertas → reduce alucinaciones
- Si el LLM rechaza una señal clara, hay que revisar si el prompt es demasiado restrictivo
- El sizing basado en Kelly fraccionario asume que `_zscore_to_win_prob()` es una calibración razonable
  → **Deuda técnica**: calibrar con datos históricos reales de VRP vs retornos realizados
