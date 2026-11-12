# P098 — previsualización e importación local

Fecha: 2026-10-23  
Estado: `BLOCKED_REAL`; preview e importación durable de carpetas locales implementados, falta la recuperación sin base de datos y la aceptación real.

## Implementación

La revisión contractual 3.2.0 añade `POST /api/v1/models/import/preview`, una operación read-only con scope `models:write`. El daemon usa el recorrido local determinista, lee como máximo 64 KiB por archivo, no sigue enlaces o junctions y limita a 100.000 el total de entradas. La respuesta contiene rutas relativas, clasificación de variantes, tamaños exactos, archivos ignorados y motivos de omisión. El preview no calcula hashes ni cambia archivos, registros o trabajos. El escáner contextual reconoce además bundles CTranslate2 (cabecera `model.bin`, `config.json` y vocabulario), NCNN (cabecera `.param` y `.bin` compañero) y MNN (estructura FlatBuffer `.mnn` y sidecars); un `.bin` suelto no se promueve a modelo.

POST /api/v1/models/import usa la operación P098 existente, exige models:write e Idempotency-Key, y encola el DTO estricto en SQLite. El journal, la respuesta idempotente y el evento de outbox se escriben en una transacción. La migración 0007 instala el binding local interno; no carga ni ejecuta adaptadores externos. Al procesar el job, el worker canonicaliza la carpeta y valida su pertenencia a la ubicación en modo ADOPT; el resolver crea en una transacción una fuente local automática por carpeta, o reutiliza la existente para esa ruta, y escribe su evento de auditoría. Todas las distribuciones importadas quedan enlazadas a esa fuente, cuya URL file: permite distinguir procedencias locales. Un consumidor persistido valida que el destino esté online y usa el importador existente para adoptar o copiar archivos. format filtra las variantes detectadas y license_id se guarda como declaración del operador, sin afirmar que se haya verificado la licencia. Los jobs quedan consultables por su propietario. En arranque, los jobs RUNNING se cierran con job_interrupted; los QUEUED se mantienen para su consumo.

“Mis modelos” permite revisar la carpeta, elegir formato y destino, ver qué variantes excluye el filtro y confirmar el número seleccionado antes de crear el job. Cambiar carpeta o parámetros invalida la revisión y la confirmación. La interfaz muestra el resultado del job.

## Verificación

La evidencia JSON registra 21 PASS, 0 FAIL y 1 aceptación REAL bloqueada. Pasan preview, endpoints, idempotencia y outbox, creación/reutilización de fuentes por carpeta, vínculo de procedencia en las distribuciones, filtro de formato y licencia, migración, recuperación de trabajos interrumpidos, worker sobre SQLite con fixtures de GGUF, CTranslate2, NCNN y MNN, reconstrucción de variantes y ubicaciones desde manifiestos en una base nueva, OpenAPI, escritorio, lint, tipos, build, Clippy, formato y verificación/sincronización del paquete. Esas fixtures verifican clasificación e importación, no la carga de pesos por los runtimes. Revalidación del 2026-10-23: `cargo test -p larm-db --all-targets -j1` y `cargo test -p larm-models --all-targets -j1` pasan completos con TEMP/TMP dentro del workspace permitido; los tres casos de backup/restore pasan en esa configuración. La suite actual del escritorio pasa 84/84 pruebas en 9 archivos, junto con ESLint, TypeScript y build de producción.

El 2026-10-23 se confirmó que la importación debe crear automáticamente la fuente local y reutilizarla para la misma carpeta canónica. La decisión queda cubierta por `cargo test -p larm-db --lib crea_una_fuente_por_carpeta_y_reutiliza_la_misma_en_reintentos -j1` y `cargo test -p larm-models --test import_worker -j1`; ambos pasan.

La pantalla de importación ahora explica que la fuente local se crea o reutiliza al confirmar, y que la procedencia queda enlazada a cada modelo. La aceptación de importación explícita, la fuente automática por carpeta y la reutilización quedaron revalidadas; la suite actual del escritorio pasa 84/84 pruebas en 9 archivos.

## Pendiente

P098 permanece abierto: falta la aceptación REAL con bundles representativos autorizados, recovery operativo de conflictos/huérfanos junto con backup del registro y fault injection completa. La reconstrucción de sidecars funciona en una base de staging nueva y ya tiene prueba de integración, pero falta recorrer el comando completo de recuperación bajo condiciones reales. Faltan además la prueba en varios volúmenes y la validación real de symlinks/junctions; Windows sandbox no permite crear junctions para el test del walker. P017 y P041 tampoco tienen `REAL_PASS`, por lo que el flujo no se promociona a `REAL_PASS`.

[Revisión contractual del preview 3.2.0](../contracts/REVISION_3_2_0_P098.md) · [evidencia](P098_importadores-de-todos-los-bundles-y-recuperacion-sin-base-de-datos.evidence.json)
