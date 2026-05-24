# ADR-0003: ThetaData para equity options, Databento para CME futures

**Estado:** Aceptado  
**Fecha:** 2026-05-24

## Contexto

Necesitamos datos de mercado para dos clases de activos:
1. Opciones sobre acciones e índices (SPX, SPY, QQQ, etc.)
2. Futuros y opciones sobre futuros de CME (ES, NQ, etc.)

## Decisión

**ThetaData** para equity options e índices.  
**Databento GLBX.MDP3** para CME futures y options on futures.

## Por qué

**ThetaData:**
- Greeks pre-calculados hasta 3rd order (delta, gamma, theta, vega, rho, vanna, charm, vomma, veta, speed, zomma, color, ultima)
- Calculados en cada tick con el precio exacto del subyacente en ese momento
- `quote_greeks` (sobre bid/ask midpoint) y `trade_greeks` (en cada trade)
- Cubre SPX, VIX, RUT, 1000+ índices con datos históricos
- WebSocket streaming para tiempo real
- Alternativa (yfinance) no tiene Greeks, es batch/delayed, no sirve

**Databento GLBX.MDP3:**
- Fuente oficial del CME feed (MDP 3.0)
- MBO (Market by Order): cada orden individual en nanosegundos → order flow imbalance
- Greeks NO vienen del feed → calculamos con Black-76 en Julia
- `definition` schema: strike, expiration, multiplier, tick size de cada contrato
- `statistics` schema: settlement price, open interest para basis y roll analysis
- Única fuente con full order book (MBP-10) para microestructura de futuros

## Consecuencias

- Para equity options: Julia no calcula Greeks → los ingiere de ThetaData
  Julia construye geometría sobre esos Greeks (vol surface, skew, VRP)
- Para CME options: Julia calcula Greeks con Black-76 (`greeks/black76.jl`)
  Requiere implied vol solver (Newton-Raphson implementado)
- Dos APIs, dos autenticaciones, dos formatos de respuesta
- `_build_slices_from_chain()` en `server.jl` usa índices posicionales del response de ThetaData
  → **Deuda técnica**: frágil si ThetaData cambia el orden de campos. Migrar a named fields.
- Databento no tiene Greeks calculados nativos — hay feature request en su roadmap
  Si lo implementan: eliminar el Black-76 path y usar el feed directo
