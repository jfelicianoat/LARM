# Configuración de LARM

Fuente de verdad: [`docs/contracts/defaults.json`](../contracts/defaults.json) y el schema `Settings` de [`larm.schema.json`](../contracts/schemas/larm.schema.json). `Settings` tiene exactamente esa forma, sin campos adicionales; serializar los defaults reproduce el contrato (test `defaults_serializan_exactamente_el_contrato`). `larm-daemon print-settings` imprime la configuración efectiva y la prueba real la valida contra el schema.

## Precedencia

```text
defaults < Settings persistido < config file (--config) < variables de entorno (LARM_*) < CLI
```

Cada capa sobreescribe solo las claves que define (deep-merge sobre objetos). Un objeto con `mode` distinto (p. ej. `low_disk_thresholds` de `PERCENT` a `BYTES`) sustituye al anterior entero. Si el resultado no cumple el schema o sus invariantes, el daemon sale con código 2 y error `VALIDATION_FAILED` antes de escribir nada; nunca se aplica un default silencioso.

El servidor inicializa la base persistida en el primer arranque. `GET /api/v1/settings` muestra el resultado efectivo de todas las capas; `PATCH` conserva la solicitud validada mediante CAS y devuelve el resultado efectivo, que también usan los cambios en vivo. Una clave presente en el fichero, incluso con `null`, prevalece sobre el valor guardado desde la API. Al retirar esa clave y reiniciar vuelve a aplicarse el valor persistido. Si se quiere editar el límite desde Configuración, se debe omitir su clave en el fichero. `print-settings` compone las entradas del proceso sin abrir la base persistida.

## Formas de sobreescribir

| Capa | Ejemplo |
|---|---|
| Fichero | `larm-daemon --config larm.json` con `{"api": {"port": 9000}}` |
| Entorno | `LARM_API__PORT=9211`, `LARM_STORAGE__DOWNLOAD_BANDWIDTH_LIMIT_BYTES_PER_SECOND=1048576` |
| CLI | `--bind 127.0.0.1 --port 8790 --data-root D:\LARM --log-format json --log-level debug` |

Solo son configuración las variables `LARM_<SECCIÓN>__<CLAVE>` cuya sección existe en `Settings` (`api`, `storage`, `leases`…); otras `LARM_*` (p. ej. `LARM_LOG_LEVEL` o variables de prueba) se ignoran aquí. Dentro de una sección conocida, una clave inexistente es un error.

Un valor solo de dígitos se conserva como string cuando el default de esa ruta es string o `null` (bytes del contrato 00: `minimum_free_bytes`, límite de velocidad…); en otro caso se interpreta como JSON escalar (`9211` → número, `false` → booleano) y, si no lo es, como string.

## Bytes

Los tamaños son strings decimales (`"1073741824"`), nunca números JSON, sin signo ni ceros a la izquierda y como máximo `9223372036854775807`. Un número JSON en un campo de bytes se rechaza.

## Defaults seguros (extracto del contrato)

- API: bind `127.0.0.1:8790`, **acceso remoto off**, body máx 1 MiB, timeouts 30 s/5 s.
- Privacidad: **telemetría externa off**, sin rutas personales en diagnósticos.
- Almacenamiento: margen mínimo 1 GiB + 500 bps, `unknown_size_policy=BLOCK`, concurrencia global 3.
- Rutas: todo bajo `%LOCALAPPDATA%/LARM` (servicio: `%PROGRAMDATA%/LARM`).
- Scheduler: prioridades `PINNED 100 > HIGH 80 > NORMAL 40 > BACKGROUND 10`.

## H08 — límite de velocidad de descarga

`storage.download_bandwidth_limit_bytes_per_second`: `null` (default, sin límite) o string decimal **positivo** (`"1048576"`). `"0"` se rechaza en validación. Orden de prioridad de descargas (contrato): `HIGH > NORMAL > BACKGROUND` con FIFO dentro de cada nivel, sin preempción destructiva; cambio por `PATCH /downloads/{id}/priority` con `If-Match` (se materializa en P051).

## H09 — umbrales de disco bajo

`storage.low_disk_thresholds` admite dos modos excluyentes:

- `PERCENT` (default): `warning_basis_points: 1500` (15 %), `critical_basis_points: 500` (5 %).
- `BYTES`: `warning_bytes` / `critical_bytes` absolutos.

Invariante validada en ambos modos: `critical < warning` (igualdad y cruce se rechazan). La evaluación se hace por volumen físico y las alertas se disparan por transición (WARNING/CRITICAL), materializada en P015/P139. El ejemplo 15/5 es un default editable, independiente de la reserva de admisión (`storage.minimum_free_bytes`).

## Logging

Nivel y formato no forman parte de `Settings` (el schema no los admite): se fijan por proceso.

- Formato `pretty` (default) o `json`: `LARM_LOG_FORMAT` < `--log-format`.
- Nivel `error|warn|info|debug|trace` (`info` default): `LARM_LOG_LEVEL` < `--log-level`. Un valor fuera de la lista detiene el arranque con `VALIDATION_FAILED`.
- Salida: stdout y `<data_root>/logs/larm.AAAA-MM-DD.log`, con rotación diaria; se conservan `diagnostics.log_retention_days` ficheros (14 por defecto; 0 conserva solo el del día).
- Timestamps en UTC, RFC 3339 con milisegundos y `Z`.
- **Redacción** (`larm_domain::redact`): cada línea sustituye por `[REDACTED]` las cabeceras `Authorization`/`Cookie`/`X-Api-Key`, `Bearer <token>`, pares `token=`/`api_key=`/`password=`/`secret=` y tokens con prefijo conocido (`sk-`, `ghp_`, `github_pat_`, `hf_`, `AKIA…`, JWT). Los tokens de LARM (hex sin prefijo) se redactan por su contexto, no por su forma, para no ocultar los sha256.
- **Correlación**: `x-correlation-id` (UUIDv7 en minúsculas; otro formato se descarta y se genera uno nuevo) desde la API hacia core; el error lo devuelve en `correlation_id` y en la cabecera.

## Errores normalizados

`larm_domain::error::LarmError` tiene exactamente la forma del schema `Error` (contrato 07):

```json
{"code": "MODEL_NOT_FOUND", "message": "Modelo no encontrado", "retryable": false,
 "correlation_id": "<UUIDv7>",
 "details": {"reason": "variant_not_in_registry", "evidence_refs": [], "resource_ids": [], "original_error": null}}
```

- Catálogo con su estado HTTP: `VALIDATION_FAILED` 400, `AUTH_REQUIRED` 401, `FORBIDDEN` 403, `NOT_FOUND`/`MODEL_NOT_FOUND` 404, `EVENT_CURSOR_EXPIRED` 410, `PRECONDITION_REQUIRED` 428, los conflictos 409 (`CONFLICT`, `INVALID_STATE_TRANSITION`, `ACTIVE_LEASE`, `STALE_REVISION`, `IDEMPOTENCY_CONFLICT`, `STORAGE_INSUFFICIENT`, `RESOURCE_UNAVAILABLE`, `APPROVAL_REQUIRED`, `LICENSE_REQUIRED`, `MODEL_OFFLINE`, `RUNTIME_NOT_INSTALLED`, `BACKEND_UNAVAILABLE`, `LOAD_OOM`), 422 (`UNSUPPORTED_CAPABILITY`, `INCOMPATIBLE`, `INTEGRITY_FAILED`, `UNKNOWN_SIZE`, `MODEL_CORRUPT`, `RUNTIME_INCOMPATIBLE`), 502 (`ADAPTER_CRASHED`, `RUNTIME_CRASH`, `DOWNLOAD_FAILED`, `PROTOCOL_ERROR`), 503 (`UPSTREAM_UNAVAILABLE`, `AUTH_BACKEND_UNAVAILABLE`), `TIMEOUT` 504 e `INTERNAL` 500.
- Un código desconocido (p. ej. de un adaptador) se conserva tal cual y responde 500.
- `retryable` nunca es `true` en licencia, aprobación, incompatibilidad, integridad, validación ni permisos.
- `message` es seguro para el usuario. El detalle técnico va solo al log (`with_technical`, no se serializa). `details.original_error` guarda el error del sistema externo redactado y truncado a 4096 caracteres.
