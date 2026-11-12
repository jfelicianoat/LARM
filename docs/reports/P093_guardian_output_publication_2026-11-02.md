# P093 — salida observada del supervisor hasta publicación

Fecha: 2026-11-02. Ejecución final congelada: 08:00:26–08:15:40 UTC. Contrato público 3.13.0 autorizado e intacto. Objetivo global de completar LARM activo. Componente interno verificado; P093 conserva preparación/`BLOCKED_REAL`, sin `CODE_PASS` integral ni `REAL_PASS`.

## Cambio y comportamiento

El cliente del supervisor devolvía un manifiesto raw que no podía entregarse al verificador físico, cuyo contrato exige una respuesta opaca observada por el host. `GuardianTransformationClient::verify_claim_output` conecta ahora ambos componentes sin permitir que el consumidor convierta JSON arbitrario en esa prueba.

Antes del RPC valida el descriptor versión3 y su propietario registrado, fence, binding sellado, trabajo, intento, instalación y contexto Core. Exige respuestas nuevas firmadas de Recheck antes y después de Verify. El helper vuelve a comprobar por su cuenta el registro y cualificación actuales y solo permite verificar el job privado devuelto por su único Execute. Los metadatos guardados o un preview sin propietario registrado no conceden esa prueba.

`TransformationOutputManifest` conserva sus campos privados y no es deserializable. Su constructor compartido es privado del crate y valida fence, binding, job privado y manifiesto; lo utilizan los dos canales tipados cualificados. El resultado acredita una respuesta observada. El cierre nativo, integridad de archivos, espacio y publicación conservan sus verificaciones independientes.

No se añaden dependencias, migraciones, `unsafe`, campos de API ni cambios contractuales. [ADR-0015](../adr/0015-supervisor-nativo-recuperable.md).

## Recorrido real añadido

El escenario conserva las tres interrupciones anteriores: ejecución con RPC pendiente, preparación previa a RUNNING y cierre confirmado sin Release. Después admite un plan nuevo y ejecuta un cuarto intento del mismo trabajo, con el usuario de Core separado del usuario del backend.

1. Rechaza la obtención de salida observada antes del registro y con etiquetas ajenas: nonce, generación, trabajo, instalación, intento, propietario, ejecución, usuario, correlación o binding alterados. Un cliente de preview tampoco puede producirla.
2. Ejecuta la conversión de referencia con el helper real y registra VERIFYING. Rechaza Verify de otro job privado y obtiene el manifiesto observado del job correcto.
3. Rechaza la captura física mientras el proceso sigue vivo; conserva una reserva y cuatro locks. Solicita Stop y comprueba el cierre nativo real de árbol/IO/calls.
4. Altera un único byte de la salida. El verificador físico rechaza `transformation_output_hash_changed` aunque el manifiesto observado sea válido. No cambia eventos, reservas ni locks y todavía no existe CAS. Restaura el byte de la fixture y vuelve a medir archivos, tamaños, EOF y SHA.
5. Ejecuta los validadores registrados. La ausencia de un hook de runtime se conserva como `NotRegistered`; no equivale a una carga real de modelo.
6. Obtiene el ticket SQL de publicación, escribe CAS/manifiesto y confirma la variante derivada, procedencia, COMPLETED y liberación exacta en la misma transacción. Hasta ese commit conserva los recursos. Repetir la finalización no cambia el trabajo ni añade eventos.
7. Confirma el commit, limpia los recibos propios y solo después retira el supervisor. Conserva abierto su stdin hasta la retirada y verifica su salida limpia. El original queda intacto y los dos usuarios temporales se revocan.

El trabajo termina en **COMPLETED, intento4, revisión19 y diecinueve eventos/outbox**. Conserva las tres entradas anteriores de historial, propietarios, errores y planes. Quedan un trabajo, dos variantes —original y derivada—, cero reservas y cero locks. El original tiene 12.582.917 bytes; los dos archivos publicados suman 16.778.016 bytes.

Catálogo inicial, mapeo del preview a volúmenes, admisión y reinicio siguen siendo fixtures explícitas del controlador. Son reales la cualificación mediante daemon HTTP, el helper del mismo binario, archivos/volúmenes, transacciones, procesos, recuperación automática y publicación. La conversión es la referencia Base64, no un conversor GGUF/ONNX ni una aceptación de runtime. El escenario no activa el productor/consumidor público ni prueba caída durante bytes parciales y reinicio confirmado hasta publicación por API.

## Pruebas y auditoría

**488 pruebas Rust PASS, 0 FAIL y 0 ignoradas; 27/27 comprobaciones PASS en el intento3.** Wrapper y auditor terminan con código0. Se contrastan 880 entradas y 27 logs/recibos sin cambios de fuente durante la ejecución.

El total Rust permanece en 488 porque se amplía el escenario nativo existente. Sus catorce negativos nuevos se contrastan como subcasos y no se suman otra vez al total. También se comprueba el rechazo previo al registro.

| Grupo | Pruebas correctas |
|---|---:|
| Dominio | 181 |
| DB unitaria y planes/admisión/reinicio | 135 |
| HTTP y OpenAPI | 16 |
| Protocolo, supervisor real/cualificado y Worker | 40 |
| Exclusividad nativa, ciclo de vida y unitarias del daemon | 64 |
| Cliente, catálogo, archivos físicos, espacio y CAS | 52 |
| Total | 488 |

Clippy de cinco crates/todos los targets con warnings denegados, formato, política de `unsafe`, arquitectura17 y copias79: PASS. La regresión previa P026 conserva 176 casos funcionales, 16 wire, 17 observaciones y nueve gates físicos. El caso anterior de publicación directa, independiente del nuevo caso con supervisor, conserva sus tres intentos/trece eventos, diecisiete negativos y un derivado.

La auditoría reconstruye independientemente Base64, SHA, procedencia, sellos RFC8785, planes, grafo de publicación, manifiesto raíz y bytes CAS. Contrasta también el historial, las identidades de ambos usuarios, cuatro intentos/diecinueve eventos, los catorce negativos y el byte corrupto retenido. Variante VERIFIED/UNKNOWN, capacidades vacías y límites desconocidos permanecen explícitos. La inspección final válida encuentra cero helpers, propietarios de fixtures y fake-adapters del workspace; la metadata de procesos no se utiliza como autoridad de cierre.

## Fallos conservados y corrección del comprobador

Los intentos1 y2 abortaron el proceso de pruebas con `STATUS_STACK_OVERFLOW` (`0xc00000fd`). Los targets ya terminados aportaban 353 pruebas correctas, pero el escenario abortado no emitió resumen Rust: ese contador no permite presentar ninguna de esas corridas como correcta.

En el intento1 el cuarto intento había quedado QUEUED/revisión15, antes del claim. Se conservaron sus fuentes y recibo; el helper se retiró mediante reto firmado Stop/Closed/Release, sin liberar recursos de Core. Se revocaron sus dos credenciales temporales y se cerró exclusivamente su servidor API, con identidad de base/trabajo y handle de proceso comprobados.

El primer aislamiento con `Box::pin` no bastó: el intento2 abortó al preparar el segundo intento, QUEUED/revisión6. Se conservaron otras dos fuentes exactas y sus recibos; se revocaron sus dos credenciales temporales y se cerró su servidor identificado. Los trabajos fallidos permanecen como evidencia y ninguna ausencia de proceso autoriza liberar sus reservas.

La corrección final elimina el futuro del nuevo escenario de la composición del controlador principal: recibe datos propios, comparte Setup mediante Arc y se ejecuta en una tarea independiente del runtime. Las operaciones grandes de claim/publicación se fijan en memoria dinámica. No se aumenta el tamaño de pila mediante configuración. El diagnóstico conservado observa un futuro de 41.456 bytes. El intento3 completa todo el recorrido y la regresión.

Se añade un ejemplo auxiliar de limpieza de fixtures abortadas que usa el observador nativo real. No cambia trabajos ni recursos de Core y nunca convierte una prueba fallida en PASS. La auditoría de preservación contrasta fuentes, logs y recibos de ambas corridas fallidas por separado.

## Evidencias

- [Resumen final3](P093_runs/observed-output-20261102/attempt3/verification-synopsis.json), [auditoría completa](P093_runs/observed-output-20261102/attempt3/post-run-audit.json) y [limpieza final](P093_runs/observed-output-20261102/attempt3/post-run-process-check.json).
- [Caso nativo cualificado](P093_runs/observed-output-20261102/attempt3/native-qualified-guardian.json), [log](P093_runs/observed-output-20261102/attempt3/verify-qualified-guardian.log) e [informe de publicación observada](P093_runs/qualified-guardian/01a19b46-ca21-76a5-9170-6a59c7f3de75/registered-owner/observed-output-publication/report.json).
- [Grafo publicado](P093_runs/qualified-guardian/01a19b46-ca21-76a5-9170-6a59c7f3de75/registered-owner/observed-output-publication/publication-record.json), [manifiesto raíz](P093_runs/qualified-guardian/01a19b46-ca21-76a5-9170-6a59c7f3de75/registered-owner/observed-output-publication/published-manifest.json) y [diagnóstico de futuro](P093_runs/qualified-guardian/01a19b46-ca21-76a5-9170-6a59c7f3de75/registered-owner/checkpoint-publication-future.json).
- [Regresión Core/CAS anterior](P093_runs/observed-output-20261102/attempt3/native-transformation.json), [wrapper congelado](P093_runs/observed-output-20261102/attempt3/frozen-verify.py) y [auditor congelado](P093_runs/observed-output-20261102/attempt3/frozen-audit.py).
- [Intento1 fallido](P093_runs/observed-output-20261102/attempt1/verification-synopsis.json), [intento2 fallido](P093_runs/observed-output-20261102/attempt2/verification-synopsis.json) y [auditoría de preservación/limpieza](P093_runs/observed-output-20261102/prior-failures-audit.json).

## Continuación necesaria

La prioridad siguiente es el productor/consumidor público del daemon: selección y cualificación actuales, preview y mapeo global de recursos reales, confirmación del plan, lanzamiento registrado, uso del nuevo manifiesto observado, cierre, verificación y publicación. La composición pública conserva `worker_unavailable`; ya no falta el enlace tipado de salida del supervisor probado aquí.

Siguen progreso/cancelación por id y H07 completo mediante API: caída durante conversión, plan nuevo confirmado y reinicio del mismo trabajo hasta publicación. También reservas por fase/volumen, retención privada, reconstrucción de procedencia, política ante pérdida definitiva del supervisor/reinicio del sistema, FAT32 completo, conversores/hooks GGUF/ONNX reales e interfaz. Runtimes, fuentes, Broker, Agora y las restantes fases de LARM continúan en el objetivo global activo.
