# P012 — Storage Profiler y Volume Identity en Windows

**Revalidación real 2026-10-25:** el daemon completó **6/6 pasos** de `tools/p012_real.py serve`, incluidos estados reales de C:, un GUID ausente y UNC, cuatro eventos/outbox y un refresco sin cambiar `row_version`. El recibo apunta a [evidencia](P012_storage-profiler-y-volume-identity-windows.evidence.json) y [transcripción](P012_runs/revalidation-20261025-022404/real/steps.json). P012 conserva su bloqueo por la dependencia P010.

**Fecha**: 2026-10-11 · **Host**: A9_MEGA (PC IA) · **Estado**: **BLOCKED_REAL**: 28/28 PASS (4 reales); solo bloqueado porque su dependencia P010 no tiene REAL_PASS. [evidence.json](P012_storage-profiler-y-volume-identity-windows.evidence.json) · [operación](../operations/storage.md) · [ADR-0010, ampliado](../adr/0010-excepcion-unsafe-sondeo-nativo.md)

## Qué se construyó

- **Dominio** (`larm-domain/src/storage.rs`):
  - Volume Identity (`volume_key`, `volume_guid`, `unc_path`, puntos de montaje y letra);
  - Storage Device Info (`StorageVolume`);
  - `PerformanceClass`, `DriveKind` y `BusKind`;
  - el analizador UNC y la clasificación por evidencia;
  - el estado observado de una ubicación.
- **Enumeración nativa** en el proceso hijo `larm-daemon probe-storage` (ADR-0010, ampliado):
  - todos los volúmenes, con su GUID y todos sus puntos de montaje;
  - sistema de ficheros y solo lectura;
  - capacidad y espacio libre en enteros de 64 bits;
  - bus, penalización de búsqueda, medio extraíble y modelo por `IOCTL_STORAGE_QUERY_PROPERTY`, leídos como bytes;
  - unidades de red mapeadas y las raíces UNC registradas.
- **Mapeo** (`larm-hardware/src/storage.rs`):
  - la identidad sale del GUID o de `\\servidor\recurso`, nunca de la letra ni de un GUID inventado;
  - una unidad mapeada y su UNC son un solo volumen;
  - lo no demostrable queda UNKNOWN y se declara en `unknown`.
- **Último estado** (`larm-db/src/storage.rs`):
  - aplica la observación a cada `storage_location` por su `volume_key` (ONLINE, READ_ONLY, UNKNOWN u OFFLINE);
  - `free_bytes` y `last_seen_at` se refrescan sin subir `row_version`;
  - un cambio material sube la versión y escribe `storage.state_changed` en el outbox, en la misma transacción.
- **Integración**:
  - el sondeo de arranque y `POST /hardware/refresh` sondean también el almacenamiento y lo guardan en `metadata.storage` del perfil;
  - un fallo de almacenamiento no tumba el perfil;
  - comando `larm-daemon storage-probe [--unc …]`.
- **Escritorio**: sección «Almacenamiento» en la pantalla Hardware. Muestra volumen (letra, ruta o «Sin letra», con el GUID), estado en texto, clase, sistema de ficheros y espacio en IEC con los bytes exactos.

## Defectos encontrados al probar

- **Carrera entre sondeos**: sin tomar el bloqueo de escritura antes de leer, observaciones simultáneas duplicaban cambios y eventos. La prueba de mutación lo confirmó tres de tres veces. La transacción ahora toma el bloqueo primero.
- **Rutas UNC desde Git Bash**: MSYS convierte `\\localhost\Temp` en `\localhost\Temp` antes de llegar al proceso. Las pruebas reales lanzan el daemon desde Python. El comportamiento del daemon es correcto.

## Prueba real en el PC IA

`tools/p012_real.py windows`: 9 comprobaciones contra oráculos independientes.

- **Enumeración**: los 3 GUID coinciden con `mountvol`, con los mismos puntos de montaje; 2 volúmenes no tienen letra (Recovery y EFI).
- **Capacidad y sistema de ficheros**: la capacidad de C: es exacta frente a `GetDiskFreeSpaceExW` (ctypes) y el NTFS coincide con `Get-Volume`.
- **Clase**: NVME, de acuerdo con `Get-PhysicalDisk` (NVMe, SSD).
- **Red**:
  - `\\localhost\Temp`, un recurso existente que no se modifica, está disponible, sin GUID y con capacidad exacta;
  - un servidor inexistente queda no disponible, sin datos y con el error real 0x80070035.
- **Estabilidad**: la identidad es la misma entre dos sondeos.

`tools/p012_real.py serve`, con el daemon real y filas de `storage_location` creadas directamente en su base (el CRUD es de P014):

- dos raíces del volumen de C: pasan a ONLINE, NVME, con la capacidad real y `row_version` 2;
- un `volume_key` inexistente pasa a OFFLINE, sin `last_seen_at` inventado;
- la raíz UNC queda ONLINE y NETWORK;
- se escriben 4 eventos con su fila de outbox;
- un refresco sin cambios avanza `last_seen_at` sin subir la versión ni escribir eventos;
- la observación queda en el perfil.

## Pendiente

- No hay un segundo volumen escribible con GUID propio (bloqueo de P000 para G04/G05). La identidad tras un cambio de letra se prueba con montajes simulados; la prueba real necesita ese volumen.
- `LOW_SPACE` según umbrales, CRUD de ubicaciones y validación de rutas y permisos: P014/P015.
- Prueba de velocidad opcional y consentida (spec §115): no implementada; la clase se basa solo en el bus y la penalización de búsqueda.
