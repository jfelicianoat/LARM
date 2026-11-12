# P093 — intentos cercados, cierre físico y reinicio transaccional

Fecha: 2026-11-01. Contrato vigente: 3.13.0, ya autorizado. Objetivo completo de LARM activo.

## Cambios

SQLite reclama una transformación QUEUED en una transacción con journal, evento y outbox. El primer claim cuenta el intento 1; el claim de un reinicio conserva el contador incrementado al aceptarlo. Un nonce UUIDv7 identifica la reclamación y queda ligado al job, plan, generación y número de intento. La admisión conserva también identidad y ubicaciones físicas del input; el lector comprueba su coherencia con el plan, el journal y el conjunto exacto de guards/reservas.

El cliente nativo conserva un binding de reclamación durante toda su vida. No permite volver a etiquetar el mismo proceso con otro intento. Su guard toma prestado el propietario real del proceso; no se deserializa desde HTTP. La comprobación de cierre consulta el drenaje efectivo de proceso/árbol/stdin/stdout/stderr. Permanece disponible aunque ya no sea válida la cualificación para nuevas llamadas.

Las comprobaciones previas al IO y la transición a VERIFYING revalidan la reclamación y el binding de la sesión. El cierre fallido sólo retira sus guards y reservas después de acreditar el cierre físico. Estado, causa redactada, journal, evento/outbox y liberación se confirman juntos; escrituras omitidas o fallos posteriores revierten todo. El primer error se conserva al repetir el cierre. Una respuesta antigua no puede cerrar ni retirar recursos de la generación nueva. Un puntero inesperado a un output impide continuar IO o liberar recursos desde ese registro incoherente.

El reinicio conserva en su historial la reclamación, el cierre y las ubicaciones anteriores. La recuperación existente puede señalar FAILED_RECOVERABLE mientras el proceso sigue vivo, pero ese estado no autoriza liberar sus recursos. La reconciliación de recursos exige el propietario físico cerrado.

## Fallo real encontrado y corregido

La primera aceptación nativa falló con `reference_transform_io_failed` / PermissionError. Una reproducción pequeña confirmó Windows error 32 al mover un directorio hijo desde staging a destino: la retención anterior de carpetas impedía esa publicación. Otra reproducción confirmó el caso de input y output en una misma raíz.

Se distinguen ahora tres usos de los handles: paquete inmutable, archivo de entrada y directorio de asignación. Los archivos originales conservan exclusión de escritura y DELETE; sus directorios y las carpetas de asignación permiten los cambios necesarios en los nombres de sus hijos, manteniendo la exclusión de DELETE/reemplazo del propio directorio. Se siguen comprobando atributos y reparse points en cada checkpoint. La política estricta de los paquetes no cambia.

Las seis pruebas de archivos comprueban creación/movimiento de staging, publicación junto al original, original sin cambios, imposibilidad de sobrescribirlo/trasladarlo o renombrar su carpeta, escritor previo, reapertura y liberación de handles. Las pruebas anteriores de junctions y hashes físicos vuelven a pasar.

Se conservan los intentos y reproducciones:

- [Intento nativo inicial](P093_runs/attempt-lifecycle-20261101/attempt1/verification-synopsis.json): 195 PASS / 1 FAIL; fuentes estables. Detecta el conflicto de compartición.
- [Publicación entre raíces RED](P093_runs/attempt-lifecycle-20261101/workspace-publication-red.log) y [primera corrección GREEN](P093_runs/attempt-lifecycle-20261101/workspace-publication-green.log).
- [Raíz compartida RED](P093_runs/attempt-lifecycle-20261101/shared-root-red.log) y [seis casos GREEN](P093_runs/attempt-lifecycle-20261101/shared-root-green.log).
- [Segundo intento](P093_runs/attempt-lifecycle-20261101/attempt2/verification-synopsis.json): 195 PASS / 1 FAIL. El recorrido y sus cierres pasan; falla la exportación de eventos de la prueba por usar `data` en vez de la columna real `payload`. Se corrige la consulta. La aceptación final completa se repite después.

## Pruebas y auditoría finales

**16/16 comprobaciones PASS; 269 pruebas PASS, 0 FAIL, 0 ignoradas.** Los casos filtrados de las ejecuciones focales no se cuentan como aprobados.

| Grupo | Pruebas PASS |
|---|---:|
| Dominio | 172 |
| Planes, admisión, reinicio y ciclo de intentos SQLite | 43 |
| Recuperación anterior | 4 |
| API de transformaciones y OpenAPI | 16 |
| Cliente tipado | 3 |
| Aceptación nativa original y consumidor/ciclo Core | 1 |
| Catálogo de entrada SQLite | 3 |
| Archivos físicos de entrada | 8 |
| Archivos/directorios retenidos y fronteras de rutas | 9 |
| Almacenamiento con sondeo nativo | 10 |

La aceptación nativa incluye de nuevo la receta P026 original: 176 casos funcionales, 16 de transporte, 17 observaciones y nueve casos del gate físico. Además, la prueba conecta la admisión y las reclamaciones SQL con el proceso cualificado y el observador real de almacenamiento Windows. El catálogo de entrada VERIFIED es una fixture y el mapeo del estimate privado al volumen global se construye explícitamente en la prueba; esto no sustituye al productor público.

Un único job recorre dos intentos. El primero se reconcilia llamando a la recuperación mientras su worker sigue vivo, acredita que sus recursos continúan retenidos y los libera sólo tras cerrar el proceso. Con otro plan se reinicia el mismo job, entra en el intento 2 y rechaza el identificador anterior. El segundo ejecuta la conversión real; se altera deliberadamente un byte del output y se registra el fallo de integridad, sin crear una variante de salida. El original permanece intacto. Al terminar quedan un job, una variante de entrada y cero reservas/locks.

La auditoría independiente contrasta las revisiones 1..8 y sus eventos, ambos nonces/generaciones, contador 1→2 sin doble incremento, identidad del input, primer error e historial. Reconstruye los hashes RFC8785 del Run, perfil y manifiesto; verifica los bytes Base64, procedencia y SHA-256 conservados y el cambio de exactamente un byte en el output dañado. Entrada: 12.582.917 bytes; dato codificado: 16.777.224 bytes.

Clippy para dominio/DB/host/transformación/daemon, todos sus targets y avisos como errores, formato, arquitectura de 17 crates y 79 copias contractuales pasan. La auditoría comprueba **847 entradas, 16 logs y 16 recibos** por SHA-256, sin cambios de fuente durante la ejecución. El wrapper terminó con código 0. No hay procesos de este recorrido pendientes de cierre.

[Resultados finales](P093_runs/attempt-lifecycle-20261101/attempt3/verification-synopsis.json) · [auditoría](P093_runs/attempt-lifecycle-20261101/attempt3/post-run-audit.json) · [datos nativos retenidos](P093_runs/attempt-lifecycle-20261101/attempt3/native-transformation.json) · [ejecutor](P093_runs/attempt-lifecycle-20261101/verify.py) · [auditor reproducible](P093_runs/attempt-lifecycle-20261101/audit.py).

## Trabajo pendiente

El servicio público sigue devolviendo `worker_unavailable`. Faltan el productor/consumidor de Core conectado al daemon, la verificación/publicación atómica de outputs y smoke, la recuperación de ownership de procesos después de reiniciar realmente el daemon y el reinicio confirmado desde la UI. La reconciliación nativa de esta prueba se invoca dentro del mismo proceso y conserva el propietario vivo: **no es una aceptación H07 de crash/reinicio del daemon**.

Quedan también el seguimiento de asignaciones/bytes por fase y los límites de fichero FAT32. La conversión binaria/Base64 controlada no acredita GGUF, ONNX ni runtimes reales. P093 conserva preparación/BLOCKED_REAL, sin CODE_PASS integral ni REAL_PASS; las puertas P016/P040/P051 y el objetivo completo de la app siguen pendientes. La autorización 3.13.0 está resuelta y no se vuelve a solicitar.
