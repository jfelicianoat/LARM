# P014 — StorageLocation registry y políticas de colocación

**Revalidación incremental 2026-10-16:** corregida INC-01 (reserva superior a capacidad) y el refresco/layout de la pantalla. `tools/p014_real.py serve` verifica 13 pasos reales, incluidos rechazo en edición sin cambiar revisión y reserva igual a capacidad aceptada. El informe y las transcripciones actuales están en [la revalidación de auditoría](AUDIT_FIXES_2026-10-16.md). Se conserva el estado contractual BLOCKED_REAL por el segundo volumen y dependencias; no se atribuye REAL_PASS a esas pruebas pendientes.

**Revalidación real 2026-10-25:** `tools/p014_real.py serve` vuelve a pasar **13/13 pasos** con el daemon real. El recibo actualizado y su hash están en [evidencia](P014_storagelocation-registry-y-politicas-de-colocacion.evidence.json) y [transcripción](P014_runs/revalidation-20261025-022400/serve-steps.json). P014 sigue `BLOCKED_REAL`: el apagado de un segundo volumen físico continúa pendiente.

**Fecha**: 2026-10-11 · **Host**: A9_MEGA (PC IA) · **Estado**: **BLOCKED_REAL**: 24 PASS (3 reales) y 1 BLOCKED. Falta la prueba real de un volumen offline (no hay segundo volumen) y las dependencias P004 y P012 no tienen REAL_PASS. [evidence.json](P014_storagelocation-registry-y-politicas-de-colocacion.evidence.json) · [operación](../operations/storage.md)

## Qué se construyó

- **Dominio** (`larm-domain/src/locations.rs`):
  - `StorageLocation` y `StorageLocationCreate`, con la forma exacta de los schemas;
  - los ocho estados;
  - nivel de disco H09 (comparación estricta y aritmética en u128);
  - espacio utilizable (`libre - reserva`, nunca negativo) y presupuesto por volumen, que usa la mayor reserva de sus raíces;
  - las seis políticas de colocación con sus hard gates. CUSTOM_RULES es solo modelo de datos, como pide el prompt.
- **Estado observado**: `location_state` añade MISSING (volumen presente, carpeta raíz ausente) y LOW_SPACE (nivel CRITICAL). Un puerto `RootInspector` comprueba las raíces con timeout; si no puede, no inventa MISSING. Los umbrales salen de `Settings`.
- **Servicio** (`larm-storage/src/locations.rs`): valida la raíz antes de guardar:
  - sintaxis: absoluta o UNC, sin dispositivo, ADS ni `..`;
  - existencia, y que sea una carpeta;
  - ningún reparse point en la ruta;
  - escritura con un fichero propio que se borra;
  - sin solaparse con otra ubicación;
  - identidad del volumen por el punto de montaje más largo que la contiene, o por la raíz UNC.

  Además protege las ubicaciones con artefactos frente a cambios de raíz y de volumen, y añade a las respuestas `metadata.volume_budget`.
- **Persistencia** (`larm-db/src/locations.rs`):
  - CRUD con `If-Match`;
  - `storage.state_changed` con `action` en el outbox en la misma transacción;
  - el borrado se niega con artefactos (`location_has_artifacts`) o con runtimes o descargas que la usan (`location_in_use`).

  Los jobs del journal se generalizaron (`SqlJournalJobs::hardware` y `::storage`).
- **API**: las 8 operaciones del contrato. Incluyen los alias `/storage` y `/storage/locations`, filtros `q` y `state` con cursor ligado a los filtros, y `POST /storage-locations/{id}/probe` como job, que se recupera tras un reinicio.
- **Escritorio**: pantalla Almacenamiento con:
  - estado en texto, espacio utilizable y presupuesto compartido;
  - el identificador del volumen, de solo lectura;
  - alta y edición con la reserva en GiB y los bytes exactos a la vista;
  - mensajes claros para cada rechazo del servidor;
  - análisis como job;
  - quitar con confirmación, avisando de que no se borran ficheros.

## Defectos encontrados al probar

- **Presupuesto compartido incoherente**: cada raíz guarda el espacio libre que vio, así que dos raíces del mismo volumen daban presupuestos distintos. La prueba real falló de forma intermitente. Ahora el libre del volumen es la observación más reciente entre sus raíces; hay una prueba unitaria y tres ejecuciones reales seguidas en verde.
- **La prueba real de P012 usaba raíces inexistentes** (`C:\p012\…`). Con MISSING ya implementado, esas filas pasan a MISSING, que es lo correcto. Se corrigió la prueba para usar carpetas reales y se regeneró su evidencia.

## Prueba real en el PC IA (`tools/p014_real.py serve`, 13 pasos)

- **Alta**: identidad real de C:. GUID igual a `GetVolumeNameForVolumeMountPointW`, capacidad exacta, NVME observada. El replay idempotente devuelve lo mismo.
- **Rechazos antes de guardar**: carpeta inexistente, fichero, junction real creada con `mklink /J`, ruta relativa, ADS y solapamiento.
- **Presupuesto**: dos raíces en C: dan un utilizable igual a libre menos la mayor reserva. Una reserva de 9e18 bytes deja 0 en ambas.
- **Red**: `\\localhost\Temp\larm-p014-…` da `volume_key` `\\localhost\temp`, sin GUID, con `storage_type` NETWORK.
- **MISSING real**: tras borrar la carpeta, `POST /probe` (job) marca MISSING; al recrearla vuelve a ONLINE.
- **If-Match**: 428 si falta y 409 si la versión es vieja.
- **Artefactos protegidos**: con un artefacto, cambiar la raíz y borrar dan 409. Sin él, el borrado da 204 y quedan los eventos `created` y `deleted`.
- **LOW_SPACE real**: segundo arranque con umbrales BYTES por encima del libre de C:.
- **Limpieza**: la carpeta creada en el recurso compartido se borra.

## Bloqueado

`P014-REAL-volumen-offline` necesita un segundo volumen escribible para desconectarlo: disco externo o VHDX, que crear requiere autorización del operador. Es el mismo bloqueo de P000. OFFLINE está probado con observaciones sobre SQLite real.

## Pendiente

- Notificaciones LOW_DISK por transición de nivel (H09): P113/P128.
- DEGRADED y ERROR existen, pero nada los produce todavía.
- Uso de `place()` en el preflight de descargas: P015.
