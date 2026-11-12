# P029 — Outbox y eventos durables antes de watchers y procesos

Estado de este incremento: **BLOCKED_REAL**.

## Implementado

- Cada mutación confirma el cambio de dominio, el evento y su fila de outbox en una transacción SQLite. El dispatcher publica en orden y confirma después; si falla el ack o se reinicia el proceso, reenvía el mismo `event_id`.
- `GET /api/v1/events` acepta `Last-Event-ID` decimal, valida el cursor y transmite el replay antes de los eventos nuevos. Se crea la suscripción en vivo antes de leer SQLite y se filtran los IDs cubiertos por el high-water mark, evitando huecos y duplicados en la frontera entre replay y bus.
- El replay respeta los 30 días y el máximo configurable de 10.000 eventos. Un cursor expirado o que supera el límite devuelve `410 EVENT_CURSOR_EXPIRED` antes de abrir SSE. La secuencia de IDs permanece durable aunque se poden todas las filas antiguas.
- La poda se ejecuta al arrancar el daemon y cada 24 horas. Elimina eventos ya entregados fuera de retención y conserva las filas con despacho pendiente. La configuración rechaza `replay_limit` fuera de `1..=10000`.
- Se conserva el envelope y los campos SSE existentes; no se cambió el esquema persistido ni los contratos canónicos.

## Pruebas y resultados

El recibo [P029](P029_outbox-y-eventos-durables-antes-de-watchers-y-procesos.evidence.json) contiene **9 PASS, 0 FAIL y 0 BLOCKED**. El gate final `tools/check.ps1 -Prompt P029 -Real` tuvo **13/14 checks PASS**; el único fallo es `package_copy`, que señala diferencias en `docs/contracts/adapter-methods.json`, `docs/contracts/api-operations.json` y `docs/contracts/schemas/larm.schema.json`. El detalle está en [gate P029](check_P029_real.json).

- SQLite: 7/7 pruebas para atomicidad/rollback, ack idempotente, replay ordenado, high-water mark durable, cursor expirado, límite y poda que conserva outbox pendiente.
- Dispatcher: 3/3 pruebas para espera del suscriptor, reintento tras fallo de ack y proyección SQLite al envelope SSE.
- API: 12/12 pruebas para autorización, replay, deduplicación en la carrera replay/bus, 410 antes de abrir stream, cursor inválido/futuro, cola lenta y apagado.
- Formato, Clippy de todos los targets y `cargo test --workspace`: PASS. También pasan build del daemon, arquitectura, formato/Clippy/tests de Tauri, lint/typecheck/tests/build del frontend.
- Prueba REAL con el proceso daemon: se reinició mientras había 39 escrituras en vuelo; después del reinicio se reprodujeron las 2 filas durables confirmadas sin IDs duplicados y una escritura posterior se observó en vivo sin duplicar. La limpieza del fixture pasó. Pasos completos en [la corrida REAL](P029_runs/real/20261022T051020103384Z/steps.json).

## Bloqueos y compatibilidad

El contrato 07 pide `snapshot_url` cuando expira un cursor. El schema canónico de `Error` prohíbe propiedades adicionales y `api-operations.json` no define una ruta para obtener esa instantánea. La respuesta 410 respeta el schema y comunica que el cliente debe resincronizar, pero no puede incluir una URL enrutable hasta que se aclare ese contrato.

P004 y P005 siguen `BLOCKED_REAL`; el contrato 00 requiere que las dependencias alcancen `REAL_PASS` antes de cerrar P029. Por esos tres bloqueos, P029 permanece `BLOCKED_REAL` aunque el recibo no tenga pruebas fallidas. No se usaron los tokens externos proporcionados.
