# Host de adaptadores (P021)

`crates/larm-adapter-host` lanza cada adaptador como un proceso aparte y habla con él según el [protocolo v1](protocol-v1.md). Los límites salen de `defaults.json` (`adapters`) y del contrato 03.

## Estado del worker

| Estado | Cuándo |
|---|---|
| `STARTING` | Proceso lanzado, esperando el handshake |
| `READY` | Handshake correcto: acepta llamadas |
| `DEGRADED` | Frame inválido, ID ajeno/duplicado, progreso sin llamada pendiente o reply fuera de schema. Sigue aceptando llamadas |
| `FAILED` | Handshake fallido o vencido, 3 infracciones de stdout, frame mayor de 1 MiB, error de lectura, 3 heartbeats sin respuesta, proceso que sale sin que se le pida, o cancelación desatendida tras la gracia |
| `STOPPED` | Parado a petición del host |

Es el estado del **proceso**, no el de la instalación del adaptador (`adapter_installation.state`), que este código nunca cambia. `reason()` da el motivo: `handshake_timeout`, `stdout_corrupt`, `heartbeat_missed`, `process_exited`, `cancel_grace_expired`…

## Lanzamiento

- **Ejecutable**: el `executable.relative_path` del manifiesto, resuelto (canónico) **dentro** del paquete. No puede salir del paquete ni por enlaces.
- **Argumentos**: los del manifiesto, como lista estructurada. Nunca una cadena de shell.
- **Entorno**:
  - se vacía (`env_clear`);
  - solo lleva `SystemRoot`, `SystemDrive`, `windir`, `NUMBER_OF_PROCESSORS`, `PROCESSOR_ARCHITECTURE` y `TEMP`/`TMP` en su carpeta de trabajo, más las variables del perfil;
  - los nombres de variable se validan (`[A-Za-z_][A-Za-z0-9_]*`).
- **Ventana**: sin consola (`CREATE_NO_WINDOW`).
- **Contención**: en Windows el worker se asigna a un Job Object con `KILL_ON_JOB_CLOSE`. El Job se conserva hasta que termina el handle del proceso raíz; al salir este, se drenan durante un máximo de un segundo los canales heredados y se cierra el Job para terminar descendientes que sobrevivan. `TreeKill` sigue como fallback y `kill_on_drop` protege el handle del hijo. La asignación ocurre justo después del spawn, así que procesos que el worker lance antes de ella no quedan cubiertos.

`stop_and_wait()` cierra stdin y espera proceso, stdout y stderr. Una escritura
bloqueada no puede impedir llegar al último recurso. Tras la gracia se termina
el árbol si sigue vivo; un timeout de drenaje es un error, nunca acredita cierre.
También se esperan canales heredados por un hijo aunque el proceso raíz ya haya
terminado. Los consumidores que vayan a confirmar metadata deben propagar ese
resultado; `stop()` conserva la interfaz anterior y registra un fallo de cleanup.
El scope del motor de conformidad permanece retenido por los lectores/wait hasta
terminar: [ejecución nativa y admisión](conformance.md#ejecutor-nativo-y-ownership).

## Llamadas

- **Método**: tiene que estar en el manifiesto; si no, `UNSUPPORTED_CAPABILITY` sin enviar nada.
- **Cola y concurrencia**: cola de 64 (llena: `RESOURCE_UNAVAILABLE` `queue_full`, reintentable) y hasta 4 llamadas en paralelo. Las mutaciones con el mismo `resource_id` van en serie.
- **Timeout por operación** (por defecto 30 s):
  1. al vencer, se envía `core.cancel`;
  2. se espera la gracia (5 s);
  3. si sigue sin responder, como **último recurso** se mata el árbol de procesos y el worker queda `FAILED`;

   en todos los casos, la llamada devuelve `TIMEOUT` `rpc_timeout`.
- **Cancelación por el host**: `cancel(request_id)` con el id de `call_request`.
- **Heartbeat**: cada 10 s. Tres ausencias seguidas: `FAILED` y se mata.
- **Proceso que termina**: las llamadas en curso reciben `ADAPTER_CRASHED`.

El lector de stdout limita el frame durante la lectura, antes de deserializar.
Superar 1 MiB provoca `stdout_frame_too_large` sin esperar newline ni EOF. Un error
del lector provoca `stdout_read_failed`. Los demás errores usan el mismo contador:
una infracción deja DEGRADED y tres terminan el árbol con `stdout_corrupt`.

Cada llamada pendiente conserva su método. El host valida el schema de su reply
con los contratos SDK antes de entregarlo a Core; un valor inválido retorna
PROTOCOL_ERROR/invalid_adapter_reply sin incluir el payload privado. Las respuestas
con ID desconocido o ya completado no se aceptan, y sólo se publica progreso de
llamadas pendientes. Tampoco se admiten peticiones del adaptador al host. El
handshake conserva su validación específica de nonce, versión y manifiesto.

## stderr

Se drena siempre, porque un stderr lleno bloquearía al adaptador:

- cada línea se redacta (`larm_domain::redact`) y se publica como log etiquetado con el adaptador y la generación;
- lo que pasa de 10 MiB se descarta y se cuenta (`stderr_dropped`);
- una línea mayor de 16 KiB se drena entera sin retenerla ni publicar fragmentos;
- se conservan las 50 primeras y las 150 últimas líneas para diagnosticar.

## Matar no es aislar

`TreeKill` termina el proceso y sus descendientes con `taskkill /T /F`, sin código inseguro. El Job Object con kill-on-close cubre procesos descendientes asignados, pero **no** aísla disco ni red. Los límites de recursos y AppContainer son de P130, detrás del puerto `Containment`.

## Adaptador falso

`crates/larm-adapter-host/examples/fake_adapter.rs` (fixture en `tests/fixtures/fake_adapter/`) tiene modos `silent`, `bad-nonce`, `crash`, `garbage`, `stderr-flood`, `deaf`, `deaf-tree`, `deaf-input`, `deaf-input-once`, `late-extra`, `tail-after-exit`, `no-heartbeat` y `env`. Fuerzan backpressure, EOF ignorado, recuperación en un nuevo arranque, un reply tardío y un stderr heredado que sigue abierto tras salir la raíz.

`tools/p026_wire_real.py` amplía la matriz a los cinco workers funcionales Windows.
Primero verifica mensajes reales contra cada SDK; después copia un proxy de test
al ejecutable declarado y preserva el worker original como hijo real. Los mutantes
tienen otro hash, instrumentación propia y no son candidatos cualificados.
`conformance_host` usa el host de transporte real y devuelve observaciones para
oráculos Python independientes, con comprobación de procesos supervivientes por
PID e imagen. Los tests por `GuardedWorker` siguen comprobando P022 por separado.

`conformance::wire` usa además el transporte nativo sobre los cinco paquetes
originales, con P022, antes del primer handshake compatible. Comparte la
construcción del proceso con Worker y espera stdin/stdout/stderr/proceso. Los 80
casos originales son nueve checks parciales por familia, sin reutilizar el PASS
de un mutante ni completar los oráculos de negocio que siguen pendientes.

`conformance::runtime::ReferenceRuntimeSuite` completa los 17 checks sobre la
referencia runtime/echo bajo P022: versión/binario, progreso/cancelación con hijo
real, permisos, planes, salud, HTTP eco, parada y crash/reinicio. El test del daemon
invoca Core y SQLite reales, confirma el recibo, pasa el gate de promoción y retira
elegibilidad cuando falla una revalidación. El endpoint público ejecuta este
recorrido para las referencias registradas en SQLite; faltan los recursos
automáticos de perfiles generales y la composición de consumidores de ejecución.

La receta `conformance::source` completa 21 checks sobre el catálogo y los bytes
de fixture: ledger privado real, Range/If-Range, progreso/cancel, fallos de outbox,
crash y recuperación, además de Core/SQLite. El IO auxiliar registrado con
`NativeIo` conserva admisión hasta cerrar incluso tras timeout/panic; el reinicio
del worker no cierra prematuramente ese recurso del oráculo. No acredita P050.

La receta `conformance::protocol` completa 17 checks y cuatro firmas contra
HTTP/echo Rust reales: IDs semánticos/RPC, cancel/timeout, errores de framing y
schema HTTP, fingerprints, crash/reinicio de worker y backend. Core/SQLite confirma
el recibo y el gate de promoción deshabilitada; no cualifica inferencia ni leases.

`conformance::transformation` completa 17 checks y cuatro firmas con archivos
Base64 reales y oráculo Rust independiente, provenance/manifiestos, ledger de
intentos, cancel/EOF, crash y fallos de outbox con recuperación. Conserva recursos
inciertos y la causa original; otro plan reinicia desde cero. Core/SQLite confirma
el recibo. No acredita conversión de modelos ni reservas globales del producto.

`conformance::benchmark` completa 17 checks y tres firmas con seis muestras HTTP
por medición, estadísticas Rust independientes, hashes wire/artefacto, ledger
SQLite privado y recuperación tras cancel/EOF/crash, colisión y fallos de commit.
Los desconocidos permanecen null/UNAVAILABLE; Core/SQLite confirma el recibo sin
seleccionar/habilitar. Faltan recursos automáticos de perfiles generales y consumidores de ejecución.

El mismo runner implementa `AdapterQualification`: sólo relectura física/P022,
sin RPC. Selección/rollback/enable y fuentes habilitadas exigen su guard dentro
de la transacción, con recheck final y handles Windows retenidos. Un guard vivo
conserva admisión; cancelar su observador no abandona la medición ni permite un
nuevo propietario. La selección local y el catálogo HTTP de fuentes reciben
este runner con perfiles persistentes; un perfil ausente o cambiado deniega activaciones.

`conformance::service::ProbeService` admite Probe como job durable, supervisa su
trabajo y espera `wait_conformance` tras errores de IO. `NativeCatalog` sólo acepta
NativeTarget compilados del composition root, no JSON del cliente. Las cinco
referencias usan el endpoint autenticado y confirman job/recibo juntos. El daemon
real tiene la ruta y rechaza explícitamente paquetes cuyo perfil aún no se ha
registrado. `PersistentTargets` reconstruye las cinco recetas compiladas desde
journals con hash, actor/manifiesto y aprobación consumida. El registro local
confirma CAS/consumo/evento/outbox juntos y no concede VALIDATED. Los oráculos
siguen necesitando sus recursos auxiliares preparados; automatizarlos y conectar
consumidores de ejecución son trabajos pendientes.
