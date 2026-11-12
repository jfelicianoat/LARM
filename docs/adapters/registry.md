# Registro de adaptadores — P024

Paquetes, registro local, consultas, selección/rollback y actualización por API o CLI están implementados con journal, reservas y recuperación. Usan el profiler real y SQLite. La instalación produce una candidata INSTALLED; la suite P026 conserva la responsabilidad de cualificarla antes de promoverla. El estado acreditado y sus dependencias se registran en el informe de P024.

## Paquete

ZIP con `manifest.json` en la raíz y los ficheros del adaptador, incluido el ejecutable relativo anunciado. La firma es el campo `signature` del manifiesto, con las reglas de P022; no se acepta confianza suministrada por un fichero importado.

Se distinguen dos hashes: `AdapterImport.expected_hash` identifica los bytes del ZIP; `AdapterManifest.package_sha256` identifica los ficheros por la regla canónica de P022, excluyendo `manifest.json`. El ZIP se copia a un snapshot privado y se vuelve a verificar antes de extraer. El manifiesto y los ficheros se verifican de nuevo antes de publicar una versión.

Antes de extraer se comprueban rutas POSIX, escapes, ADS, nombres reservados de Windows, colisiones entre ficheros y directorios y duplicados sin distinguir mayúsculas. Se rechazan enlaces y reparse points. Los límites son 1 GiB, 100000 entradas y relación de expansión 1000; el directorio central tiene además una cota de memoria de 64 MiB, comprobada en el EOCD/ZIP64 **antes** de construir el índice ZIP. Se comprueban también los bytes realmente expandidos, sin fiarse exclusivamente de los tamaños de cabecera.

`ArchivePlan.required_bytes()` calcula ZIP privado + expansión + max(1 GiB, ceil(5 % del pico)). El coordinador reserva esa cantidad mediante P015 antes de extraer; mostrar el cálculo en la inspección no admite una operación. La publicación conserva un bloqueo de versión hasta terminar el commit. En el uso aislado de `StagedPackage`, Drop retira una publicación no confirmada. El coordinador durable transfiere esa propiedad al journal con `retain_for_recovery` antes de esperar a SQLite: un resultado incierto no permite borrar bytes posiblemente registrados.

`PackageControl` comprueba cancelación, tiempo y fencing entre bloques de IO. La inspección asíncrona tiene un timeout de 30 s y cancelación cooperativa. El coordinador conserva un hilo dueño del IO hasta que termine; cancelar el futuro que lo observa sólo señala cancelación y no libera sus recursos. Una lectura bloqueada por el sistema puede continuar hasta responder. Después de publicar se finaliza el punto de compromiso o se conserva PUBLISHING para reconciliar; no se revierte por el timeout de un observador.

La biblioteca ZIP está fijada a `zip = 9.0.3`, sin características por defecto y con Deflate mediante zlib-rs. Versión, checksum, licencia y compatibilidad Rust se contrastaron con [el registro oficial](https://crates.io/api/v1/crates/zip/9.0.3), y se registraron como extensión del lock de entorno del 16 de octubre, sin alterar la fecha del inventario inicial.

## Registro local y comandos de inspección

`RegistrySource` es el puerto de descubrimiento. `LocalFolderRegistry` inspecciona los ZIP de una carpeta, devuelve versiones explícitas en orden estable y rechaza identidades duplicadas. No instala, ejecuta ni elige una versión por interpretar su nombre. El registro remoto sigue deshabilitado hasta su implementación firmada de P027 y configuración del operador.

```powershell
target/debug/larm-daemon.exe inspect-adapter-registry --folder C:\Pruebas\adaptadores
target/debug/larm-daemon.exe inspect-adapter-package --file C:\Pruebas\adaptadores\runtime.zip --expected-hash <SHA256_DEL_ZIP>
```

Los comandos solo muestran identidad, familia, hashes, bytes y el presupuesto calculado. No abren una base de datos, ejecutan plugins ni guardan secretos.

## API implementada

- `GET /api/v1/adapters`: scope `read`, filtros `q` y `state`, `limit` 1–200 y cursor firmado. El cursor está ligado a los filtros. Orden estable `(created_at, id)`.
- `GET /api/v1/adapters/{id}`: scope `read`, UUIDv7, fila persistida con la forma exacta de `adapter_installation`.
- `POST /api/v1/adapters/{id}/update`: scope `adapters:write`, UUIDv7, `Idempotency-Key` y `AdapterImport` exacto; devuelve 202 `JobReceipt`. El actor procede del Bearer autenticado. Aceptación, journal, respuesta original y evento/outbox se guardan juntos.
- `GET /api/v1/jobs/{id}`: estado actual para el propietario con `read` o un admin; otro principal recibe 404. No incluye rutas privadas del paquete ni del journal.
- `POST /api/v1/adapters/declarative` conserva el comportamiento de P023.

OpenAPI identifica la actualización como implementada. La selección y el rollback tienen una herramienta local del operador. Import/export mantiene las rutas y el propietario P028.

Los nombres DISCOVERED/ENABLED/DISABLED/BROKEN/UPDATE_AVAILABLE del prompt no sustituyen a los estados del schema y del DDL, que prevalecen: STAGED/VERIFIED/INSTALLED/VALIDATED/RETIRED/FAILED. La selección y habilitación se guardan en una tabla auxiliar tipada; no se basan en una clave libre de `metadata`. La publicación de un ZIP tampoco equivale a haber pasado la suite de conformidad de P026.

## Selección y rollback local

La migración `0002_adapter_selection` añade `active_installation_id`, `previous_installation_id`, `enabled`, `row_version` y `updated_at`, con referencias a instalaciones existentes. Añade también unicidad de identidad/versión (`NOCASE` de SQLite). Conserva el checksum de 0001, las filas y el lifecycle; una identidad duplicada impide aplicar 0002 sin borrar datos. Migrar no elige una versión por defecto.

```powershell
# Consulta; null si aún no se ha seleccionado una versión.
target/debug/larm-daemon.exe --data-root C:\Pruebas\larm adapter-selection --adapter-id acme.runtime

# La primera promoción necesita revisión 0 y empieza deshabilitada.
target/debug/larm-daemon.exe --data-root C:\Pruebas\larm adapter-selection --adapter-id acme.runtime --promote <UUID_INSTALACION_VALIDADA> --expected-revision 0

target/debug/larm-daemon.exe --data-root C:\Pruebas\larm adapter-selection --adapter-id acme.runtime --enabled true --expected-revision 1

# Usar la revisión recién consultada; no reutilizar los números del ejemplo.
target/debug/larm-daemon.exe --data-root C:\Pruebas\larm adapter-selection --adapter-id acme.runtime --rollback --expected-revision <REVISION_ACTUAL>
```

La herramienta usa la identidad Windows del operador, como la administración local de P009. Promover exige una instalación de la misma identidad/familia, `VALIDATED` y no revocada. El comando no concede VALIDATED ni ejecuta la suite de P026. La selección no concede permisos de ejecución: el host aplica P022 en cada lanzamiento, incluida la aprobación exacta de paquetes UNTRUSTED y la revocación actual.

Cada cambio compara la revisión esperada bajo el bloqueo de escritura de SQLite. Si cambió, responde `STALE_REVISION`. Selección, journal `adapter.selection` en fase COMPLETED y evento/outbox `adapter.updated` se confirman en una transacción. Un fallo en cualquiera revierte todo; el evento conserva la revisión de la instalación y lleva la revisión de selección en su payload.

Promover una segunda versión conserva la activa anterior si sigue válida; una activa fallida no sustituye al último bueno ya guardado. El rollback comprueba de nuevo el anterior y lo selecciona sin modificar paquetes ni lifecycle. Consume ese anterior para que otra petición no alterne las versiones. La habilitación se conserva; deshabilitar sigue permitido tras un fallo o revocación. Estas mutaciones son sólo de metadatos y no crean reservas de disco ni locks de artefactos. La instalación ZIP sí reserva y recupera su IO.

## Journal y coordinador

`AdapterImport` reproduce los tres campos publicados: `package_path`, `expected_hash` y `approval_id`. Los tres son obligatorios; el último puede ser null. El DTO rechaza campos adicionales, hashes fuera de formato y aprobaciones que no sean UUIDv7.

`AdapterUpdateRepository` y `SqlAdapterUpdates` guardan `adapter.update` en `operation_journal`, sin otra migración. Aceptar un trabajo escribe juntos su journal, la respuesta idempotente 202 y el evento/outbox. La clave se separa por principal/operación y el replay comprueba tanto cuerpo como instalación destino; actores distintos no comparten trabajos. Se conserva 30 días la respuesta original QUEUED, mientras el estado actual se consulta mediante `JobState`.

Los pasos internos son QUEUED, INSPECTING, RESERVED, EXTRACTING, STAGED, PUBLISHING, COMPLETED y FAILED. Se proyectan en los cuatro estados públicos del schema, sin añadir enums a la API. El digest canónico liga actor, correlación, destino, revisión, solicitud y plan. Un journal alterado se rechaza, sin retirar a ciegas sus reservas o bloqueos.

Preparar el trabajo comprueba la revisión del destino y confirma juntos plan, locks de volumen/paquete, reserva P015 y consumo de una aprobación exacta. Reutiliza P019/P015 dentro de la misma transacción; una instantánea antigua, espacio insuficiente, conflicto o fallo de outbox revierte todo, incluida la aprobación. El host valida manifiesto, firma y rutas; deriva el delta y digest de elevación de permisos con P022 antes de presentar el plan. Sin aprobación exacta no empieza a copiar.

Completar exige PUBLISHING, fencing y reserva vigentes. Registra sólo una candidata INSTALLED, confirma el job/evento y libera sus recursos en una transacción. No altera selección, permisos ni lifecycle del destino. Fallar también confirma error, estado y liberación de sus recursos juntos; no libera recursos ajenos. **El coordinador sólo debe llamar a completar tras verificar los bytes publicados y a fallar después de confirmar que el IO terminó.** Un timeout de observación no acredita esa condición.

`UpdateCoordinator` usa un lock exclusivo de proceso en la raíz y un mutex que vive en el hilo de IO. Las rutas privadas se reconstruyen desde raíz canónica, UUID del job e identidad/versión del manifiesto; no se siguen ciegamente strings del journal. Cada bloque comprueba el Resource Guard y registra los bytes escritos en la reserva. El hash canónico P022 comparte su implementación del SDK con checkpoints de cancelación/límites; el manifiesto se vuelve a leer con cota de 1 MiB. El profiler P012 aporta GUID/UNC y libre; P015 calcula el mínimo mayor entre el global y las ubicaciones de ese volumen. UNKNOWN, offline o read-only no se convierten en admisión.

La recuperación obtiene ambos locks antes de actuar. Antes de publicar, retira sólo el workspace propio y confirma FAILED por interrupción. En PUBLISHING verifica nuevamente manifiesto, hash, confianza y fencing, mantiene el lock de versión y completa el mismo job. Un ack perdido de un commit ya confirmado conserva la instalación; un error transitorio conserva el journal y recursos para reconciliar. Una copia final corrupta, revocada o rechazada por una revisión cambiada se conserva para inspección y no se registra. La pérdida de fencing detiene escrituras y no libera recursos a ciegas. No hay promoción ni ejecución de plugins en este flujo.

## Actualización y recuperación locales

```powershell
# request.json contiene AdapterImport exacto: package_path, expected_hash y approval_id (nullable).
target/debug/larm-daemon.exe --data-root C:\Pruebas\larm update-adapter --installation <UUID_ORIGEN> --request C:\Pruebas\request.json --idempotency-key <CLAVE_NUEVA>

target/debug/larm-daemon.exe --data-root C:\Pruebas\larm recover-adapter-updates
```

Son comandos del operador local de Windows, con actor `local-operator`; la aprobación, cuando haga falta, debe corresponder a ese actor, digest, recursos y hash. No convierten una aprobación de otro principal en propia. La actualización devuelve el receipt original y el estado actual del job; repetir la clave/cuerpo/destino no crea otra instalación. `recover-adapter-updates` reconcilia pendientes y limpia workspaces propios de trabajos terminales. No expone un endpoint HTTP alternativo.

Ambos aceptan `--trust-store <FICHERO>` con el `TrustStore` cerrado de P022, fijado explícitamente por el operador. Sin ese argumento, CLI y HTTP usan `<data_root>/security/adapter-trust.json` (JSON cerrado de hasta 1 MiB, sin reparse points) para bindings de publicador/official y revocaciones; las claves autorizadas proceden de `settings.security.publisher_keys`. Cada clave configurada debe tener el mismo identificador y valor en el binding. Claves retiradas de settings no se reactivan por permanecer en ese fichero. Un mapa vacío no fija claves. No se deduce publicador ni condición official del ZIP. Paquetes sin firma quedan UNTRUSTED y una firma desconocida se rechaza. El fichero no se incorpora desde el ZIP ni concede permisos de ejecución. La distribución remota conserva su propietario P027.

## Servicio HTTP y recuperación

El daemon reconcilia trabajos ya iniciados antes de escuchar; los QUEUED quedan en la cola durable. `UpdateService` se activa después de abrir el listener y supervisa un único worker por raíz. No crea un hilo bloqueado por cada petición. La aceptación vive fuera del futuro HTTP: un timeout o desconexión del cliente no abandona una aceptación confirmada ni crea otro job al repetir la clave. Un fallo transitorio de lectura deja la cola en SQLite para reintentar.

Al apagar, el servicio cierra admisión, señala cancelación al worker y espera a que termine su IO. Una copia interrumpida antes de publicar acaba FAILED y limpia sólo su workspace cuando ya no escribe. Tras publicar, un commit incierto conserva bytes/journal/recursos para el siguiente arranque. Un paquete inválido que queda terminal no impide continuar con otros trabajos pendientes. No hay liberación por mera antigüedad.

Las pruebas nativas HTTP comprueban autorización, actores, replay, aprobación exacta, cierre con copia en curso y muerte/reinicio después de publicar. Terminan únicamente sus propios procesos de prueba. El oráculo consulta NTFS y SQLite directamente y contrasta el volumen con el profiler real; no ejecuta ni cualifica el worker copiado.
