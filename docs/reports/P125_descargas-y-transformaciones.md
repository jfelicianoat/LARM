# P125 — Descargas y transformaciones

**Estado:** `BLOCKED_REAL` — lectura, prioridad y controles de descarga implementados; faltan las aceptaciones completas de P051/P093.

Ampliación posterior del 2026-10-27: [caudal del Core y CAS concurrente](P051_h08_core_cas_2026-10-27.md). Dos consumidores transfieren bytes TCP reales con contraste sin límite y respetan presupuesto agregado/ráfaga, cambio persistido, pausa y reapertura SQLite. Ocho pares adicionales completan 16 jobs. Se reproduce y corrige la carrera Windows al publicar el mismo blob; exige hash/tamaño antes de reutilizarlo y rechaza corrupción sin retirar el parcial. Pasan motor 50/50, storage 32/32 y daemon/P048 original 1/1 en 192,81 s, con cualificación 39/16 y regresión de integridad/pausa/reinicios/CAS. Clippy/fmt y 74 copias pasan; 40 inputs/binarios contrastados. La fuente y los planes del ensayo Core son de test: falta reunir dos transferencias cualificadas, PATCH y kill/reinicio en un mismo H08 del daemon. Frontend, workspace y runners completos no se repiten; los informes anteriores conservan su alcance histórico.

Ampliación posterior del 2026-10-27: [Settings y descarga P048 TCP](P003_settings_file_2026-10-27.md) pasan 49 pruebas focales y dos nativas (6,54 s y 182,98 s). La API usa la misma prioridad del fichero que el arranque y conserva la base guardada; sin override de fichero, P048 mantiene 65.536 bytes/s después de dos reinicios y recupera con bloques acotados hasta SHA/CAS/procedencia. Se repiten confirmación/replay, rechazo de corrupción y cierre de reservas/outbox. Cualificación 39/16, Clippy/fmt y package_copy 74/74 pasan; 37 inputs y binarios contrastados. Continúan pendientes H08 agregado, disco lleno, ingestión P043, GUI conjunta y dependencias; frontend/workspace/runners completos no se repiten. Los recibos anteriores conservan su alcance histórico.

Ampliación 2026-10-27: la [prueba del daemon TCP con P048 original](P048_daemon_tcp_2026-10-27.md) pasa 1/1 en 103,93 s, con cualificación real del perfil controlado de 39 casos/16 observaciones. El Core rechaza corrupción sin publicar ni registrar CAS/procedencia, conserva una pausa tras reiniciar y recupera otra caída en RUNNING con Range/If-Range hasta completar 1.048.832 bytes con SHA/CAS/procedencia. Preview, confirmación y replay pasan; ambos journals y sus reservas/outbox quedan cerrados, principal temporal revocado. Clippy y sintaxis pasan y se contrastan 28 inputs y ambos ejecutables. El catálogo se prepara desde metadatos RPC; faltan ingestión P043, GUI conjunta y las demás aceptaciones reales. Este incremento no repite frontend, workspace ni el runner formal completo. Los incrementos anteriores conservan su alcance histórico.

## Incremento 2026-10-26

La [ampliación HTTP 416](P051_http416_2026-10-26.md) pasa dos fases Windows 2/2 (1.461,20 s): confirma integridad antes de publicar un parcial completo, rechaza el alterado y reinicia el rango prematuro. Repite las cancelaciones y la recuperación del daemon; motor 48/48 y P048 20/20. La prueba usa API real con fuente de referencia cualificada; frontend y GUI→daemon conjunta conservan su verificación anterior.

La [ampliación de integridad del daemon](P051_integrity_2026-10-26.md) pasa 1/1 en 528,44 s: ETag cambiado reinicia a cero, y un archivo corrupto termina FAILED sin publicación y con journal/reserva cerrados. La otra descarga pausada se conserva y se repiten controles/reinicio. Pasa 141 pruebas focales de DB/motor/P048; no se repitió frontend ni la aceptación GUI→daemon conjunta.

La [ampliación posterior de cancelación activa](P051_active_cancel_2026-10-26.md) pasa 1/1 en Windows (393,21 s): confirma el cierre del árbol del adaptador al observar `CANCELLED`, limpia su parcial/reserva y conserva otra descarga pausada. El host compartido pasa 57/57 pruebas; Clippy y formato pasan. La interfaz conserva su verificación anterior: no se repitieron frontend ni GUI→daemon conjunto en esta ampliación.

Descargas ya permite pausar, reanudar y solicitar cancelación con confirmación explícita. Bloquea doble envío y recupera respuestas perdidas con la misma orden y clave, sin presentar el recibo 202 como estado final. Un worker dedicado limpia cancelaciones aunque el slot de transferencia esté ocupado; conserva la reserva ante un fallo de limpieza. La interfaz final pasa 126/126 pruebas, focal de cola/controles 13/13, lint y build/TypeScript. La revisión visual simulada de escritorio/375 corrigió el desbordamiento del selector de prioridad.

La integración Windows de P051 pasó 1/1 en 366,98 s: pausa con checkpoint de 131.072 bytes y reserva conservada, reanudación, cancelación de otro job en cola con el único worker ocupado, y recuperación después de terminar el daemon desde 196.608 bytes hasta completar 2 MiB verificados. La admisión del segundo job tardó 7.066 ms bajo el límite de 30 s; su cancelación tardó 524 ms. La UI y la API real se probaron por separado. [Informe y recibos del incremento](P051_controls_2026-10-26.md).

Las cifras de la verificación original que siguen son históricas; no se repitió aquí el runner formal ni la puerta global.

## Cambios

- Se añadió `GET /api/v1/downloads`, autenticado y paginado con cursor firmado ligado al actor. La consulta de SQLite filtra por propietario y ordena establemente por fecha e ID.
- Se regeneró `docs/api/openapi.json` para incluir la ruta como operación implementada; `openapi --check` confirma que el documento coincide con el servidor.
- El escritorio incorpora `#/descargas` con las vistas de descargas y transformaciones. La cola presenta el estado y el progreso disponibles; total desconocido, velocidad o ETA no calculables y verificaciones pendientes se conservan como tales.
- El detalle de transformaciones muestra los datos presentes —entrada, salida, herramienta y versión, parámetros, pico temporal y diagnóstico— y distingue los errores de hash normalizados.
- La pantalla conecta prioridad con CAS, pausa/reanudación y cancelación confirmada. Conserva el diagnóstico de las transformaciones sin ofrecer un reinicio hasta conectar su operación y nuevo preflight.
- La decisión para importar carpetas locales se implementó en el incremento de importación: crear una fuente local automáticamente y reutilizarla al reintentar la misma carpeta canónica.

## Verificación

El runner de P125 registra **12 PASS, 0 FAIL y 1 prueba REAL BLOCKED**. Las pruebas focales cubren autorización, paginación firmada y aislamiento entre actores en API/SQLite; la interfaz cubre total desconocido, error de hash, diagnóstico recuperable y navegación. La suite completa del escritorio pasa **90/90**. También pasan las suites de dominio, SQLite y API, los 18 tests del binario del daemon, lint, TypeScript, build, formato, Clippy y el chequeo de OpenAPI. Los checks de Tauri compilan y terminan correctamente; su binario no contiene tests. `package_copy` confirma **62 copias idénticas al paquete 3.7.0**.

La puerta oficial `tools/check.ps1 -Prompt P125 -Real`, repetida después del arreglo, pasa **14/14 checks**, incluido `cargo_test` para todo el workspace serial, la compilación y pruebas Tauri, frontend y evidencia P125. El recibo anterior de 13/14 queda como antecedente: `cargo_test` había encontrado un bloqueo transitorio de Windows al reemplazar el archivo de control mientras el observador Python lo leía. `ReferenceProtocolSuite::mode` ahora reintenta ese `PermissionDenied` hasta un segundo. Tras el cambio, el test protocolario pasa **1/1** y el target completo `reference_adapters` pasa **11/11** en **662,32 s**; el informe protocolario posterior registra 100 casos y 17 observaciones aprobadas ([evidencia](P026_runs/native-protocol/01a16c14-0587-77ee-8315-5afab04ef4f2/01a16c14-2504-776b-8dda-a10a574b969b/protocol.json)). También pasan `cargo fmt --all --check`, Clippy de `larm-adapter-host`/daemon y `validate_evidence.py`. La prueba de origen que antes recibió `STORAGE_INSUFFICIENT` con 2,6 GB libres pasa al repetirse con espacio suficiente y loopback local habilitado. La ejecución oficial actual está guardada en [check_P125_real.json](check_P125_real.json); el log de `cargo_test` está en [la transcripción del gate](check_runs/20261024T0413141388658Z/cargo_test.log).

## Contrato P052

La revisión solicitada ya está incorporada en el paquete canónico como contrato **3.7.0**. El schema admite `optional_artifact_ids`; el DTO del SDK los preserva y valida como UUIDv7 únicos. Los intervalos de tamaño pertenecen a `ArtifactManifest.artifacts[].size_estimate` (`ArtifactSizeEstimate`), no al request. No se añadió un segundo campo de tamaño al contrato.

## Pendiente para REAL_PASS

P051 y P093 siguen sin `REAL_PASS`. La API de descarga ya demuestra pausa/reanudación, cancelación activa y en cola, ETag cambiado, hash incorrecto y reinicio con fuente controlada. Faltan la aceptación GUI→daemon conjunta, proveedor real, disco lleno y el resto de la matriz HTTP integrados, medición H08 e interrupción/reinicio de transformaciones con nuevo preflight, guard y reserva.

Evidencia: [PromptEvidence](P125_descargas-y-transformaciones.evidence.json) · [puerta oficial](check_P125_real.json) · [última transcripción de puerta](check_runs/20261024T0413141388658Z/cargo_test.log).
