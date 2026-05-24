# ADR-0005: Un servidor + systemd (no Docker, no Kubernetes)

**Estado:** Aceptado  
**Fecha:** 2026-05-24

## Contexto

Necesitamos infraestructura para correr Julia + Rust continuamente, con supervision de procesos, logging, y recovery automático.

Opciones evaluadas: Kubernetes, Docker Compose, Docker solo, bare metal + systemd.

## Decisión

Un servidor dedicado (Hetzner o AWS) con systemd para supervisión de procesos.  
No Docker. No Kubernetes.

## Por qué

**Kubernetes es incorrecto para este sistema:**  
K8s está diseñado para workloads stateless con horizontal scaling. El OMS tiene estado en memoria (posiciones, órdenes abiertas). Un pod restart sin reconciliación con el broker es un riesgo real de trading — posiciones abiertas sin estado local. K8s añade latencia de red, complejidad operacional masiva, y no aporta nada que systemd no haga mejor aquí.

**Docker Compose añade complejidad sin beneficio:**  
Elegimos ZMQ con `ipc://` (Unix socket) por ser el transporte más rápido. Docker pone los contenedores en network namespaces distintos — los Unix sockets no cruzan ese boundary. Con TCP `127.0.0.1:5555` funcionaría pero añade overhead. El beneficio de Docker (reproducibilidad del entorno) se logra igual con `setup.sh` + versiones fijadas.

**systemd hace exactamente lo necesario:**  
- `Restart=on-failure` + `RestartSec` → recovery automático
- `StartLimitBurst` → no reinicia infinitamente si hay bug
- `ExecStopPost` → hook al morir para alertas y log de shutdown
- `EnvironmentFile` → secrets en `/opt/trading/.env`, no en el unit file
- `journald` → logs centralizados sin configuración adicional
- `zeta.target` → start/stop atómico de ambos servicios

## Cuándo reconsiderar

- **Docker Compose:** cuando necesites múltiples ambientes idénticos (paper/live) o CI que testee el sistema completo. Cambio: ZMQ sobre TCP en lugar de ipc://.
- **NATS en lugar de ZMQ:** cuando haya múltiples instancias del executor o múltiples consumidores del ZetaState.
- **K8s:** probablemente nunca para este sistema. Si escala a ese punto, el sistema habrá cambiado fundamentalmente.

## Consecuencias

- Deploy manual via `setup.sh` — no hay CI/CD automático todavía
- Servidor único = single point of failure — aceptable para paper trading, revisar para producción con dinero real
- Logs en journald — exportar a servicio externo (Grafana Loki, Datadog) para retención larga
- `PrivateTmp=false` en executor — necesario para acceder a `/tmp/zeta.sock`
