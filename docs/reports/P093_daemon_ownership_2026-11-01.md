# P005/P093 — propiedad del anfitrión antes de recuperar trabajos

Fecha: 2026-11-01. Objetivo global activo, contrato público 3.13.0. Incremento de coordinación nativa verificado; P093 sigue en preparación/`BLOCKED_REAL`, sin `CODE_PASS` integral ni `REAL_PASS`.

Durante la revisión de la recuperación se detectó que el servicio abría/migraba SQLite y reconciliaba trabajos antes de intentar ocupar el puerto. Dos procesos con la misma base podían alcanzar esa recuperación aunque uno terminase después por el puerto o el pipe. Se añade una propiedad física que precede a logging, migración, recuperación y creación de consumidores.

`DaemonOwnership` retiene la raíz y sus ancestros mediante los pins de directorio de Windows y abre un ancla con sharing exclusivo. Mantiene otra ancla en el directorio de la base. Esto cubre también una segunda raíz configurada contra una base absoluta o fuera de su raíz, sin depender del texto mostrado de la ruta ni del número de puerto. La exclusión del directorio de base es conservadora: bases independientes alojadas en el mismo directorio deben separarse en directorios distintos para servirlas simultáneamente.

El servicio exige el objeto opaco y comprueba que corresponda a la ruta de base que va a abrir. Una instancia competidora termina con `daemon_data_root_in_use` antes de logging/SQLite/recovery. Las anclas permanecen en disco; su presencia, contenido, PID o un flag `closed` no conceden autoridad. La propiedad efectiva reside en los handles retenidos, y Windows los cierra al terminar el proceso. Un nuevo propietario obtiene un epoch nuevo. También se rechazan junctions/reparse points y anclas que no sean archivos regulares. La implementación está dirigida a Windows y rechaza plataformas donde esta protección no esté disponible.

`db-restore` adquiere la misma propiedad de raíz y directorio de base antes de validar/copiar/sustituir el backup. Se conserva el backup online de SQLite. La restauración se rechaza mientras el servicio mantiene la base y vuelve a funcionar después de detenerlo.

## Evidencia nativa

Seis pruebas de archivos/handles comprueban exclusión y alias `.`; epoch nuevo tras cerrar; contenido antiguo/flags ignorados y conservados; bloqueo de reemplazo de raíz/ancla con IO permitido en hijos; raíces independientes; ancla de directorio rechazada; junction rechazada; binding de base y exclusión desde otra raíz.

Las ocho pruebas del ciclo de vida ejecutan el binario real. El nuevo recorrido inicia una API, añade un sentinel de descarga `RUNNING` y lanza otra API con la misma raíz y **otro puerto**. El segundo proceso sale con código 1 y el snapshot del job, progreso, reservas y eventos no cambia. Se repite desde otra raíz que señala la misma base absoluta, con el mismo resultado. Después se mata y espera el primer proceso, sin borrar el ancla; una nueva API adquiere la propiedad y recupera el sentinel una sola vez. Este sentinel es una fixture sin worker real de descarga.

Otros casos comprueban que un fallo de arranque por puerto ocupado libera la propiedad, y que una copia SQLite real se puede crear online, no restaurar con el servicio vivo, sí restaurar offline y abrir de nuevo con el servicio real. Las pruebas de sintaxis/configuración y de recuperación anterior conservan sus resultados.

**Esto acredita exclusión y recuperación del anfitrión. No acredita cierre del árbol de un worker P093 tras una caída.** El tipo no implementa `TransformationAttemptGuard` y su epoch no libera guards/reservas de transformación. El camino existente de recuperación conserva esa separación.

La investigación del siguiente paso usa la documentación de [Job Objects de Microsoft](https://learn.microsoft.com/en-us/windows/win32/procthread/job-objects) y la descripción oficial del [riesgo de crear un proceso y asignarlo después a un Job Object](https://devblogs.microsoft.com/oldnewthing/20230209-00/?p=107812). El host actual conserva sus objetos nativos/IO en memoria; faltan su ownership recuperable, el cierre físico acreditado tras una caída y el oráculo H07 con worker real. La propiedad exclusiva añadida aquí es un requisito previo para coordinar esa recuperación.

## Resultado congelado

**21/21 comprobaciones PASS, 359 pruebas PASS, 0 FAIL, 0 ignoradas**. Wrapper y auditor terminan con código 0. Se verifican 858 entradas de código/contrato y 21 logs/recibos sin cambios de fuente durante la ejecución. La auditoría independiente comprueba los snapshots y logs del anfitrión y reconstruye también las evidencias anteriores de transformación/CAS.

| Grupo | Pruebas correctas |
|---|---:|
| Propiedad nativa del host | 6 |
| Binario real: ciclo de vida | 8 |
| Tests internos del daemon | 49 |
| Regresión anterior de dominio, HTTP, SQL, host, almacenamiento y transformación | 296 |

La regresión conserva P026 original (176 casos funcionales, 16 wire, 17 observaciones y 9 gates físicos) y el recorrido Core/SQLite/CAS directo: tres intentos, trece eventos, diecisiete negativos de publicación, un derivado registrado, original intacto y cero reservas/locks finales. Su catálogo/preview/ámbito siguen siendo fixtures, sin productor público ni aceptación real GGUF/ONNX.

Clippy de dominio, DB, transform, host y daemon, todos los targets y warnings denegados: PASS. Formato: PASS. Arquitectura: 17 crates PASS. Copias contractuales: 79 PASS. La comprobación preliminar del ciclo de vida también pasó antes de ampliar la cobertura a base compartida y restauración. No hubo cambios ni aceptación GUI.

- [Resumen del conjunto](P093_runs/daemon-ownership-20261101/attempt1/verification-synopsis.json).
- [Auditoría independiente](P093_runs/daemon-ownership-20261101/attempt1/post-run-audit.json).
- [Recibo nativo del anfitrión y referencias a logs](P093_runs/daemon-ownership-20261101/attempt1/native-daemon-owner.json).
- [Regresión de transformación/CAS](P093_runs/daemon-ownership-20261101/attempt1/native-transformation.json).
- [Wrapper](P093_runs/daemon-ownership-20261101/verify.py) y [auditor](P093_runs/daemon-ownership-20261101/audit.py); cada ejecución requiere un nombre de intento nuevo.

Prioridad siguiente: ownership duradero del proceso/árbol/IO de transformación y prueba de caída/reinicio real H07, antes de componer el productor/consumidor público. Continúan pendientes la contabilidad de reservas por fase/volumen del journal, retención del output privado, reconstrucción de procedencia, FAT32 completo, conversores/hooks reales y UI. El servicio público P093 conserva `worker_unavailable`; este incremento no completa el desarrollo global de LARM.
