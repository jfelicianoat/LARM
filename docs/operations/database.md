# Base de datos de LARM

SQLite local con SQLx (`larm-db`, ADR-0006). Contiene solo metadatos: los modelos nunca se guardan como blobs (spec §86).

## Ubicación

`<data_root>/state/larm.sqlite` (`paths.database_relative_path`), con sus ficheros `-wal` y `-shm`. `data_root` es `%LOCALAPPDATA%/LARM` por defecto (`%PROGRAMDATA%/LARM` en modo servicio).

## Pragmas

Los fija el DDL del contrato (`docs/contracts/persistence.sql`) y los aplica el pool al abrir cada conexión:

| Pragma | Valor | Motivo |
|---|---|---|
| `journal_mode` | `WAL` | lectores y un escritor concurrentes; un backup convive con escrituras |
| `foreign_keys` | `ON` | integridad referencial; las FK del DDL son `DEFERRABLE INITIALLY DEFERRED` y se comprueban al confirmar |
| `busy_timeout` | `5000` ms | espera ante un bloqueo antes de fallar |

Si otro proceso retiene la base más de `busy_timeout`, la operación falla con `RESOURCE_UNAVAILABLE` reintentable; cualquier otro fallo de SQLite es `INTERNAL`, con el error original redactado en `details.original_error`.

## Esquema y migraciones

- `migrations/0001_bootstrap.sql` es copia literal del DDL contractual (34 entidades y 9 tablas auxiliares). Un cambio del esquema solo entra como migración nueva.
- `migrations/0002_adapter_selection.sql` añade la selección de adaptadores (P024) y un índice único de identidad/versión con `NOCASE` de SQLite. No selecciona ni habilita instalaciones existentes. Si hay duplicados, falla y revierte la migración completa; conserva las filas y eventos para que el operador pueda resolver la ambigüedad. No los fusiona ni los borra. El paquete 3.11.1 aplica 13 migraciones hasta P032 y contiene 53 tablas de usuario: 48 contractuales más `schema_metadata`, `adapter_selection`, los locks/cualificaciones de perfiles y el estado de alertas por volumen. Las aperturas del pool y de los backups usan rutas de filesystem literales: el prefijo canónico de Windows y `%`, `#` o apóstrofes no se interpretan como componentes de URL.
- Las migraciones van dentro del binario. La tabla `schema_metadata(version, name, sha256, applied_at)` registra cada una con el sha256 de su contenido.
- Cada migración se aplica en una transacción `BEGIN IMMEDIATE` junto con su registro: si falla a mitad no queda ninguna tabla nueva ni registro.
- Reaplicar es idempotente. Si una migración ya aplicada tiene ahora otro contenido, el arranque se detiene con `CONFLICT` (`migration_checksum_mismatch`).
- Los tamaños se guardan como `INTEGER` (máximo `9223372036854775807`) y viajan en JSON como string decimal. Un valor negativo leído de la base es `INTEGRITY_FAILED`.

## Comandos

```text
larm-daemon [--data-root DIR] db-info              # estado: pragmas, migraciones con sha256, filas por tabla
larm-daemon [--data-root DIR] db-backup --out F    # backup atómico + verificación
larm-daemon [--data-root DIR] db-restore --from F  # restauración verificada (daemon detenido)
```

`db-info` migra si hace falta y no muestra contenido de filas. La ruta de la base solo aparece con `privacy.include_personal_paths=true`; si no, se muestra `<data_root>/state/larm.sqlite`.

## Backup y restauración

- **Backup**: `VACUUM INTO` hacia `F.partial` y renombrado a `F` al completarse: un fallo no deja un backup a medias ni destruye el anterior. Se verifica con `integrity_check` y el número de filas de cada tabla frente a la base viva.
- **Restauración**: comprueba el backup (íntegro y con `schema_metadata`) antes de tocar nada. Aparta la base actual y sus `-wal`/`-shm` como `larm.sqlite.pre-restore*`, coloca la copia y comprueba que la restaurada tenga las filas del backup. Si algo falla, devuelve la base previa a su sitio.
- Si la base está abierta por otro proceso (por ejemplo, el daemon en marcha), Windows no deja moverla: la restauración falla con `RESOURCE_UNAVAILABLE` y no cambia nada.
- `*.pre-restore` se conserva hasta que el operador lo borre.

## Pruebas

- `cargo test -p larm-db`: pragmas, tablas exactas del DDL, idempotencia, migración rota sin rastro, migración modificada bloqueada, rollback, FK diferidas, H07/H01/H02/H08, bytes hasta `i64::MAX`, backup/restore y restauración rechazada.
- `tools/p004_real.py`: el binario real sobre NTFS, incluida la inyección de fallos desde un segundo proceso (base abierta y bloqueo retenido).
