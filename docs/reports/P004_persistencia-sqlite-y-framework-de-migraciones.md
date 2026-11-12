# P004 — Persistencia SQLite y framework de migraciones

**Revalidado**: 2026-10-11 · **Estado**: **BLOCKED_REAL** — 23/23 PASS (4 reales); bloqueado por las dependencias P001–P003. [evidence.json](P004_persistencia-sqlite-y-framework-de-migraciones.evidence.json) · [operación](../operations/database.md)

Ampliación 2026-10-27: las aperturas del pool y de backups usan rutas literales de filesystem. Las rutas canónicas Windows y los caracteres Unicode/%/#/apóstrofe pasan reapertura y copia/restauración: 2/2, biblioteca 77/77, integración 18/18 y cuatro recorridos CLI. Las expectativas de P004 se actualizan a las 13 migraciones/53 tablas del contrato 3.11.1, sin cambiar el DDL. Se preservan corridas anteriores y el fallo restringido de renombrado; las mismas pruebas pasan bajo ejecución normal de Windows. [Informe y alcance](P048_native_http_2026-10-27.md). La evidencia formal de 23/23 que figura arriba es histórica y no se ha repetido completa.

## Revalidación: lo que estaba mal

- La puerta del workspace estaba roja: un aviso de clippy en `tx.rs` y un test escrito contra columnas inexistentes de `storage_location`.
- **Las migraciones no eran atómicas**: el SQL y el registro iban en sentencias separadas y sin transacción, así que un fallo a mitad dejaba la base sin poder reintentar. Ahora es `BEGIN IMMEDIATE` + registro + `COMMIT`, comprobado con una migración rota que no deja rastro.
- Las migraciones se leían de una carpeta en tiempo de ejecución y sin hash. Ahora van en el binario, con sha256 en `schema_metadata`, y una migración aplicada que se modifique bloquea con `CONFLICT`.
- `applied_at` usaba `to_rfc3339()` (+00:00 y nanosegundos). Ahora es ms+Z.
- **No existía la restauración** que exige el prompt, y el backup borraba el anterior antes de crear el nuevo. Ahora:
  - el backup es atómico (temporal + rename);
  - la verificación compara fila a fila;
  - el restore se verifica, conserva la base previa y la devuelve si algo falla.
- `ByteCount` aceptaba números y negativos. Se sustituye por `DecimalU64` ⇄ INTEGER, y un negativo en la base da `INTEGRITY_FAILED`.
- Faltaban el comando `db-info` y `docs/operations/database.md`.
- Los tests eran débiles: el de FK diferida pasaba hubiera error o no, y el backup nunca se restauraba.
- Un bloqueo `SQLITE_BUSY` daba el código inventado `DB_IO`. Ahora da `RESOURCE_UNAVAILABLE` reintentable.

## Pruebas reales

- `db-info` idempotente sobre una raíz nueva.
- `db-backup` y `db-restore` con 25 filas.
- Restore con la base abierta por otro proceso → `RESOURCE_UNAVAILABLE` sin tocar nada.
- Bloqueo de escritura retenido → falla tras `busy_timeout` sin dejar el esquema a medias; el backup convive con un escritor en WAL.
