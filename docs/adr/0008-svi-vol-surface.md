# ADR-0008: SVI para vol surface fitting

**Estado:** Aceptado  
**Fecha:** 2026-05-24

## Contexto

Necesitamos una parametrización de la superficie de volatilidad implícita que sea:
- Arbitrage-free (no calendario ni mariposa negativa)
- Interpolable para strikes/expirations no observados
- Computacionalmente eficiente para fitting en tiempo real
- Capaz de capturar skew, wings, y ATM vol simultáneamente

Opciones: SVI (Gatheral), SABR, Heston, cubic spline, polynomial interpolation.

## Decisión

**SVI (Stochastic Volatility Inspired)** de Jim Gatheral (2004) para fitting de cada slice de expiración.

`w(k) = a + b*(ρ*(k-m) + √((k-m)²+σ²))`

donde `k = ln(K/F)` (log-moneyness) y `w = σ²_imp * T` (varianza total implícita).

## Por qué

**vs SABR:**  
SABR tiene 4 parámetros (α, β, ρ, ν) y es estándar en tasas de interés pero produce smiles que no siempre son arbitrage-free. SVI tiene condiciones necesarias y suficientes para ausencia de arbitrage de mariposa bien documentadas.

**vs Heston:**  
Heston es un modelo de volatilidad estocástica completo — ideal para pricing y hedging con el modelo. Para fitting de superficie observada, SVI es más flexible y más rápido de calibrar.

**vs splines/polinomios:**  
No tienen interpretación financiera de los parámetros. SVI tiene: `a` = nivel ATM, `b` = pendiente de alas, `ρ` = skew (asimetría), `m` = desplazamiento ATM, `σ` = curvatura.

**SVI gana porque:**  
- 5 parámetros interpretables financieramente → diagnóstico fácil si el fit falla
- Fitting vía Nelder-Mead en Optim.jl — rápido, sin derivadas
- Los 5 parámetros se pueden interpolar entre expirations para superficie continua
- `svi_variance(p, k)` es una función simple → evaluación rápida para smile_metrics

## Consecuencias

- El fitting puede fallar con pocos strikes (<5) → `fit_svi()` requiere mínimo 5 puntos
- Los pesos por inverso del bid-ask spread mejoran el fit pero requieren datos de bid/ask
  → Si solo hay IV mid, usar pesos uniformes
- No implementado: condición de arbitrage de calendario entre slices (total variance monotone in T)
  → **Deuda técnica**: añadir verificación cross-slice antes de construir `VolSurface`
- Interpolación entre slices: lineal en varianza total (no en vol) para evitar arbitrage de calendario
  → Implementado en `surface_vol()`
- `delta25_strike()` usa Newton-Raphson sobre delta → puede no converger en condiciones extremas
  → **Deuda técnica**: añadir fallback y log de convergencia
