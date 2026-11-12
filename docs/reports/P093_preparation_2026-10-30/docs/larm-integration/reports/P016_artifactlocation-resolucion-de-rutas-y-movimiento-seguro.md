# P016 — ArtifactLocation, resolución de rutas y movimiento seguro

**Fecha**: 2026-10-11 · **Host**: A9_MEGA (PC IA) · **Estado**: **BLOCKED_REAL**. Todas las pruebas ejecutables pasan, incluida la real (11 pasos), pero quedan 3 pruebas BLOCKED y sus dependencias P014, P015 y P019 no tienen REAL_PASS. [evidence.json](P016_artifactlocation-resolucion-de-rutas-y-movimiento-seguro.evidence.json) · [operación](../operations/moves.md)

## Qué se construyó

- **Dominio** (`larm-domain/src/artifacts.rs`):
  - `ArtifactLocation` según el schema;
  - rutas relativas validadas: nada absoluto, ni unidad, ni ADS, ni `.`/`..`, ni nombres reservados;
  - resolutor de la raíz desde el GUID del volumen: misma letra, letra cambiada, sin letra (`\\?\Volume{…}`) o montaje en carpeta; con el volumen desconectado no inventa una ruta;
  - fases del journal y DTO `Move`/`Plan`.
- **Plan** (`larm-storage/src/artifacts.rs`, puro):
  - `RENAME` en el mismo volumen y `COPY` entre volúmenes;
  - `EXTERNAL`, `HOST_MANAGED` y los blobs compartidos se quedan en su sitio (`KEEP`);
  - locks en orden global;
  - espacio solo de las copias (P015);
  - hash RFC 8785 que incluye cada `row_version`.
- **Job** (`larm-storage/src/move_job.rs`):
  - fases `PLANNED → LOCKED → COPYING → VERIFIED → DB_SWITCHED → ACCESS_TESTED → SOURCE_DELETED → COMPLETED`;
  - copia a `staging/<job>/<artifact>.partial` con flush y SHA-256 releído;
  - cambio de registro con CAS y fencing en una transacción, con journal y evento `artifact.moved`;
  - prueba de acceso resolviendo desde el registro nuevo;
  - borrado del origen solo con el lock;
  - recuperación al arrancar según la fase.
- **SQLite** (`larm-db/src/artifacts.rs`): repositorio de `artifact_location` y journal de movimientos.
- **API**:
  - `POST /api/v1/models/{id}/move` con el id de la variante: plan en seco o ejecución por `plan_hash`; el job se sigue en `/jobs/{plan_id}`;
  - `GET /api/v1/artifact-locations[/{id}]`;
  - `openapi.json` regenerado (29 operaciones implementadas).

## Cambios en P019 y P014

- **Guard**:
  - la lease viva de una instancia de la variante se comprueba dentro de la transacción que toma el lock exclusivo (spec §38);
  - `JournalStep.from_phases` hace que un plan `PLANNED` solo se ejecute una vez.
- **Defecto heredado de P012/P014, encontrado por la prueba real**: un sondeo que empezó antes de crearse una ubicación de red la juzgaba con una observación que nunca sondeó su recurso, y la marcaba OFFLINE. Ahora cada sondeo solo decide sobre las ubicaciones que conocía al empezar. Hay prueba de regresión, y la mutación que quita el arreglo la hace fallar.
- **Alta de ubicaciones**: guarda `metadata.volume_relative_path` para poder reconstruir la raíz si cambia la letra.

## Decisiones

- **Sin aprobación obligatoria** (`approval_required: false`). Un movimiento no destruye datos: el origen solo se borra tras verificar la copia, cambiar el registro y abrir el destino. El contrato 03 reserva la aprobación para borrados, licencias y código. Si llega un `approval_id`, se consume ligado al `plan_hash` y a los recursos del plan; nunca se ignora.
- **Respuesta síncrona**: la operación es `sync` con 201 `Plan` (contrato). El checklist pide un job asíncrono con progreso, así que el trabajo corre en segundo plano y `plan_id` es el id del job.
- **Bloqueos**: volúmenes compartidos; variante y artefactos movidos en exclusiva. Bloquear el volumen entero en exclusiva pararía cualquier otro uso del disco.
- **Blobs compartidos**: no se mueven. Su `artifact_location` es una sola fila para todas las variantes, así que moverlo arrastraría a las demás sin sus locks.

## Pruebas

| Prueba del prompt | Dónde |
|---|---|
| Same-volume move | `mismo_volumen_renombra` y real `mueve-mismo-volumen` |
| Cross-volume (simulado) | `otro_volumen_copia_verifica_y_borra`; real C: ↔ `\\localhost\Temp` |
| Hash mismatch | `hash_distinto_conserva_el_origen` y real `hash-distinto` (origen alterado con el mismo tamaño) |
| Destination disappears | `destino_desaparece_antes_del_commit` (borra la carpeta real justo antes del cambio) y `caida_tras_cambio_con_destino_perdido_vuelve_al_origen` |
| Active lease blocked | `lease_activa_bloquea_y_al_soltarla_se_mueve`, prueba del guard y real `lease-activa-bloquea` |
| Crash/recovery from journal | Caídas en COPYING, VERIFIED, renombrado, DB_SWITCHED y ACCESS_TESTED (se aborta la tarea bloqueada en el disco y se arranca la recuperación), y real con `TerminateProcess` copiando 1 GiB |

**Negativos**:

- schema: campo extra, `plan_hash` o ids con formato inválido;
- autorización: 401 y 403;
- falta de `Idempotency-Key`;
- plan: sin hash, inventado, de otro destino, obsoleto, caducado o ya usado;
- variante, destino u origen ausentes;
- otro titular del lock (`CONFLICT`);
- aprobación desconocida, de otros recursos o ya usada;
- acceso fallido tras el cambio.

**Mutaciones**: cada una se aplicó, se ejecutaron las pruebas y se restauró el código. Las 9 hacen fallar al menos una prueba:

1. sin comprobar la lease;
2. cambio de registro sin fencing;
3. borrado del origen sin fencing;
4. sin verificar el hash;
5. sin comprobar el destino;
6. sin devolver el registro al origen;
7. sin retirar la copia colocada;
8. paso del journal sin restricción de fase;
9. sondeo sin el filtro de ubicaciones conocidas.

## Prueba real (`tools/p016_real.py`, 11 pasos PASS)

Se ejecutó con el daemon en el PC IA. Las ubicaciones se crearon por la API. Las filas de variante, artefacto y lease se sembraron con sqlite3, porque su API llega en P040, P041 y P100.

1. Plan en seco en C: con `RENAME` y `KEEP(external_preserved)`, sin rutas absolutas.
2. Renombrado de un modelo de 64 MiB con su sha256 intacto y el evento `artifact.moved`.
3. Copia verificada a `\\localhost\Temp` con reserva de 1,06 GiB durante el job; al terminar no quedan locks ni reservas.
4. Lease ACTIVE: 409 `ACTIVE_LEASE` y nada se mueve. Con la lease RELEASED, el mismo plan devuelve el modelo a C:.
5. Plan obsoleto: 409 `STALE_REVISION` y el plan pasa a `CANCELLED`.
6. Hash distinto real: `INTEGRITY_FAILED` y el registro queda intacto.
7. `TerminateProcess` con 72 MiB copiados de 1 GiB: el journal queda en `COPYING` con 5 locks. Al rearrancar, `FAILED` `job_interrupted`, staging borrado, orígenes íntegros y ningún lock.

## Bloqueado

| Prueba | Por qué |
|---|---|
| **G05** (dos volúmenes físicos con cortes tras cada fase) | `\\localhost\Temp` es otra `volume_key`, pero está en el mismo disco. Falta el segundo volumen (bloqueo de P000) |
| **Letra cambiada en real** | Necesita un volumen cuya letra se pueda cambiar. El resolutor está probado con observaciones |
| **Lease real de P100** | P100 no existe. La lease de la prueba es una fila sembrada |

## Pendiente

- **Interfaz «Mover a…»**: plan, lease bloqueante y fases. La pantalla de modelos aún no existe (P040+ y G05/P123). La API y el job ya publican todo lo que necesita.
- **Carpetas vacías**: tras borrar un origen, sus carpetas vacías se quedan en la raíz.
