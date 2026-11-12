# Revisión de contrato LARM 3.10.2 — telemetría operativa de fuentes P127

Fecha: 2026-10-25. Predecesora: 3.10.1. Ámbito: estado de `source.sync`, límite observado, última sincronización y respuesta de `model_source`.

## Contrato

- La respuesta `model_source` incorpora el objeto cerrado y obligatorio `operational`; `model_sourceCreate` y las operaciones de configuración no cambian. `operational.status` admite `UNKNOWN`, `SYNCING`, `HEALTHY`, `RATE_LIMITED`, `AUTH_REQUIRED` o `ERROR`. `last_sync_at` es RFC 3339 UTC nullable, definido como la hora de finalización del último intento. `rate_limit` es nullable y usa `limit`, `remaining`, `reset_at` y `retry_after_seconds`, cada uno nullable y con las validaciones de `source_v1.rate_limit` del SDK.
- `SyncResult` añade `rate_limit` opcional y nullable para que adaptadores actualizados informen el límite observado en una respuesta correcta; omitirlo mantiene compatibilidad con adaptadores ya instalados. Los errores 429 conservan la observación tipada en el error existente. Valores ausentes permanecen nulos; el Core no calcula límites ni tiempos que el adaptador no informó.
- `enabled` sigue siendo configuración y no determina el estado operativo. La fila nueva se crea como `UNKNOWN`; `source.sync` en curso fija `SYNCING`. Un resultado correcto fija `HEALTHY`, una limitación HTTP 429 fija `RATE_LIMITED`, 401/403 fija `AUTH_REQUIRED` y los demás fallos sanitizados fijan `ERROR`. Los errores de evaluación local posteriores a una respuesta válida no degradan la salud upstream.
- El snapshot se persiste en `model_source_operational_snapshot`, con clave foránea a `model_source`, sin cambiar el `row_version` de la configuración. La migración inicializa fuentes existentes como `UNKNOWN` y un trigger inicializa las futuras. El estado operativo no se mezcla con `metadata` abierta.
- El worker durable P050 escribe `SYNCING` al iniciar. Al terminar, el snapshot, el cursor/resultado del `source_watch`, el evento y su outbox se confirman en una transacción. El snapshot conserva sólo el código de estado y los metadatos permitidos de límite; no almacena cuerpos upstream, cabeceras, URLs firmadas ni secretos.
- La UI separa activación de estado operativo y muestra límite y fecha sólo cuando existen observaciones tipadas. Fuentes sin una ejecución durable continúan en `UNKNOWN`; esta revisión no inventa una sincronización manual distinta de P050.

## Migración

`contracts/migrations/0009_p127_source_operational_snapshot.sql` crea la tabla y trigger, y siembra los registros existentes. La migración local equivalente es `migrations/0011_p127_source_operational_snapshot.sql`. El paquete también incorpora la definición en `contracts/persistence.sql`.

## Verificación contractual

- `model_source` exige el objeto cerrado `operational`; la solicitud de creación permanece idéntica.
- `RateLimit` rechaza propiedades adicionales, valores decimales no canónicos, límites mayores que el máximo decimal del contrato, fechas distintas del formato UTC del SDK y `remaining > limit`.
- `SyncResult.rate_limit` acepta ausencia o `null` por compatibilidad y valida el objeto cuando se suministra.
- Las pruebas cubren migración y siembra, `UNKNOWN`/`SYNCING`/éxito/401/403/429/error, rollback atómico, recuperación después de abrir SQLite, ausencia de inferencias y filtrado de secretos.
