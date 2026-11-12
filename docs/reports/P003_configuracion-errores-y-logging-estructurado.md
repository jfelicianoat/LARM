# P003 — Configuración, errores y logging estructurado

**Revalidado**: 2026-10-11 · **Estado**: **BLOCKED_REAL** — 24/24 PASS (4 reales); bloqueado por las dependencias P001 y P002. [evidence.json](P003_configuracion-errores-y-logging-estructurado.evidence.json) · [configuración](../operations/configuration.md)

Ampliación posterior del 2026-10-27: [Settings y descarga P048 TCP](P003_settings_file_2026-10-27.md) pasan 49 pruebas focales y dos nativas (6,54 s y 182,98 s). La API usa la misma prioridad del fichero que el arranque y conserva la base guardada; sin override de fichero, P048 mantiene 65.536 bytes/s después de dos reinicios y recupera con bloques acotados hasta SHA/CAS/procedencia. Se repiten confirmación/replay, rechazo de corrupción y cierre de reservas/outbox. Cualificación 39/16, Clippy/fmt y package_copy 74/74 pasan; 37 inputs y binarios contrastados. Continúan pendientes H08 agregado, disco lleno, ingestión P043, GUI conjunta y dependencias; frontend/workspace/runners completos no se repiten. Los recibos anteriores conservan su alcance histórico.

## Revalidación: defectos encontrados y corregidos

1. **`LarmError` no tenía la forma del schema `Error`** (contrato 07): le faltaban `retryable` y `details`, y serializaba el detalle técnico (`detail`) hacia la API. Además había un segundo modelo de error en la rebanada P107. Ahora hay **un solo tipo**:
   - `{code, message, retryable, correlation_id, details{reason, evidence_refs, resource_ids, original_error}}`;
   - catálogo de 34 códigos con su estado HTTP;
   - `retryable` nunca en licencia ni incompatibles;
   - detalle técnico solo en logs;
   - `original_error` redactado y truncado a 4096.
2. **`Settings` no cumplía el schema**:
   - `log_level` y `log_format` estaban en el nivel superior, y el schema tiene `additionalProperties:false`;
   - los bytes se aceptaban como número;
   - el límite de velocidad se serializaba como número, cuando el schema exige string;
   - había enums libres;
   - no se comprobaban UUIDv7, puerto ≥ 1 ni elementos vacíos.

   Corregido con `DecimalU64` estricto, enums cerrados y `LowDiskThresholds` como unión etiquetada. La salida real de `print-settings` se valida contra el schema.
3. **Era imposible cambiar `low_disk_thresholds` de PERCENT a BYTES**: el merge profundo mezclaba los campos de los dos modos. Ahora un objeto con otro `mode` sustituye al anterior.
4. **Cualquier variable `LARM_*`** del entorno se tomaba como config y podía impedir el arranque. Ahora solo cuentan `LARM_<SECCIÓN>__<CLAVE>`.
5. **Un nivel de log inválido pasaba a `info` en silencio**. Ahora se rechaza (exit 2).
6. **No había rotación de logs** (`larm.log` en append infinito). Ahora hay rotación diaria con la retención del contrato (`diagnostics.log_retention_days` = 14) y timestamps RFC3339 UTC con milisegundos y `Z`.
7. El redactor se amplió (`hf_`, `Bearer` suelto, `X-Api-Key`, `client_secret`) y pasó al dominio. Los sha256 no se redactan.
8. El correlation id entrante solo se acepta si es UUIDv7 en minúsculas, como exige el schema.

## Pruebas reales (daemon en Windows)

- `precedence`: CLI > entorno > fichero > defaults, con la salida válida contra `Settings`.
- `rejects`: 6 configs inválidas → exit 2 sin escribir logs.
- `logging`: JSON diario, timestamps ms+Z, correlation UUIDv7 y el secreto inyectado redactado.
- `retention`: 20 logs antiguos → quedan 14.

## Incremento H08 — Settings durable y límite activo (2026-10-25)

`GET` y `PATCH /api/v1/settings` ahora están conectados a la tabla singleton `settings`. La respuesta publica ETag; PATCH exige `If-Match`, valida el schema y guarda la revisión junto con `settings.changed` en el outbox mediante CAS. La precedencia sigue siendo defaults < Settings persistido < fichero < entorno < CLI. La pantalla Configuración permite fijar un límite en bytes por segundo o elegir sin límite.

El daemon comparte un token bucket entre todos los consumers y reserva permiso antes de pedir cada bloque a la fuente. Cambiar el límite despierta los transfers que esperan y afecta a su siguiente bloque; al reiniciar se recupera el valor persistido con los overrides de mayor precedencia. Los workers usan los máximos configurados globales y por fuente.

Pruebas de código: API Settings 2/2, persistencia SQLite/CAS/outbox 2/2, configuración y precedencia 52/52, política y consumidor 35/35, frontend 105/105 y typecheck PASS. P003 sigue `BLOCKED_REAL` por dependencias anteriores; la aceptación de caudal con dos transferencias reales en Windows permanece pendiente dentro de P051.

Pendiente fuera de P003: la propagación del correlation id hasta el worker de adaptador llega con P021.
