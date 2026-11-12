# P093 — Transformation Pipeline framework

Fecha: 2026-11-06  
Estado: BLOCKED_REAL

Desarrollo pausado a petición del usuario. Productor/consumidor público de referencia implementado, pendiente de validación completa. Corrida congelada: 358 pruebas PASS/una FAIL/cero ignoradas; doce comprobaciones PASS de trece ejecutadas, quince no ejecutadas y 890 entradas intactas. El caso público falla antes del primer plan por401 bearer_invalid del segundo principal recién creado; la autenticación tiene caché de cinco segundos. Queda ajustar la fixture a esa recarga, repetir el caso nativo y completar la regresión/auditoría. Fuentes y fallo preservados, sin procesos auxiliares abiertos en la consulta CIM final válida. No acredita CODE_PASS integral/REAL_PASS ni sustituye los recibos anteriores. Contrato3.13.0 autorizado intacto. [Punto de pausa](P093_public_transformations_pause_2026-11-06.md).

## Histórico de salida observada hacia publicación

Incremento actual P093: el supervisor produce el manifiesto opaco observado para su claim registrado y lo entrega al verificador físico; descriptor/fence/binding/contexto exactos y Recheck firmado antes/después de Verify. Nuevo intento4 del mismo job completa conversión de referencia, cierre nativo, medición, CAS y publicación transaccional; revisión19/diecinueve eventos, catorce negativos, historial conservado, input intacto y cero recursos finales. Auditor reconstruye16.778.016 bytes y rechaza una corrupción controlada. Final intento3: 488 pruebas PASS/0 FAIL/0 ignoradas, 27/27 comprobaciones, 880 entradas/27 logs auditados. Dos abortos de pila del comprobador quedan conservados; se corrige mediante escenario con datos propios en tarea independiente, sin aumentar pila. [Alcance y evidencias](P093_guardian_output_publication_2026-11-02.md). Próxima prioridad: productor/consumidor público, selección/preview/admisión y progreso/cancelación, después H07 completo por API con reinicio confirmado hasta publicación. Mantiene worker_unavailable, preparación/BLOCKED_REAL y sin CODE_PASS integral/REAL_PASS; objetivo global completo activo y contrato3.13.0 autorizado intacto.

## Histórico de intención durable y reconciliación automática

Incremento actual P093: intención durable antes del lanzamiento y reconciliación automática real del daemon antes/después de RUNNING. PREPARING/REGISTERED/CLOSED y eventos/outbox son atómicos con sus revisiones; ausencia/alteración de notificación revierte la operación. Helper exige intención exacta; usuario del backend distinto del propietario Core. El supervisor conserva anclas de exclusividad hasta terminar y retira testigos ya cerrados sin cambiar recursos. Tres interrupciones nativas del mismo trabajo: RPC pendiente, preparación sin registrar y cierre confirmado sin Release; revisiones5/9/13, historial conservado, input intacto y cero recursos finales. Final intento2: 488 pruebas PASS/0 FAIL/0 ignoradas, 27/27 comprobaciones, 878 entradas y 27 logs auditados; regresión previa completa PASS. [Alcance y evidencias](P093_native_intent_2026-11-02.md). Prioridad siguiente: productor/consumidor público, transporte de manifiesto observado hacia verificación/publicación, progreso/cancelación y H07 completo por API. Mantiene worker_unavailable, sin CODE_PASS integral/REAL_PASS; objetivo completo activo y contrato3.13.0 autorizado intacto.

## Histórico de propietario registrado

Incremento actual P093: propietario nativo registrado atómicamente con claim/journal y respuesta viva para recuperar autoridad; preparación sin ejecución previa al registro, input físico retenido, alcance de RPC fijado y Execute único. Prueba real de admisión Core y caída/reinicio del servidor API con controlador explícito: conserva owner/error/recursos, observa el supervisor persistido y solo libera tras cierre nativo; replay sin eventos extra y original intacto. Final intento3: 405 pruebas PASS/0 FAIL/0 ignoradas, 27/27 comprobaciones, 874 entradas y 27 logs auditados; regresión previa completa PASS. Corrige PIPE_BUSY concurrente y un error de lectura HTTP del comprobador, con intentos fallidos conservados. [Alcance y evidencias](P093_registered_guardian_2026-11-02.md). Próxima prioridad: intención durable del lanzamiento y productor/consumidor público con salida observada, progreso/cancelación y H07 de conversión/reinicio confirmado/publicación por API. Mantiene worker_unavailable, sin CODE_PASS integral/REAL_PASS; objetivo completo activo y 3.13.0 autorizada intacta.

## Histórico de RPC cualificado del supervisor

Incremento actual P093: supervisor con describe/plan/execute/verify cualificados desde SQLite de solo lectura y el mismo binario del daemon HTTP que acredita P026; identidad antes de handshake, acceso y perfil actuales, control privado firmado, nonces de un uso acotados y cierre después de terminar RPC/IO/árbol. Nativa nueva comprueba identidad incorrecta, firma ausente/inválida/repetida, perfil modificado, Base64/manifest/procedencia reales, propietario de prueba caído y reconexión con pins hasta retirada; no crea recursos Core. Final intento2: 393 pruebas PASS/0 FAIL/0 ignoradas, 27/27 comprobaciones, 869 entradas y 27 logs/recibos auditados; regresión previa completa PASS. [Alcance y evidencias](P093_qualified_guardian_2026-11-01.md). Falta ligar la autoridad al journal/fence real antes de ejecución, resolver claim/lanzamiento/activación, AttemptGuard, progreso/cancelación por id, consumidor público y H07 con job Core al caer el daemon API. Mantiene worker_unavailable, sin CODE_PASS integral/REAL_PASS; objetivo global completo activo y 3.13.0 autorizada.

## Histórico de supervisor nativo básico

Incremento anterior P021/P093: supervisor nativo recuperable como componente bajo nivel, con Job Object exterior heredado, Worker/IO retenidos y consulta viva firmada vinculada a descriptor/fence/acción/nonce. Tres pruebas nativas de caída del propietario/reconexión, clientes inválidos y caída del supervisor; nueve archivos de escritores contrastados. Siete unitarias y dieciocho pruebas del Worker, más la regresión anterior. Intento1 conserva el fallo de retirada con stdin abierto; corregido mediante lector dedicado. Final intento2: 387 pruebas PASS/0 FAIL/0 ignoradas, 26/26 comprobaciones, 866 entradas y 26 logs/recibos auditados. No concede P022 ni implementa AttemptGuard, journal, RPC remoto o API P093; no acredita H07 integral del daemon público. Próxima prioridad: ligar la autoridad de ejecución al journal antes de ejecutar y probar caída/reinicio real con recursos. [Alcance y evidencia](P093_native_guardian_2026-11-01.md). Sin CODE_PASS integral ni REAL_PASS; objetivo global activo y 3.13.0 autorizada.

## Histórico de propiedad del daemon

Incremento anterior P005/P093: propiedad nativa del anfitrión por raíz y directorio de SQLite antes de recovery, con binding de base y protección de restauración. Dos servicios/puertos y otra raíz contra la misma base no alteran el sentinel; matar/reiniciar el anfitrión permite adquirir de nuevo y recuperarlo una sola vez. Seis pruebas físicas, ocho del ciclo de vida real y cuarenta y nueve internas, junto a la regresión anterior. Final 359 pruebas PASS/0 FAIL/0 ignoradas, 21/21 comprobaciones, 858 entradas y 21 logs/recibos auditados. No acredita aún cierre del árbol/IO de transformación tras una caída: sigue siendo la próxima prioridad de H07 antes del productor/consumidor público. Regresión CAS/Core conserva un derivado, original intacto y cero recursos finales; faltan también reservas por fase/volumen, retención privada, reconstrucción de procedencia, FAT32, conversores/hooks reales y UI. [Alcance y evidencia](P093_daemon_ownership_2026-11-01.md). Sin CODE_PASS integral ni REAL_PASS; 3.13.0 autorizada.

## Histórico de publicación CAS

Incremento anterior: grafo durable previo a FS, staging por claim con lease exclusiva, CAS recuperable y registro/finalización atómicos. 296 pruebas PASS/0 FAIL/0 ignoradas, 18/18 comprobaciones, 856 entradas y 18 logs/recibos auditados. Tres intentos/trece eventos, diecisiete negativos, un derivado y cero reservas/locks; reentrada en el mismo proceso, sin H07 completo. [Evidencia histórica](P093_cas_publication_2026-11-01.md).

## Histórico de verificación física de salidas

Incremento anterior: límites privados sellados, coexistencia salida/CAS/control y captura física tras cierre/drenaje real; hooks con ausencia explícita. 284 pruebas PASS/0 FAIL/0 ignoradas, 17/17 comprobaciones, 852 entradas y 17 logs/recibos auditados. Dos intentos/ocho eventos, corrupción posterior rechazada, original intacto y ninguna salida publicada en ese conjunto. [Alcance y evidencia histórica](P093_output_verification_2026-11-01.md).

## Histórico de ownership de intentos

Incremento anterior: claim cercado con nonce, contador y snapshot de input; guard del proceso real y cierre fallido atómico tras drenaje, con historial y rechazo de callbacks antiguos. Corrige un bloqueo Windows de publicación de hijos en carpetas retenidas, preservando el original. Final 269 pruebas PASS/0 FAIL/0 ignoradas, 16/16 comprobaciones, 847 fuentes y 16 logs/recibos contrastados. P026 original más admisión/reinicio/cierre Core directos con proceso y almacenamiento reales: un job, dos intentos, ocho eventos, corrupción rechazada sin output publicado y cero reservas/locks finales. Fixtures de catálogo/preview explícitas; no H07 de reinicio real del daemon. [Alcance y evidencia histórica](P093_attempt_lifecycle_2026-11-01.md).

## Histórico de catálogo y cliente cualificado

Incremento actual: catálogo/selector físico y espacio nativo reciente, referencias explícitas y evidencia de adquisición para declaraciones desconocidas; cliente cualificado para las cuatro RPC de transformación, con guard retenido, permisos y cierre/drenaje. Final 63 pruebas PASS/0 FAIL/0 ignoradas, 12/12 comprobaciones; 843 fuentes y 12 logs/recibos contrastados. Receta P026 original y consumidor nativo Base64 pasan; la auditoría reconstruye Run/perfil/manifiesto, tamaños/hashes y bytes de salida conservados. Cero jobs/reservas/locks de transformación de Core creados. Faltan productor/consumidor de Core, claim/cierre/recovery, outputs/smoke y UI; el servicio público mantiene `worker_unavailable`. [Alcance actual](P093_qualified_worker_2026-11-01.md) · [recibos](P093_runs/qualified-worker-20261101/attempt1/verification-synopsis.json). Sin CODE_PASS integral ni REAL_PASS; 3.13.0 autorizada.

## Histórico de revisión y reinicio

Incremento actual: autorización3.13.0 resuelta y contrato publicado. Ruta de reinicio explícito con CAS, scopes/headers/DTO cerrado; SQLite conserva el mismo job y su input/historial, obtiene recursos nuevos tras el cierre anterior y confirma plan/journal/outbox/idempotencia juntos. Final222pruebas PASS/0FAIL/0ignoradas,11/11 comprobaciones,34inputs y11logs contrastados. Clippy dominio/DB/API, check del daemon, formato/arquitectura y79copias PASS; paquete3226controles/53negativos/153operaciones PASS. [Alcance y pendientes](P093_restart_3_13_0_2026-10-30.md) · [recibos actuales](P093_runs/revision-3-13-0/attempt2/verification-synopsis.json).

La composición pública mantiene el rechazo por motor no disponible hasta conectar selector/volumen/espacio y productor/supervisor del worker. Reinicio nativo, outputs/smoke, recovery de recursos y acción de interfaz pendientes. El contrato3.13.0 está autorizado: no requiere volver a preguntar por ese alcance. No hay CODE_PASS integral ni REAL_PASS.

## Incrementos previos de preparación

El incremento actual incorpora admisión inicial atómica con revalidación de catálogo/cualificación/input, espacio reciente, presupuesto, guards/reservas, aprobación e idempotencia. Añade hashes físicos y handles de archivos/ancestros Windows. Reproduce y corrige una aceptación parcial ante una escritura obligatoria omitida. Verificación final11/11 PASS,237 pruebas PASS/0FAIL/0ignoradas,34inputs y11logs contrastados. Clippy cuatro crates, compilación de comprobación del daemon, formato, arquitectura17 y copias78 PASS. [Cambios, pruebas y límites](P093_admission_inputs_2026-10-30.md) · [recibos actuales](P093_runs/admission-inputs-20261030/verification-synopsis.json).

El incremento inmediatamente anterior de binding/previews y reintento sin mutación al fallar conserva sus194 pruebas y snapshot previo. [Informe histórico del componente](P093_bound_plans_2026-10-30.md).

En ese snapshot anterior el servicio público todavía no conectaba worker/admisión/verificación y la propuesta3.13.0 estaba pendiente. Su autorización y publicación posteriores se recogen arriba. Los recibos anteriores conservan el contrato y las fuentes de cada ejecución.

## Histórico del 23 de octubre

Los recibos formales y el gate general siguientes pertenecen a las fuentes de esa fecha; no certifican los cambios actuales.

P093 añade el contrato de dominio para planes y trabajos de transformación, DTOs estrictos y rutas API para crear planes, confirmar ejecuciones con idempotencia y consultar jobs. SQLite persiste actor, correlación, request, plan sellado y hashes de entrada. Al arrancar, la recuperación convierte interrupciones RUNNING/PAUSING/VERIFYING en FAILED_RECOVERABLE junto con causa, journal, evento y outbox dentro de una transacción; no publica un output parcial. La migración 0008 habilita el estado y su índice de recuperación. OpenAPI publica las tres operaciones.

El grafo de crates publicado permanece intacto: DTOs e invariantes de transformation viven en `larm-domain`, su API usa el puerto de aplicación del dominio, y `larm-api`/`larm-db` no añaden dependencias internas nuevas. La revisión del código y las pruebas muestran un límite funcional pendiente: el repositorio todavía no conecta el worker al preflight de espacio pico, guards/reservas, staging ni verificación de artefactos. Por ello las escrituras devuelven UPSTREAM_UNAVAILABLE antes de crear filas. Tampoco está implementado el reinicio durable de FAILED_RECOVERABLE bajo un plan y preflight nuevos. El adaptador de referencia protegido sí ejecuta un proceso local, rechaza accesos fuera de permisos y verifica el bundle; esa prueba no sustituye el job público del daemon.

La evidencia focal contiene 9 pruebas PASS, 0 FAIL y 2 aceptaciones reales bloqueadas. Incluye tests del dominio, API, recuperación SQLite, migración, conformance del adaptador protegido, compilación del daemon, coherencia de OpenAPI y arquitectura. La puerta completa `tools/check.ps1 -Prompt P093 -Real` terminó **PASS (14/14 checks)**: fmt, Clippy, fixtures, workspace, arquitectura, copia contractual, Tauri y frontend; sus logs están enlazados en [check_P093_real.json](check_P093_real.json).

Pendientes para cerrar la fase:

- Conectar worker y preflight peak-disk por volumen con input inmutable, herramienta y parámetros fijados, guards/reservas y provenance.
- Implementar verificación de hashes y smoke/load antes de registrar outputs; conservar el original por defecto.
- Implementar reinicio desde FAILED_RECOVERABLE con input íntegro, nuevo preflight, ownership de temporales, attempt incrementado y trazabilidad anterior.
- Ejecutar en Windows los oráculos reales de disco insuficiente, crash, hash mismatch y recovery/restart a través de jobs del daemon.
- Obtener `REAL_PASS` de P016, P040 y P051, requerido por el manifiesto y el contrato 00.

Las pruebas, sus salidas y hashes de entrada constan en [la evidencia contractual](P093_transformation-pipeline-framework.evidence.json). La matriz de dependencias impide completar P093 hasta cumplir esas puertas.
