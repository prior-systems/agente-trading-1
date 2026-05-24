# ADR-0002: ZeroMQ PUSH/PULL para IPC Julia→Rust

**Estado:** Aceptado  
**Fecha:** 2026-05-24

## Contexto

Julia computa el Zeta Field cada ~60 segundos. El resultado (ZetaState + StrategyProposal) necesita llegar al Rust executor para tomar decisiones de ejecución.

Opciones evaluadas: HTTP/REST, Unix Domain Socket, ZeroMQ, NATS, Apache Arrow Flight, shared memory (mmap), RabbitMQ, Kafka.

## Decisión

ZeroMQ PUSH/PULL con transport `ipc:///tmp/zeta.sock`.  
Rust **bind** (PULL, endpoint estable).  
Julia **connect** (PUSH, se conecta al endpoint).

## Por qué

**vs HTTP en Julia:**  
HTTP requiere correr un servidor HTTP en Julia — añade dependencia, complejidad de lifecycle, y request/response overhead para lo que es un flujo unidireccional.

**vs Unix socket raw:**  
ZMQ da buffering, reconexión automática y high-water mark de forma gratuita. Un socket raw requiere implementar framing, reconexión y backpressure manualmente.

**vs RabbitMQ / Kafka:**  
Son brokers externos — requieren un tercer proceso siempre corriendo. Para IPC en la misma máquina con 1-5 mensajes/minuto es arquitectura sobredimensionada.

**vs shared memory:**  
Requiere sincronización explícita (futex/semaphore). Justificado solo si sincronizamos en cada tick. El field se computa en minutos, no microsegundos.

**vs NATS:**  
NATS es la siguiente opción si necesitamos múltiples consumidores o múltiples nodos. La migración desde ZMQ es cambio de transporte, no de lógica de mensajes.

**ZMQ gana porque:**  
- Zero infraestructura extra en el servidor
- `ipc://` = Unix socket por debajo = performance máximo en misma máquina
- SNDHWM=100, SNDTIMEO=100ms → Julia nunca se bloquea esperando a Rust
- Si Rust no está listo, el mensaje se descarta y el siguiente ciclo de 60s envía uno nuevo (aceptable)

## Consecuencias

- Mensajes no son persistentes — si executor cae entre dos signals, se pierde la señal
  → Aceptable: el siguiente ciclo envía una nueva. No hay "señal única irrecuperable".
- Si en el futuro hay múltiples consumidores del ZetaState (monitor, backtester):
  → Cambiar a PUB/SUB en ZMQ o migrar a NATS. Cambio localizado en `ipc.jl` e `ipc.rs`.
- `PrivateTmp=false` en el systemd del executor para acceder a `/tmp/zeta.sock`
