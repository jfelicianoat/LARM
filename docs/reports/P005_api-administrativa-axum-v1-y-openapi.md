# P005 — API administrativa Axum v1 y OpenAPI

**Fecha**: 2026-10-11 · **Host**: A9_MEGA (PC IA) · **Estado**: ver [evidence.json](P005_api-administrativa-axum-v1-y-openapi.evidence.json); depende de P003, P004 y P009, que están en BLOCKED_REAL por motivos de entorno. OpenAPI: [docs/api/openapi.json](../api/openapi.json).

## Qué se construyó

- **Aplicación unificada** (`larm-api::app`). Capas, de fuera a dentro:
  1. CORS: desactivado salvo `security.allowed_origins`, y solo con coincidencia exacta, nunca `*`.
  2. Log de cada petición con correlación, sin cuerpos ni cabeceras.
  3. En la parte administrativa: envelope del contrato para los rechazos del framework, límite de cuerpo (`api.request_body_bytes`), timeout (`api.request_timeout_seconds` → `504 TIMEOUT`) y el guard de P009 delante de todas las rutas.
  4. El gateway de inferencia (P107) queda **fuera** del guard y de la normalización.
- `GET /api/v1/health/live` (público) y `GET /api/v1/health/ready` (scope `read`; DB con los pragmas del contrato → READY, si no DEGRADED; inaccesible → 503 `UPSTREAM_UNAVAILABLE`).
- SSE `GET /api/v1/events` (`read`), conectado a un bus real: el daemon publica los eventos persistidos en `event`/`event_outbox` con cada mutación. Heartbeat de `events.heartbeat_seconds`. Una cola de 1024 desbordada envía `resync_required` y cierra. El cierre del bus termina los streams.
- **OpenAPI 3.1 generado** desde `api-operations.json` y los dos schemas (contrato 07: «fuente única… sin inventar otro DTO»), con `x-larm-owner`, `x-larm-scope`, `x-larm-mode` y `x-larm-implemented`. `larm-daemon openapi --out docs/api/openapi.json [--check]`. Lo valida `openapi-spec-validator`.
- **DTO `Search` completo (H05)**: los 18 campos explícitos, rangos decimales como string, `ScoreRange` 0..100, `minimum <= maximum`, listas sin duplicados, `filter_digest` sin `cursor`/`limit` e insensible al orden de las listas, y detección de la contradicción `INCOMPATIBLE` + `show_incompatible=false`. No se afirma búsqueda federada (P044).
- Daemon `--serve-api`: compone la aplicación, el bootstrap y, con `--bridge-config`, el puente.

## Defectos encontrados al probar (corregidos)

- La normalización de errores **reescribía las respuestas del gateway**: un 4xx en texto del servidor de modelos habría llegado al Broker como envelope de LARM. Ahora solo se normaliza la parte administrativa.
- **El daemon no terminaba con un cliente SSE abierto**: el apagado ordenado espera conexiones y el SSE no se cierra nunca. Lo reprodujo la prueba real; ahora el apagado cierra el bus y los streams. Prueba real: exit 0 en < 1 s con SSE conectado.
- Una ruta fuera del catálogo no pasaba por el guard (las capas de Axum solo envuelven rutas que casan). No se ejecutaba nada, pero el motivo era impreciso: el fallback distingue `operation_unknown` de `operation_not_implemented`.

## Discrepancias registradas

- El prompt pide `/api/v1/health` y `/api/v1/readiness`; el contrato (`api-operations.json`) fija `/api/v1/health/live` (público) y `/api/v1/health/ready` (`read`). Se aplica el contrato (precedencia del contrato 00; la especificación no nombra estas rutas).
- **Defecto del contrato**: `P014_get__storage_locations` se repite para `/api/v1/storage-locations` y `/api/v1/storage/locations` (la especificación §87 solo nombra la segunda). OpenAPI exige ids únicos: el segundo recibe la ruta como sufijo y conserva el id original en `x-larm-contract-operation-id`.
- `/api/v1/events` es de P113 en el contrato; aquí está la conexión al bus, el heartbeat, el desbordamiento y el cierre. El replay por `Last-Event-ID` y `410 EVENT_CURSOR_EXPIRED` quedan para P113.
- `GET`/`PATCH /api/v1/settings` ya están implementadas para P003 con ETag, `If-Match`, CAS y outbox; PATCH valida Settings y aplica el límite de ancho de banda al limitador activo. OpenAPI marca ambas operaciones implementadas.

## Pruebas

8 de aplicación con Tower, 7 de OpenAPI (incluida la unicidad de ids), 6 de `Search`. Reales: OpenAPI al día y válido; bind `0.0.0.0` sin remoto rechazado; daemon que escucha **solo en 127.0.0.1** (`Get-NetTCPConnection`), live/ready, envelope con correlación propagada, 2 MiB → `body_too_large`, CORS off, **SSE que recibe el evento `credential.created` de una mutación real**, apagado con SSE abierto y log de peticiones sin tokens.
