# P050 — Source Watcher, sincronización incremental y notificaciones

**Estado: `BLOCKED_REAL`.** El código local está implementado y sus pruebas focales pasan. La cualificación real no se completa porque P044, P046, P047 y P029 aún no tienen `REAL_PASS`, y porque el daemon todavía no conecta el evaluador de compatibilidad de P090.

## Implementación

- `larm-domain` define `SourceWatch`, su DTO de creación y el objeto de confirmación de sync; valida UUIDv7, intervalos, configuración pública y prohíbe activar descarga automática sin P119.
- `larm-models` ejecuta sincronización incremental reutilizando cursor y ETag persistidos. Usa jitter determinista de hasta 300 segundos, backoff exponencial con tope de 24 horas, control CAS y motivos de error estables sin rutas ni secretos. Si una vigilancia pide compatibilidad y esta es desconocida, registra el fallo con backoff y conserva el cursor.
- `larm-events` clasifica `NEW_MODEL`, `NEW_COMPATIBLE_MODEL` y `MODEL_UPDATE`, suprime revisiones sin cambios y produce una clave de deduplicación opaca.
- `larm-db` usa las tablas contractuales existentes, sin migración adicional. Cursor/ETag, marcas operativas en `metadata.watcher`, notificaciones, eventos y outbox se confirman en una sola transacción. La deduplicación observa la ventana mínima de 24 horas.
- La API expone GET/POST/GET-id/PATCH/DELETE en `/api/v1/source-watches`, con scopes, DTO cerrado, `Idempotency-Key`, paginación firmada e `If-Match`.
- El daemon ejecuta `source.sync` solo con adaptador instalado y cualificado, método declarado y permisos de lectura u origen comprobados. La llamada tiene timeout de 60 segundos y se cierra el worker. Los modos de autenticación aún no cualificados fallan cerrados.
- El consumidor del watcher queda conectado al ciclo de vida del daemon; el apagado solicita su cierre y espera su terminación.

## Verificación

El runner formal `tools/p050_evidence.py` registra cinco comprobaciones locales PASS y una aceptación real BLOCKED: clasificación de eventos (2 tests), worker/cursor/reintentos (6), SQLite/outbox/rollback (5), API (1) y compilación del wiring del daemon. El JSON incluye hashes de fixtures, transcripciones y hashes de entradas contractuales.

La puerta oficial `tools/check.ps1 -Prompt P050 -Real` terminó **14/14 checks PASS, 0 fallos**. La suite serial del workspace pasó **1.202 tests en 122 suites, 0 fallidos y 0 ignorados**, con el loopback de los HTTP fixtures permitido. También pasan formato, Clippy, build de fixtures, arquitectura (17 crates), `package_copy` (67/67), Tauri y lint/typecheck/test/build del escritorio. La evidencia PromptEvidence se valida con `tools/validate_evidence.py` y conserva `BLOCKED_REAL`, no `REAL_PASS`.

## Pendiente para `REAL_PASS`

- Repetir el ciclo sobre una instalación upstream cualificada en Windows/PC IA, con datos y ubicaciones de prueba; verificar persistencia tras reinicio y observaciones/outbox reales.
- Obtener `REAL_PASS` en P044, P046, P047 y P029 según el orden contractual de dependencias.
- Conectar la evaluación de compatibilidad P090. Mientras tanto, una vigilancia que la exige no avanza cursor ni emite una compatibilidad supuesta.
- No hay descarga automática en P050; permanece deshabilitada hasta la política autorizada P119.

Incremento 2026-10-27: corregidos el gate redundante de capabilities de `source.sync` y la identidad de ejecución del watcher; el contexto usa el actor aprobado por P026 y la auditoría conserva `system:source-watcher`. Pasan 70/70 pruebas focales y el ensayo daemon/P048 TCP 1/1 (46,18 s), con cualificación 39/16, UNKNOWN→HEALTHY durable tras reinicio, replay, HEAD sin GET y sin descargas ni catálogo automáticos. La preparación del binding/due es fixture; no acredita ingesta P043, upstream privado ni GUI. Paquete 3.11.1 74/74, formato y Clippy host/daemon pasan. Se conservan el primer nativo fallido y la reproducción/corrección de la carrera Windows del control atómico. [Informe del incremento](P127_sync_binding_2026-10-27.md).
