# ADR-0004: Zeta Field — modelar geometría de mercado, no precio

**Estado:** Aceptado  
**Fecha:** 2026-05-24

## Contexto

Los sistemas de trading tradicionales (incluyendo AutoHedge, referencia de este proyecto) tratan las opciones como acciones con más parámetros: predicen dirección de precio y ejecutan. Esto ignora la naturaleza fundamental del mercado de opciones.

El mercado de opciones es heteroscedástico: la varianza no es constante. Tiene clustering de volatilidad, transiciones de régimen, momentum persistence, e interacciones no lineales. Black-Scholes asume varianza constante — el smile existe precisamente porque esa asunción es falsa.

## Decisión

El sistema modela el mercado como un **campo dinámico** (Zeta Field) en lugar de reaccionar a precio. El activo subyacente real es la **volatilidad**, no el precio.

Componentes del campo:
- **IV vs HV realizada** (VRP): edge real en opciones
- **Greeks como blancos móviles**: delta/gamma/theta/vega cambian continuamente
- **El smile como inteligencia**: RR25 = sentimiento direccional; BF25 = pricing de colas; term structure = riesgo de eventos
- **Heterocedasticidad estructurada**: GARCH(1,1) + HMM — régimen como distribución, no estado binario
- **Rugosidad del path**: Hurst exponent — H≈0.1 en vol intraday (rough vol)

## Por qué

**vs predicción de precio:**  
La predicción de dirección tiene un edge mínimo para la mayoría de activos. El edge real en opciones viene de mispricing de volatilidad (VRP) y estructura de la sonrisa.

**vs modelo paramétrico (Heston, SABR):**  
Los modelos paramétricos asumen que conoces la forma de la heterocedasticidad. El Zeta Field no asume forma — la geometría emerge de los datos. SVI es el único punto paramétrico y solo para fitting de superficie, no para el campo.

**Régimen como distribución (HMM), no estado binario:**  
"Estamos en régimen de alta volatilidad" es menos útil que "P(low)=15%, P(normal)=72%, P(stress)=13%". La incertidumbre del régimen es información, no ruido.

## Consecuencias

- El sistema es más complejo de construir que uno puramente direccional
- Requiere datos de vol surface (opciones), no solo precio → ThetaData es necesario
- La métrica de éxito no es "predijo la dirección" sino "capturó el VRP" y "gestionó Greeks dentro de tolerancia"
- Los agentes LLM consumen `zeta_context_string()` — texto estructurado del campo — no precio raw
- Backtesting requiere datos históricos de opciones, no solo OHLCV → más caro y complejo
