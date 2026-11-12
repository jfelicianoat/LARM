# Almacenamiento: identidad de volumen y último estado

El Storage Profiler (P012) enumera los volúmenes del equipo y las rutas de red, les da una identidad estable y mantiene al día el estado de cada `storage_location`. Solo lee: no formatea, no monta y no crea recursos compartidos.

## Identidad (spec §28, contrato 02)

| Campo | Volumen local | Ruta de red |
|---|---|---|
| `volume_guid` | GUID de Windows en minúsculas y sin llaves (`fb4f4fbc-…`), como en P000 | `null`: nunca se inventa |
| `volume_key` | El mismo GUID | `\\servidor\recurso` en minúsculas |
| `drive_letter` | Primera letra entre los puntos de montaje (`C:`); solo para mostrar | Letra mapeada, si la hay |
| `unc_path` | `null` | `\\servidor\recurso` tal como se escribió |

La reserva de espacio, la deduplicación por volumen y la identidad usan la `volume_key`. La letra es solo presentación: si cambia de `E:` a `F:`, la ubicación sigue siendo la misma.

Se reconocen:

- volúmenes sin letra (Recovery, EFI);
- volúmenes montados en carpetas;
- volúmenes con varias letras.

Para la clave UNC se aceptan `\\servidor\recurso\…`, `\\?\UNC\servidor\recurso\…` y la barra `/`. Se rechazan las rutas de dispositivo (`\\.\`, `\\?\C:`), los servidores o recursos vacíos y `..`.

Si el GUID de un volumen no se puede leer o una ruta UNC no se puede analizar, la `volume_key` queda desconocida (`metadata.unknown`). La colocación automática (P014) no debe usar ese volumen.

## Clase de rendimiento (spec §115)

Solo con evidencia real del dispositivo (`IOCTL_STORAGE_QUERY_PROPERTY`):

| Evidencia | Clase |
|---|---|
| Unidad de red | NETWORK |
| Medio extraíble o bus USB/SD/MMC | REMOVABLE |
| Bus NVMe | NVME |
| Otro bus sin penalización de búsqueda | SSD |
| Otro bus con penalización de búsqueda | HDD |
| Sin evidencia, o un VHD sin penalización conocida | UNKNOWN, con motivo `storage_class_unproven` |

Nunca se deduce del nombre, la letra o el tamaño.

## Capacidad y espacio libre

`GetDiskFreeSpaceExW`, con enteros de 64 bits. «Libre» es lo disponible para el usuario del daemon, respetando cuotas. Se publican como string decimal (contrato 00).

## Último estado de `storage_location`

Cada sondeo (el de arranque, cada `POST /hardware/refresh` y cada `POST /storage-locations/{id}/probe`):

1. comprueba también las raíces UNC registradas;
2. guarda la observación en `metadata.storage` del perfil de hardware;
3. aplica a cada `storage_location` lo observado en su `volume_key`.

| Observado | Estado |
|---|---|
| No aparece o la ruta de red no responde | OFFLINE (se conservan capacidad, espacio y `last_seen_at`) |
| Volumen de solo lectura | READ_ONLY |
| Volumen visible, pero falta la carpeta raíz | MISSING |
| Visible, pero sin capacidad | UNKNOWN |
| Nivel CRITICAL de los umbrales de disco (H09) | LOW_SPACE (pausa nuevas asignaciones; no borra ni cancela nada) |
| Lo demás | ONLINE |

Los umbrales salen de `storage.low_disk_thresholds` en `Settings`: PERCENT, por defecto 15 % de aviso y 5 % crítico, o BYTES. La comparación es estricta, así que la igualdad no cruza. Si una ruta de red no responde no se marca MISSING: no se inventa. DEGRADED y ERROR existen en el schema, pero ningún sondeo los produce todavía.

- `free_bytes` y `last_seen_at` se refrescan **sin** subir `row_version`: son observación del sistema y no deben romper el `If-Match` de quien edita la ubicación.
- Un cambio de estado, capacidad, sistema de ficheros, clase o letra sí sube `row_version` y escribe `storage.state_changed` (`{from, to, volume_key, observed_at}`) en el outbox, en la misma transacción.
- Dos sondeos simultáneos no duplican cambios: la transacción toma el bloqueo de escritura antes de leer.
- Un sondeo solo juzga las ubicaciones que conocía al empezar. Una ubicación creada mientras corría no se marca OFFLINE por no estar en esa observación, porque su recurso de red no se sondeó (defecto corregido en P016).

Si el sondeo de almacenamiento falla, el perfil de hardware se registra igual, `metadata.storage` lleva `{error, detail}` y `storage_location` no se toca.

## Gestionar ubicaciones (P014)

| Operación | Scope | Notas |
|---|---|---|
| `GET /storage-locations` (y sus alias `GET /storage`, `GET /storage/locations`) | `read` | Filtros `q` (nombre o ruta) y `state`; el cursor queda ligado a los filtros |
| `POST /storage-locations` | `storage:write` | `Idempotency-Key`; 201 |
| `GET /storage-locations/{id}` | `read` | |
| `PATCH /storage-locations/{id}` | `storage:write` | `If-Match`: 428 si falta, 409 si es vieja |
| `DELETE /storage-locations/{id}` | `storage:write` | `If-Match`; 204 |
| `POST /storage-locations/{id}/probe` | `storage:write` | Job, 202 |

El DTO de escritura solo lleva `name`, `root_path`, `reserved_bytes`, `priority`, `auto_placement` y `performance_class`. Esta última es una pista que solo se usa si la clase observada es UNKNOWN (`metadata.performance_class_source` dice cuál se usó). El GUID, la ruta canónica, la capacidad, el espacio, el estado y las fechas los calcula el servidor.

**Antes de guardar** la raíz se valida contra el disco:

- debe ser absoluta o UNC: se rechazan las rutas relativas, de dispositivo, con flujos alternativos (ADS) o con `..`;
- debe existir y ser una carpeta;
- ni ella ni ningún ancestro puede ser un symlink, una junction o un punto de montaje (`reparse_point_rejected`);
- LARM debe poder escribir en ella: se crea un fichero propio y se borra;
- no puede estar dentro de otra ubicación ni contenerla (`root_overlap`, 409);
- su identidad sale del volumen que la contiene.

**Espacio utilizable**: `libre - reserva`, nunca negativo. Varias raíces del mismo volumen comparten presupuesto: la reserva del volumen es la mayor de sus raíces, no la suma. Las respuestas lo calculan en `metadata.volume_budget` (`usable_bytes` y `shared_with`), sin guardarlo.

Cuando el sondeo conoce la capacidad, crear o editar una reserva superior a ella devuelve 400 `VALIDATION_FAILED` / `reservation_exceeds_capacity`, sin cambiar filas, revisión ni eventos. Una reserva igual a la capacidad es válida: puede dejar el espacio utilizable en cero si supera el libre. Si la capacidad es desconocida no se inventa un límite. La interfaz explica el rechazo, conserva el formulario y permite actualizar la lista, también al cancelar una edición.

**Protección de datos**:

- quitar una ubicación con artefactos es 409 `location_has_artifacts`, y si la usan runtimes o descargas, 409 `location_in_use`;
- cambiar la raíz de una con artefactos es 409 `root_change_requires_migration`;
- si la misma ruta pertenece ahora a otro volumen y hay artefactos, es 409 `volume_identity_changed`;
- quitar una ubicación nunca borra ficheros.

Cada alta, cambio o baja escribe `storage.state_changed` con `action` en el outbox.

**Colocación automática** (`larm-domain::locations::place`). Los hard gates son:

- estado ONLINE (LOW_SPACE pausa nuevas asignaciones);
- `auto_placement` activado;
- identidad conocida;
- espacio suficiente;
- en FAT32, ningún fichero de más de 4 GiB.

Las políticas son ASK_ALWAYS (por defecto: nunca elige sola), PREFERRED_LOCATION, MOST_FREE_SPACE, FASTEST_STORAGE (NVME > SSD > HDD > NETWORK > REMOVABLE > UNKNOWN) y BY_MODEL_TYPE. CUSTOM_RULES por ahora es solo modelo de datos y pregunta. Los empates se resuelven por prioridad descendente y luego por id. La usará el preflight de descargas (P015).

## Consultarlo

- `larm-daemon storage-probe [--unc \\servidor\recurso]` imprime la observación.
- Escritorio:
  - pantalla **Almacenamiento**: ubicaciones con estado en texto, espacio utilizable y presupuesto compartido; añadir, editar, analizar y quitar con confirmación;
  - pantalla **Hardware**: todos los volúmenes.

## Estado del PC IA (2026-10-11)

| Volumen | Clase | Detalle |
|---|---|---|
| C: (`fb4f4fbc-…`) | NVME | NTFS, 1,8 TiB en el Crucial CT2000P310 |
| Recovery | NVME | Sin letra |
| EFI | NVME | FAT32, sin letra |
| `\\localhost\Temp` | NETWORK | Recurso compartido ya existente; se usó en las pruebas sin modificarlo |

El segundo volumen escribible que pide P000 para G04/G05 sigue sin existir.

## Preflight de espacio y reservas (P015)

Una operación (descarga, transformación, copia) se describe en **fases**. Cada fase lista los ficheros vivos a la vez, con:

- su papel: `DOWNLOAD`, `FINAL_ARTIFACT`, `INSTALLED`, `DERIVED_CACHE` o `TEMPORARY`;
- su volumen; `$destination` es el de la ubicación que se evalúa;
- su tamaño, con confianza EXACT, HIGH, ESTIMATED o UNKNOWN y su cota superior;
- si ya existe en disco.

| Concepto | Cálculo |
|---|---|
| Pico nuevo del volumen v | Máximo, entre fases, de los bytes **nuevos** vivos a la vez en v. Lo que ya existe (fuente local, CAS) no es asignación nueva. |
| Margen | `max(minimum_margin_bytes, ceil(pico · margin_basis_points / 10000))`. Por defecto, `max(1 GiB, 5 %)`. |
| Requerido | Pico nuevo + margen. |
| Utilizable | Libre del SO − mínimo de política − reservas de otros trabajos, nunca negativo. El mínimo de política es el mayor entre `minimum_free_bytes` y las reservas de las raíces del volumen. |

Ejemplo del contrato: fuente de 30 ya en disco, temporal de 18 y salida de 9 en el mismo volumen dan 57 de total y 27 nuevos. Si la fuente se descarga, 57 nuevos.

- Los tamaños no exactos se reservan por su **cota superior**.
- Un tamaño UNKNOWN sin cota no es elegible (`size_unknown`) salvo con un presupuesto duro explícito del operador (`hard_budget_bytes`).
- Toda la aritmética es comprobada: lo que no cabe en 64 bits es `size_overflow`, nunca un número truncado.

El planificador evalúa **todas** las ubicaciones y dice por qué cada una sirve o no:

- `fits`;
- `state_offline`, `state_missing`, `state_low_space`…;
- `volume_identity_unknown`;
- `fat32_file_too_large`;
- `size_unknown`;
- `insufficient_space: requiere X y hay Y utilizables`.

Las que sirven aparecen primero, ordenadas de más a menos espacio utilizable.

**Admisión**: una sola transacción de escritura reserva el requerido de cada volumen en `volume_reservation`, todo o nada.

- La medida del espacio libre debe tener menos de 5 s (`free_snapshot_stale`, reintentable).
- Si algo no cabe: `STORAGE_INSUFFICIENT` y no se reserva nada.
- La propia reserva no se cuenta dos veces: al crear bytes, `remaining = total − creados`, porque el SO ya los refleja.
- Dos admisiones simultáneas no reservan el mismo espacio.

Para probarlo a mano:

```text
larm-daemon storage-preflight --file operacion.json [--location <id> --job <id>]
```

El diálogo de destino antes de descargar (spec §30) llega con los planes de descarga (P052).
