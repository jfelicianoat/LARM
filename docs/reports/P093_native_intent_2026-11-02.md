# P093 — intención durable y recuperación automática del supervisor

Fecha: 2026-11-02. Ejecución final congelada: 07:00:56–07:16:38 UTC. Contrato público 3.13.0 autorizado e intacto. El objetivo global de completar LARM continúa activo. Este incremento verifica componentes internos; P093 conserva preparación/`BLOCKED_REAL`, sin `CODE_PASS` integral ni `REAL_PASS`.

## Cambio y comportamiento

Antes de lanzar un helper, Core guarda una intención `transformation.owner` ligada al trabajo, revisión, intento, plan, cualificación y contexto de su usuario. La preparación compara la propuesta y confirma en una sola transacción la nueva revisión QUEUED, la intención PREPARING y el evento/outbox. El helper comprueba ese registro exacto antes de preparar el input. Ni la propuesta ni la preparación conceden ejecución.

El claim confirma RUNNING y REGISTERED juntos, con disponibilidad nativa comprobada antes y dentro de la transacción. El cierre del intento confirma CLOSED y libera exactamente sus recursos en la misma transacción. Los cambios obligatorios omitidos, las revisiones antiguas y las intenciones incoherentes hacen retroceder toda la operación. El payload de la intención es inmutable; contiene metadatos públicos y nunca la clave privada de lanzamiento.

El daemon real reconcilia al arrancar las preparaciones y ejecuciones interrumpidas. Reconstruye el observador desde los metadatos validados, exige respuestas nuevas firmadas y solicita Stop. Solo después de comprobar el cierre del árbol, IO y llamadas puede cerrar el intento y liberar reservas/locks. Si la interrupción ocurrió antes de RUNNING, adopta esa preparación ya cerrada para registrar el fallo; esa vía no permite ejecutar. Un helper ausente, un timeout, un PID o un estado persistido no acreditan cierre y no liberan recursos.

La retirada del testigo ocurre después del commit de Core. Un segundo recorrido de arranque puede retirar un testigo todavía vivo cuyo intento ya quedó cerrado, incluso si está archivado y existe otro intento. Ese recorrido es mantenimiento: no cambia el trabajo, sus eventos ni recursos. La ausencia de un testigo en ese recorrido tampoco se usa como prueba de cierre.

El supervisor de recuperación conserva duplicados de las mismas anclas nativas de exclusividad del daemon y la base mientras sus tareas terminan. Su cierre es cooperativo; el cierre ordinario del servidor espera al supervisor. El lanzador conserva Child/stdin y espera la salida solo después de Release; esos handles y el código de salida no sustituyen la prueba nativa.

Se separan el usuario que cualificó el backend y el propietario del trabajo Core. La cualificación usa el primero, obtenido del perfil persistido; la autorización del trabajo conserva el segundo. No se añade autoridad desde parámetros del adaptador.

No se añaden dependencias, migraciones, `unsafe` ni campos al contrato público. [ADR-0015](../adr/0015-supervisor-nativo-recuperable.md).

## Fallo de consistencia reproducido y corregido

La prueba con un trigger AFTER que eliminaba el outbox reprodujo una preparación confirmada sin su notificación obligatoria. Comprobar solo `rows_affected()` no detectaba esa pérdida. Se conserva el fallo real con código101 y las fuentes exactas previas.

`events::append` vuelve a leer el par evento/outbox dentro de la transacción y contrasta enlace, identidad, actor, recurso, revisión, correlación, tiempos, metadatos y payload. La operación completa se revierte ante ausencia o alteración. Pasan los cuatro negativos de borrado de outbox y alteración de payload/actor/revisión. Por afectar al escritor compartido, se ejecutaron las 77 pruebas unitarias de DB, además de las 58 de planes/reinicio.

## Tres interrupciones con procesos reales

El caso usa el daemon HTTP real para cualificar P026 y el mismo ejecutable para el helper. Catálogo inicial, mapeo de preview a volúmenes, planes globales y admisión/reinicio del trabajo son fixtures explícitas del controlador. Archivos, volúmenes, cualificación, transacciones, procesos, reinicios del servidor y reconciliación automática son reales. El controlador conserva abierto el stdin del helper hasta la retirada explícita.

| Interrupción | Resultado comprobado |
|---|---|
| RUNNING, con RPC real pendiente | El daemon reiniciado conserva `WORKER_INTERRUPTED`, reconecta, acredita cierre y libera recursos. Mismo trabajo, revisión5 y cinco eventos; otro arranque no añade eventos. |
| PREPARING, antes del registro RUNNING | Execute sigue rechazado. El daemon cierra el helper y registra el fallo con el mismo trabajo/intento2. Revisión9, nueve eventos e historial anterior conservado. |
| CLOSED confirmado, sin enviar Release | Se reproduce expresamente la ventana entre commit y retirada. El daemon retira el testigo vivo del intento3. El trabajo permanece exactamente igual en revisión13, con trece eventos. |

Los nuevos planes de reinicio son distintos y sus sellos se reconstruyen independientemente. Las observaciones consistentes de SQLite conservan una reserva y cuatro locks mientras el intento está abierto, y muestran cero de ambos tras su cierre. Al final quedan un trabajo y una variante original, sin derivado en este caso. El original de 12.582.917 bytes permanece intacto. Los dos usuarios temporales se revocan; el usuario del backend no puede consultar el trabajo ajeno por HTTP.

La primera interrupción sucede con un RPC del ledger privado retenido; no demuestra una caída durante bytes parciales de conversión. Los planes/admisión siguen originándose en el controlador de prueba. Esto no acredita el recorrido público completo ni H07 integral hasta publicación.

## Verificación final

**488 pruebas PASS, 0 FAIL, 0 ignoradas; 27/27 comprobaciones PASS en el intento2.** Wrapper y auditor terminan con código0. Se contrastan 878 entradas y 27 logs/recibos, sin cambios de fuente durante la ejecución.

| Grupo | Pruebas correctas |
|---|---:|
| Dominio | 181 |
| DB unitaria y planes/admisión/reinicio | 135 |
| HTTP y OpenAPI | 16 |
| Protocolo, supervisor real/cualificado y Worker | 40 |
| Exclusividad nativa, ciclo de vida y unitarias del daemon | 64 |
| Cliente, catálogo, archivos físicos, espacio y CAS | 52 |
| Total | 488 |

Clippy de cinco crates/todos los targets con warnings denegados, formato, política de `unsafe`, arquitectura17 y copias79: PASS. Las pruebas SQL usan dobles explícitos para los fallos transaccionales; la autoridad de cierre se verifica por separado con procesos nativos. La nueva prueba física demuestra que conservar el ancla duplicada impide otra adquisición hasta que termina su propietario retenido.

La regresión P026 conserva 176 casos funcionales, 16 wire, 17 observaciones y nueve gates físicos. El caso Core/CAS anterior, distinto de las tres interrupciones anteriores, conserva tres intentos/trece eventos, diecisiete negativos de publicación, un derivado y cero reservas/locks finales. Se reconstruyen independientemente Base64, SHA256, procedencia, sellos RFC8785 y 16.778.016 bytes de salida sana; una corrupción controlada se rechaza. Los casos internos no se vuelven a sumar al total Rust.

El intento1 de esta fase también pasó: 485 pruebas, 27 comprobaciones y auditoría. Se conserva como baseline anterior al refuerzo de vida de las anclas, retirada de testigos archivados y tercer escenario nativo; no se presenta como verificación de las fuentes finales.

La inspección final válida encuentra cero helpers, propietarios de prueba y fake-adapters de este workspace. La primera consulta CIM careció de acceso y PowerShell continuó tras un error no terminal, generando un recibo PASS inválido. Ese recibo queda conservado y señalado como inválido. La repetición con acceso suficiente y errores terminales es la evidencia de limpieza. Ninguna inspección de procesos concede autoridad de cierre.

## Evidencias

- [Resumen final2](P093_runs/native-intent-20261102/attempt2/verification-synopsis.json), [auditoría](P093_runs/native-intent-20261102/attempt2/post-run-audit.json) y [limpieza válida](P093_runs/native-intent-20261102/attempt2/post-run-process-check.json).
- [Caso cualificado](P093_runs/native-intent-20261102/attempt2/native-qualified-guardian.json), [log](P093_runs/native-intent-20261102/attempt2/verify-qualified-guardian.log) e [informe de las tres interrupciones](P093_runs/qualified-guardian/01a19b12-b65b-7747-990b-1fa04565c113/registered-owner/report.json).
- [Preparación temprana](P093_runs/qualified-guardian/01a19b12-b65b-7747-990b-1fa04565c113/registered-owner/early-preparation/report.json) y [retirada posterior al commit](P093_runs/qualified-guardian/01a19b12-b65b-7747-990b-1fa04565c113/registered-owner/closed-witness-retirement/report.json).
- [Fallo de outbox antes de corregir](P093_runs/native-intent-20261102/development-outbox-before.json), [después](P093_runs/native-intent-20261102/development-outbox-after.json) y [fuentes anteriores](P093_runs/native-intent-20261102/development-source-before-outbox-fix/crates/larm-db/src/events.rs).
- [Baseline485](P093_runs/native-intent-20261102/attempt1/verification-synopsis.json) y [auditoría del baseline](P093_runs/native-intent-20261102/attempt1/post-run-audit.json).
- [Regresión Core/CAS](P093_runs/native-intent-20261102/attempt2/native-transformation.json), [wrapper congelado](P093_runs/native-intent-20261102/attempt2/frozen-verify.py) y [auditor congelado](P093_runs/native-intent-20261102/attempt2/frozen-audit.py).

## Continuación necesaria

La prioridad siguiente es conectar el productor/consumidor público del daemon y llevar la salida observada y firmada por el supervisor al verificador físico y a la publicación. La respuesta raw de Verify todavía no sustituye el manifiesto opaco observado por el host. Deben unirse sin rebajar ese requisito, con selección/cualificación actuales y un plan global que autorice los recursos reales.

Faltan progreso/cancelación por id, caída durante conversión, plan nuevo confirmado y reinicio del mismo trabajo hasta publicación mediante API. El motor público conserva `worker_unavailable`. Siguen las reservas por fase/volumen, retención privada, reconstrucción de procedencia, política ante pérdida del supervisor/reinicio del sistema, FAT32 completo, conversores/hooks GGUF/ONNX reales e interfaz. El resto de LARM, incluidos runtimes, fuentes, Broker y Agora, permanece en el objetivo global activo.
