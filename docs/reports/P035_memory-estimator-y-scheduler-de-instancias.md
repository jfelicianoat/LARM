# P035 — Estimador de memoria y scheduler de instancias

**Estado:** `BLOCKED_REAL` · **Fecha:** 2026-10-22

P035 implementa el cálculo de memoria por pool, el preflight de admisión y las decisiones del scheduler. La fase queda `BLOCKED_REAL` porque el daemon todavía no usa esta reserva en el flujo productivo desde un `LaunchProfile` hasta una `RuntimeInstance` con lease; además, P011, P019, P033 y P034 no acreditan ese camino en `REAL_PASS`.

## Implementación

- El estimador transformer calcula KV con aritmética comprobada y redondeo hacia arriba, suma pesos, activaciones, workspace, overhead, memoria multimedia y temporales, y aplica el margen de seguridad del contrato 04. Metadata insuficiente y modelos multimedia sin estimador de adapter producen `UNKNOWN`.
- El preflight deduplica pools de memoria unificada, descuenta las reservas del sistema y de otras instancias, combina límites de perfil, lease y presupuesto duro por el menor valor, y rechaza `UNKNOWN` en admisión automática. Los fallos usan `LOAD_OOM_RISK` y conservan alternativas para el operador.
- La admisión toma guards P019 para variante, artefactos y pools, y guarda la reserva de todos los pools en una sola transacción SQLite. El reemplazo multi-pool es atómico; reservar un pool individual conserva las otras reservas del titular.
- El scheduler aplica las prioridades PINNED/HIGH/NORMAL/BACKGROUND, desempata la cola de inicio por FIFO y limita los arranques simultáneos. La selección de descarga respeta leases activos, instancias PINNED, runtimes warm y procesos externos; implementa NEVER, IDLE_TIMEOUT de 300 s, MEMORY_PRESSURE y LRU.

## Verificación

La evidencia formal registra **7 PASS, 0 FAIL y 1 REAL BLOCKED**, con estado `BLOCKED_REAL`. Pasan el estimador y preflight, la política del scheduler, la reserva SQLite multi-pool atómica, la compatibilidad de reserva individual, la admisión integrada con guards P019, una prueba Windows que observa la RAM del sistema y la usa en el preflight, y la excepción persistida para `UNKNOWN` con override y presupuesto duro por pool explícitos. Esa última prueba confirma que el modo automático no deja ninguna reserva y que el override sólo persiste los bytes acotados.

`tools/check.ps1 -Prompt P035 -Real` pasó **13/14 comprobaciones** en el contexto local: formato, Clippy del workspace, fixtures, tests completos del workspace, arquitectura, shell Tauri y los cuatro pasos del frontend. Sólo falla `package_copy` porque las copias locales de `adapter-methods.json`, `api-operations.json` y `schemas/larm.schema.json` siguen divergentes del paquete canónico, deuda registrada desde P031/P099. En el sandbox, tres tests declarativos con probes HTTP a loopback terminaban en `health_unreachable`; al ejecutar el mismo gate con acceso local a esos procesos y endpoints, `cargo_test` pasó.

La prueba real de P035 acredita lectura de memoria Windows y una decisión de admisión basada en ella; no acredita el arranque de un modelo gestionado. El runner bloquea explícitamente ese caso hasta integrar el flujo productivo.

## Pendientes

- Conectar el estimador, preflight y reserva de pools al flujo `LaunchProfile → LaunchPlan → RuntimeInstance/lease`, manteniendo la reserva hasta confirmar el cierre del proceso.
- Alcanzar `REAL_PASS` para P011, P019, P033 y P034 antes de cualificar este consumidor.
- Reconciliar las copias contractuales locales con el paquete canónico y repetir el gate para obtener 14/14.

La evidencia y el recibo del gate están en [evidencia P035](P035_memory-estimator-y-scheduler-de-instancias.evidence.json) y [gate P035](check_P035_real.json).
