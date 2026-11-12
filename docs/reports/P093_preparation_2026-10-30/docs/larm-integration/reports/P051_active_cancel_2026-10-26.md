# P051/P125 — cancelación activa y cierre nativo

Estado de fase: `BLOCKED_REAL`; cancelación activa con fuente controlada: **PASS**. Ampliación del [incremento de controles](P051_controls_2026-10-26.md). No cualifica un proveedor externo ni cierra las aceptaciones completas de P051/P125.

## Cierre antes del estado terminal

El host cierra el handle del proceso principal después de confirmar su salida y mantiene abierto su Job Object hasta comprobar que no quedan miembros. Para terminar descendientes sólo usa IDs confirmados en ese job después de adquirir sus handles; conserva esos handles durante la operación para impedir reutilizar el PID. La consulta se repite para incluir descendientes que aparezcan durante el cierre. Un error de consulta conserva el job y su admisión; el observador no recibe una confirmación de cierre incompleta. El proceso auxiliar de terminación tiene espera limitada y no abre una consola.

La señal de drenaje sólo se publica cuando termina normalmente la tarea. Un panic o un error al esperar el proceso no acredita cierre. El consumidor ya exigía cerrar la sesión antes de retirar parciales, liberar reservas y confirmar `CANCELLED`; ahora esa espera incluye los descendientes del adaptador.

La retención de un handle evita confundir procesos por reutilización del PID; un objeto de proceso también puede seguir existiendo después de terminar. Esta distinción se documenta en [Microsoft: duración de un identificador de proceso](https://devblogs.microsoft.com/oldnewthing/20110107-00/?p=11803/). El código usa las APIs seguras existentes de `win32job` y `sysinfo`, con versiones fijadas en Cargo; no añade código `unsafe`.

## Corrección del observador de la prueba

El [primer intento](P051_runs/control-attempts/20261026_native_active_cancel_pid_present.json) falló a los 341,48 s porque seguía observando algún descendiente del servicio al llegar a `CANCELLED`. Ese intento no distingue ejecución de un objeto de proceso ya terminado ni identifica qué descendiente sobrevivía.

El [segundo intento](P051_runs/control-attempts/20261026_native_active_cancel_conhost.json) falló a los 314,48 s e identificó únicamente `conhost.exe`. El observador reunía descendientes del daemon sin demostrar pertenencia al SourceWorker. Ese resultado no acredita que siguiera vivo el lector de la descarga y no permite atribuir retrospectivamente la misma causa al primer intento.

El observador final identifica primero el ejecutable exacto del paquete cualificado entre los descendientes del daemon aislado y después incorpora el árbol de ese worker. Exige encontrarlo mientras la transferencia tiene bytes durables. Al confirmar la cancelación comprueba que ya no aparecen esos procesos con el mismo PID, instante de creación y ejecutable. No se añadió una espera de gracia después de `CANCELLED` para hacer pasar la aserción.

No se excluye un proceso por llamarse `conhost.exe`: el ensayo final incluye el que pertenece al árbol del adaptador y comprueba también su salida. Se excluyen los descendientes del servicio cuya pertenencia al adaptador no se ha demostrado.

## Verificación focal

La implementación final pasa 57/57 pruebas del host: 12 unitarias, 5 de política/permisos, 16 de runtimes, 7 de fuentes y 17 de ciclo de procesos. Los casos de descendientes comprueban su salida antes de que regrese `stop_and_wait`, incluso cuando no heredan stdin/stdout/stderr. También cubren canales heredados, caída, cancelación cooperativa, cancelación no atendida y arranque interrumpido.

Clippy del host y daemon con todos sus targets y `-D warnings`: salida 0. Formato Rust y sintaxis de los runners: PASS. Los avisos del compilador se limitan a copiar archivos de caché incremental cuando Windows no permite hard links.

## Integración Windows

La prueba `source_suite_nativa_completa_descarga_recupera_y_confirma_core_sqlite` pasa 1/1 en **393,21 s**, con la implementación final y dos procesos reales del servicio. Las dos comprobaciones de conformidad registran 21/21 observaciones aprobadas cada una. La admisión sigue sin ejecutar RPC ni tomar la exclusión de una sesión activa.

| Observación | Resultado |
| --- | --- |
| Transferencia al solicitar cancelación | `RUNNING`; checkpoint y parcial de 65.536 bytes |
| Procesos propios observados | ejecutable Python del paquete y su `conhost.exe` |
| Cancelación activa | `CANCELLED` en 10.498 ms; ambos procesos ya ausentes al observar el estado terminal |
| Limpieza | parcial retirado, journal `CANCELLED`, cero reservas y eventos sin outbox |
| Fuente y catálogo | sin nuevas lecturas tras cancelar; ningún `artifact_location` publicado |
| Otra descarga pausada | bytes intactos; `PAUSED`; una reserva conservada |
| Replay de cancelación | 202 y el mismo recibo |
| Pausa/reanudación de la otra descarga | checkpoint 131.072 bytes conservado y avance posterior |
| Admisión del job adicional en cola | 202 en 7.381 ms; límite HTTP de 30 s |
| Cancelación en cola con único slot ocupado | `CANCELLED` en 511 ms; sin afectar al parcial activo |
| Recuperación tras terminar el daemon | `Range: bytes=196608-` y el mismo `If-Range` |
| Resultado final | `COMPLETED`, intento 3; 2.097.152 bytes y SHA-256 correcto |
| Cierre final | CAS/procedencia, journal/outbox y recuperación correctos; cero reservas; principal temporal revocado |

SHA-256 final: `5647f05ec18958947d32874eeb788fa396a05d0bab7c1b71f112ceb7e9b31eee`.

[Recibo](P051_runs/daemon-restart/01a178b2-239b-7557-b71e-153390a49067/result.json) · [peticiones tras reiniciar](P051_runs/daemon-restart/01a178b2-239b-7557-b71e-153390a49067/resumed-requests.json) · [checks y hashes](P051_runs/daemon-restart/01a178b2-239b-7557-b71e-153390a49067/increment.checks.json).

## Alcance pendiente

No se repitieron la suite completa del workspace, el frontend, los runners formales completos ni la puerta global. Los resultados de interfaz del incremento anterior conservan su fecha y alcance. Siguen pendientes proveedor real cualificado, acceso privado/licencias, fallos de disco lleno y ETag/hash alterados en el recorrido completo del daemon, medición H08, GUI→daemon conjunto, transformaciones P093 y los `REAL_PASS` de las dependencias.
