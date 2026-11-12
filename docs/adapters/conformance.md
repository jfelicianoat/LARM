# Confirmar conformidad de una instalación (P026 en curso)

El caso de uso `larm_core::conformance::AdapterConformance::validate` recibe sólo
el ID registrado y el contexto del actor. No recibe un JSON PASS, un recibo externo
ni una lista de checks del cliente. `AdapterConformanceRunner` es un puerto interno
que debe ejecutar los oráculos reales bajo P022 y devolver cuando haya terminado
todo su IO. El motor `NativeConformanceRunner` ya implementa ese puerto.
**La ruta pública, su cola durable y el registro local de perfiles persistentes
están compuestos**. La receta runtime/echo ya recorre Core y SQLite
con el paquete real en un test de integración, igual que source con su ledger y
transferencia HTTP; protocol también recorre ese flujo con backend echo Rust y
fallos de transporte reales, transformation con archivos Base64 e intentos reales,
y benchmark con muestras HTTP/estadísticas/ledger privado y recuperación.
`POST /api/v1/adapters/{id}/probe` ejecuta este flujo cuando existe la receta
registrada por el host; no admite veredictos. El daemon resuelve los perfiles
desde SQLite; una instalación sin perfil termina con fallo explícito. Los oráculos sintéticos
de tests no se registran como suites de producto.

## Sondeo público durable

El endpoint exige `adapters:write`, `Idempotency-Key` y Probe exacto, con dos claves
obligatorias anulables: `resource_id` null o el ID de la ruta, y `profile_id` null
o `conformance.v1`. Campos extra, duplicados, nulables ausentes o tipos inválidos
se rechazan antes del servicio. Devuelve 202/JobReceipt original QUEUED; el cliente
consulta `/api/v1/jobs/{id}`. Repetir key/cuerpo/destino no reejecuta, y otra
combinación produce conflicto. El replay queda aislado por actor y dura 30 días.

`ProbeService` conserva la aceptación aunque se desconecte el observador, reclama
QUEUED en SQLite y ejecuta una sola tarea supervisada. El snapshot aceptado debe
seguir vigente. Instalación/recibo/journal de conformidad y resultado terminal del
job se confirman en la misma transacción con eventos/outbox. Un error previo sin
Execution no equivale a fallo de conformidad; un commit no confirmado bloquea el
recibo anterior de ese snapshot hasta una nueva validación. No se pierde esa
restricción por otra petición denegada. La lectura del job filtra propietario/admin.

Un error de cleanup espera `wait_conformance` antes de declarar terminal el job.
Shutdown cierra admisión y espera la tarea/IO acotados por la suite. Un RUNNING de
otro proceso permanece RUNNING con `recovery_required`: no se reejecuta ni se
inventa quiescencia por tiempo (reconciliación de procesos P130 pendiente).
Errores públicos conservan categoría conocida y eliminan mensajes privados.

Las cinco referencias pasan por este servicio, el router HTTP y el guard real con
credenciales efímeras en memoria, con replay inmutable. El daemon/TCP real prueba
por separado auth/scopes, DTO, privacidad, persistencia y shutdown/reapertura con
14 checks: un paquete sin suite registrada falla y conserva INSTALLED/UNTRUSTED,
sin ejecutar ni marcar VALIDATED. El recorrido adicional de perfil persistente
usa runtime real, CLI de registro, reinicios y selección física.

## Perfiles locales persistentes

`register-adapter-probe-profile --installation ID --actor-id ACTOR --approval-id ID
--file CONFIG [--expected-profile ID]` registra datos de una de las recetas
compiladas de `conformance::profiles::Recipe`. El fichero de administración local
fija paquete/work, raíces, inputs, perfil de capacidades, timeout y parámetros del
oráculo. No contiene código, observaciones ni Proof, y no entra por Probe. Los
constructores y sus schemas rechazan recetas/campos ajenos. El paquete debe ser el
publicado bajo la raíz de datos para esa instalación. Los recursos auxiliares del
oráculo tienen que estar preparados por el operador; su creación automática y
la configuración de perfiles generales de producto siguen pendientes.

El journal `adapter.conformance.profile` guarda el documento completo y su hash
JCS. Comprueba actor/manifiesto/paquete y exige la aprobación P022 exacta. Consume
la aprobación, sustituye el perfil anterior por CAS y añade evento/outbox en una
transacción. Un sondeo QUEUED/RUNNING impide cambiar su perfil. READY significa
configuración registrada; no concede VALIDATED ni confianza. Cualquier fallo o
escritura ignorada revierte también el consumo. La lectura comprueba hash,
recursos y el snapshot de aprobación realmente consumido.

`PersistentTargets` reconstruye sólo las suites compiladas desde ese
registro. Su hash completo entra en el descriptor de la suite; renovar el perfil
o su concesión exige nueva conformidad. La concesión conserva su expires_at
explícito, aunque el TTL corto de la aprobación de un uso haya transcurrido. La
caducidad efectiva, el actor, P022 y los bytes se verifican otra vez al ejecutar
o adquirir el guard. No se heredan credenciales de entorno ni se importan PASS.

La receta adicional `source.http.chunks.v1` comprueba un perfil HTTP import-only
con bytes y un servidor loopback controlados por el operador. Sus parámetros
cerrados son `source_id`, `storage_id`, `url`, `expected_file`, `control`,
`requests` y `evidence_root`. El archivo esperado y `work/profile.json` se añaden
a los inputs inmutables; control, traza y oráculo quedan fuera del paquete/work.
No admite código, Proof ni observaciones del cliente. Comprueba los nueve casos
wire comunes y cancelación, progreso, crash, permisos, estado, recuperación y
familia source: 16 observaciones exigidas por un manifiesto sin capacidades de
búsqueda. Incluye metadatos, plan, bloques, reanudación, hash, auth, 429 y corte de
conexión, sin consultar el ledger privado del adaptador de referencia. El
registro no equivale a haber superado el sondeo. Un perfil loopback cualificado
no cualifica otro URL, una fuente privada ni el recorrido Core de descarga/CAS.

La selección local acepta `--actor-id` de la concesión y usa el resolver
persistente con el gate físico. El catálogo HTTP de fuentes recibe el mismo
runner que el servicio de sondeos, conservando su admisión compartida. El recorrido
real de fuentes habilitadas comprueba alta/edición, actor, If-Match, input cambiado,
rollback de outbox, replay histórico sin reactivar, desactivación y reinicio. Una
operación de selección no ejecuta el oráculo; sólo remide y retiene handles hasta
COMMIT. Desactivar sigue permitido con un contexto físico ya cambiado.

## Verificación de la fase

`tools/p026_evidence.py` ejecuta 51 grupos declarados y después la auditoría
independiente. Distingue contratos, integración/fault injection de fixtures y
recorridos reales. Comparte el catálogo con `p026_prepare_evidence.py`; registra
cada comando, resultado y log/hash, y congela las fuentes antes/después. El
runner no importa un PASS ni sustituye ejecución por inspeccionar recibos viejos.
La auditoría recibe el recibo de esta ejecución, reconstruye los journals de las
cinco referencias y comprueba que las evidencias HTTP se generaron dentro de su
intervalo, incluido el hash de la traza de fuentes sin tokens.

`tools/check.ps1 -Prompt P026 -Real` añade las puertas generales de workspace,
shell Tauri y frontend. El estado de PromptEvidence lo calcula `Evidence.finish`
a partir de las pruebas y P025: el bloqueo de una dependencia no se elimina
porque los checks propios pasen. Preparar recursos generales de producto y sus
consumidores sigue las fases propietarias; las referencias sólo acreditan sus
capacidades observadas.

El escaneo de credenciales lee en bloques con solapamiento para detectar valores
entre límites, mantiene ocho lectores y una cola acotada, y propaga errores de IO.
Incluye NDJSON/CSV y SQLite/WAL además de código y logs; conserva las exclusiones
de artefactos de build. Sus seis tests usan marcadores sintéticos, sin leer el
almacén. El presupuesto de 1800 s para todo el historial no modifica las cotas
RPC/cancel/worker del producto. No registrar patrones, contenido ni tokens.

## Binding de una ejecución

`Run` liga un UUIDv7 y el actor a una instantánea de la instalación: revisión,
paquete, manifiesto, permisos, familia y capacidades. Incluye hashes del ejecutor
o suite, del perfil completo de capacidades y del contexto efectivo de ejecución.
El ejecutor debe calcularlos sobre los bytes y bindings reales; un SHA con formato
válido no acredita haber ejecutado la suite. Core valida primero el manifiesto
con el SDK, sus identidades y el estado/confianza registrados.

El hash de la instantánea incluye metadata, trust y estado. Las revisiones se
canonizan como strings decimales para conservar i64 exacto por encima de 2^53.
No se redondean y se rechaza una revisión sin espacio para el siguiente incremento.
El hash de ejecución incluye el actor: una prueba de otro actor no confirma nada.

`Proof::evaluate` exige cobertura exacta de 15 comprobaciones comunes, una de la
familia y una por cada capacidad anunciada. Todos los resultados deben pasar,
sin extras/duplicados/omisiones, y cada observación debe llevar SHA-256 de evidencia.
El orden de observaciones no altera el hash. `Proof` y `Outcome` no implementan
Deserialize: no pueden importarse desde JSON como autoridad. Esto comprueba
invariantes de la evidencia interna, no su autenticidad física por sí solo.

## Core y persistencia

Core rechaza ID/contexto inválidos, REVOKED, estado no instalado y manifiesto
inconsistente antes de llamar al ejecutor. Un permiso denegado al iniciar no se
confunde con un fallo de conformidad ni cambia la instalación. Tras la ejecución,
Core comprueba snapshot y actor; entrega la prueba tipada al repositorio.

`SqlAdapterConformance` confirma en una sola transacción:

- CAS del snapshot completo y de la revisión registrada;
- estado VALIDATED si pasa, o INSTALLED si falla, sin alterar trust/manifiesto;
- recibo en `metadata.larm_conformance_v1`;
- journal `adapter.conformance` en COMPLETED o FAILED, ligado a run/binding/outcome;
- evento publicado `adapter.updated` y su entrada de outbox.

Un cambio concurrente del hash de paquete registrado, permisos, metadata, actor, confianza
o revisión impide aplicar el resultado viejo. El repositorio no lee archivos:
el ejecutor debe verificar el paquete físico antes/después del trabajo y antes
de confirmar. Promoción/habilitación/rollback y fuentes ya exigen el gate físico
interno descrito abajo; falta componerlo en el daemon y en consumidores de ejecución.

El recibo incluye además el hash del estado confirmado completo (revisión i64
exacta, estado, trust, manifiesto, metadata y timestamps), excluyendo sólo el propio
recibo para evitar circularidad. Promoción, habilitación y rollback P024 exigen
ese hash vigente y el mismo recibo en un journal COMPLETED válido, dentro de su
transacción SQLite. VALIDATED sin recibo no basta. Una evidencia ausente, alterada,
de otra instalación o anterior a cualquier cambio no concede elegibilidad.
Desactivar sigue permitido y una activa obsoleta no reemplaza al último bueno.

Habilitar una fuente P043 exige la misma comprobación y que las capacidades del
binding coincidan con el manifiesto confirmado. Registrar una conexión sigue sin
ejecutar ni cualificar: cambia revisión/metadata y requiere una validación posterior.
Los bindings deshabilitados pueden configurarse mientras se completa ese trabajo.

El retry idéntico del mismo actor devuelve el recibo histórico y no escribe otra
vez ni restaura VALIDATED tras una revocación posterior. Cambiar el veredicto,
binding o actor del mismo run se rechaza. Leer un recibo comprueba hashes, fase,
recursos y la coherencia del payload; un recibo corrupto no se acepta. Su lectura
no afirma que la instalación siga siendo válida: consultar además su estado actual.

La auditoría de fallo sólo conserva una categoría de código conocida. Core
devuelve el Error original, incluidos códigos desconocidos, sin sustituirlo por
esa categoría ni guardar mensajes privados en metadata/journal/eventos.

Este commit no selecciona, habilita ni promueve el adaptador, y no concede
reservas. La ejecución de la suite debe tener su propia admisión y ciclo de vida;
la transacción de metadata no mantiene locks mientras se ejecuta un plugin.

## Gate físico de uso

`AdapterQualification` recibe la instalación y el Run del journal que SQLite ya
ha comprobado. Devuelve un guard interno no deserializable. `NativeConformanceRunner`
implementa este puerto sin lanzar un worker: resuelve NativeTarget actual y vuelve
a medir paquete/manifiesto, ejecutor/suite, perfil, inputs, entorno efectivo,
raíces, límites y concesión P022. Su ausencia deniega con
`conformance_physical_gate_unavailable`; un recibo válido por sí solo no habilita.

`SqlAdapterSelections` lo exige al promover, habilitar (también no-op) y hacer
rollback. Sólo conserva como último bueno una activa que siga pasando el gate.
`SqlSources` lo exige al crear/editar una fuente habilitada. Conservan los guards
hasta COMMIT y llaman `recheck` después de preparar journal/evento/outbox: un
cambio o error revierte la transacción completa y conserva el error original.
Desactivar y configurar fuentes deshabilitadas siguen disponibles sin ese IO.
Un replay de creación sólo devuelve el recibo histórico; no vuelve a activar.

En Windows se mantienen handles que niegan escritura/borrado de los archivos
existentes del paquete, inputs y ejecutor. Se remide el inventario antes de
confirmar para detectar entradas añadidas. La admisión impide otra ejecución o
validación del mismo ID y `shutdown` espera los guards. Cancelar al observador de
un recheck no suelta handles/admisión mientras su medición sigue en curso.
Esto no sustituye ACL/AppContainer/aislamiento P130 ni el gate en futuros
consumidores de ejecución. Selección local y fuentes HTTP ya reciben ese puerto;
sin perfil vigente o con bytes/contexto cambiados se deniega la activación.

Los tests nativos de las cinco referencias comprueban además input cambiado,
archivo añadido, ausencia de mutaciones en SQLite, escritura denegada bajo guard,
recheck del inventario y habilitación tras restaurar, terminando deshabilitados.
Son nueve checks de gate por familia, separados de la prueba funcional P026.

## Ejecutor nativo y ownership

El composition root debe resolver un `NativeTarget` por instalación: `WorkerSpec`,
concesión P022 del mismo actor, perfil de capacidades relativo al paquete, inputs
inmutables, timeout y una `NativeSuite` compilada con el host. No se deserializan
planes, suites ni observaciones recibidas del cliente como autoridad.

El motor verifica manifiesto/bytes reales, autorización y actor antes de lanzar.
Mide el binario actual del host, descriptor de la suite, inputs, perfil completo,
entorno efectivo congelado, paths/raíces resueltas, concesión y límites. El perfil
se liga por bytes y cobertura de IDs; activar mapping/estimador y observar su
conducta pertenece al SDK del worker y a los oráculos completos, sin callbacks
ficticios del host. Las suites deben declarar Access exacto y ligar toda su
configuración e inputs efectivos; su propio IO debe terminar antes de devolver.

Cada arranque/reinicio vuelve a comprobar bytes/contexto. Los workers funcionales
usan `GuardedWorker`; los probes previos al handshake conservan la misma
construcción del proceso y autorización P022, con canales acotados y sin aceptar
un ID de paquete/hash distinto como sustituto del original.
El motor espera proceso y stdout/stderr antes de devolver `Execution`; vuelve a
medir después. Un cambio de archivo/contexto o cobertura incompleta invalida el
resultado. El timeout cancela el oráculo y pasa por la misma barrera de cierre.

Una sola ejecución por ID conserva admisión también cuando se cancela al
observador. `shutdown()` cierra admisión y espera todos los propietarios, incluidos
los retenidos por canales/proceso que aún no terminan. Los lectores y wait del
proceso conservan el scope nativo incluso si falla el handshake o hay un panic.
No se confunde un timeout de cleanup con IO terminado ni se confirma metadata.
Esto no sustituye reservas/leases de los propietarios P019/P050 ni aislamiento P130.

El IO auxiliar del oráculo se registra con `NativeIo` en `NativeSession`. Se cierra
en la barrera final, también tras timeout y panic; un reinicio del worker lo
conserva. Si el cierre tarda más que la gracia, no hay Execution confirmable y el
propietario conserva admisión hasta terminar. `shutdown()` espera ese propietario.
Las conexiones SQLx del inspector del ledger usan este cierre explícito.

Los dieciocho tests del motor ejecutan procesos reales con P022, pero usan observaciones
sintéticas **CODE_PASS** para probar el ensamblado de evidencia y el ownership.
Cubren binding determinista con nonce nuevo, permisos/actor/bytes inválidos,
perfil/suite ajenos, cambio de inputs/paquete, cobertura incompleta, error original,
timeout, cancelación del observador, admisión concurrente, shutdown, panic,
handshake fallido y crash/reinicio. El probe que ignora EOF se termina y drena;
un nuevo arranque en el mismo contexto vuelve a completar handshake/RPC y cierra
cooperativamente. Una respuesta tardía invalida cobertura y el cierre conserva
el primer fallo del oráculo. Los cuatro tests del gate añaden archivos bloqueados, once cambios de bytes/contexto,
inventario nuevo, retry y cancelación del observador con IO retenido.
No cualifican las cinco referencias funcionales.

## Probes del paquete original

`NativeSuite::protocol_probes_first` evita el handshake automático antes de probar
el inicial. `wire::run_common` ejecuta 16 casos por familia: handshake obligatorio,
major incompatible, nonce/capacidades, campos extra, parámetros nulos/extra, JSON
malformado/UTF-8/NaN, ID inválido, método no anunciado, cancel de ID ajeno, límite
exacto de 1 MiB/+1, exceso no JSON y recuperación mediante un RPC válido. Comprueba
que no cambien archivos ni directorios de work antes del handshake compatible.
El EOF exige salida correcta y ningún frame sobrante; no descarta un duplicado
tardío para convertir un retry en PASS.

Los cinco paquetes originales ya recorren ese transporte nativo bajo P022: **80
casos y nueve observaciones reales por familia**. Los informes conservan IDs,
expected/observed, tamaños y hashes de inputs/respuestas/stdout, snapshot de estado
y cierre. No contienen payloads privados ni importan veredictos externos.

Son nueve checks parciales: handshake, schema, major, request_id, duplicate_reply,
errors, frame_limit, malformed_json y unannounced_method. Cancelar un ID ajeno no
demuestra cancelación cooperativa de una operación. Faltan los seis checks de
cancel/progress/crash/permissions/state/io_recovery y los de familia/capacidad.
El test exige que el motor rechace esa cobertura incompleta: no concede VALIDATED.

## Receta nativa de runtime/echo

`runtime::ReferenceRuntimeSuite` sólo acepta el paquete `reference.echo` y su perfil
exacto `vendor.reference.echo`. Añade los seis checks comunes restantes, el de
familia y el de capacidad a los nueve del transporte: **17 observaciones reales**.
Invoca las **15 firmas runtime** con parámetros válidos: cinco tienen una operación
aplicable y diez deben responder `UNSUPPORTED_CAPABILITY` con el motivo declarado.
Los rechazos no se contabilizan como operaciones funcionales soportadas.

El oráculo compara la versión observada por otro proceso y el hash del ejecutable;
recibe progreso de un sondeo vivo, cancela su ID y comprueba que termine su hijo.
Rechaza una ruta fuera de la concesión sin alterar su sentinel, un modelo ausente
y un límite de memoria no soportado. Contrasta el hash JCS del plan, los parámetros
efectivos y memoria UNKNOWN; observa arranque, PID/imagen del paquete, salud READY,
eco HTTP independiente, parada, puerto disponible, plan de un solo uso y generación.
Provoca un crash durante otro sondeo, comprueba cierre de hijos y reinicia: el plan
anterior no reaparece y un plan nuevo permite otro eco y parada completos.

El test del composition root usa Core, SQLite migrado y el runner P022 reales.
Comprueba recibo VALIDATED y gate de promoción; después fuerza una expectativa de
versión incorrecta y verifica el error original, recibo fallido, estado INSTALLED
y rechazo de habilitación, con journal/evento/outbox. La selección permanece
deshabilitada. También pasan los nueve checks del gate físico. El endpoint público
usa esta receta registrada en SQLite. Faltan automatizar los recursos auxiliares
de perfiles de producto y componer los consumidores de ejecución. Los informes se conservan por ejecución.
Conservan el digest de Run y los checkpoints de cancel/crash para recalcular los
hashes de las ocho observaciones funcionales desde el informe persistido.

La receta acredita el launcher/echo y sus planes/instancias volátiles. No acredita
inferencia, el ledger de jobs Core ni aislamiento del sistema operativo P130.
`sysinfo` reutiliza la versión ya fijada para observar sólo la imagen del echo en
el paquete propio; no termina procesos ajenos.

## Verificación y trabajo restante

`source::ReferenceSourceSuite` valida la referencia `reference.catalog` con su
perfil exacto y cinco capacidades source. Ejecuta las nueve firmas y añade doce
observaciones a las nueve wire: **21 checks reales**. Los 99 casos funcionales
incluyen paginación, metadatos/redacción, filtros, revisión/manifiesto JCS, estimado
fijado sin admisión Core, sincronización/ETag, permisos, 401 real y presupuesto
antes de aceptar un job. Descarga bytes propios, contrasta SHA/tamaño y JobState,
prueba replay tras reinicio, corte HTTP y Range/If-Range, cancel/progreso, triggers
SQLite en aceptación y commit final, recursos retenidos en estado incierto,
recuperación sin volver a descargar bytes verificados y crash con partial durable.

El control HTTP y los bytes esperados pertenecen al harness, fuera de los roots
del worker; sólo inyectan respuestas HTTP, nunca PASS. El inspector Rust observa
el SQLite privado mediante SQLx fijado, sin cargar código del plugin ni depender
de `larm-db`. Su cierre se registra en la sesión. Los snapshots, requests HTTP y
checkpoints quedan en el informe; pueden recalcularse sus hashes con otro motor JCS.
El test de composición confirma el recibo real en Core/SQLite y prueba el gate de
promoción con la selección deshabilitada. No cualifica una descarga P050, un
upstream remoto ni concede una ruta pública de validación.

Las pruebas de dominio y Core usan observaciones/puertos de fixture (CODE_PASS).
Las pruebas de persistencia usan SQLite real y comprueban CAS concurrente,
reinicio, replay, revocación, snapshot alterado, recibo corrupto, máximo i64 y
triggers que abortan instalación, journal, evento y outbox. Verifican que todo
revierte y que un retry posterior confirma una sola vez. No cualifican un worker.

Los oráculos de procesos reales siguen en `p026_wire_real.py`,
`p026_negotiation_real.py` y los cinco runners funcionales. Sus paquetes y mutantes
tienen hashes propios: no se reutilizan como PASS de otra instalación.

La receta `protocol::ReferenceProtocolSuite` completa las cuatro firmas y **17
checks reales**, con 100 casos funcionales y 16 wire. Contrasta perfil/política,
schema IDs y fingerprints JCS, invocación/fixture HTTP, metadatos desconocidos,
errores antes de red y ocho fallos HTTP (401/429/redirect/JSON/schema/tamaño/tipo/
longitud). Observa progreso y heartbeat durante una llamada retenida; rechaza un
duplicado semántico sin otro POST y cancela por `protocol.cancel` y `core.cancel`.
Prueba timeout de IO real y reutilización del ID, crash/reinicio del worker sin
estado activo residual y muerte/reinicio del backend propio con PID/imagen
observados. El proxy no emite resultados de conformidad; reenvía al echo real.
El work permanece intacto, no sigue redirect ni envía autorización. El informe
conserva respuestas/fingerprints, errores y traza de HTTP para otra auditoría JCS.
Su recibo pasa por Core/SQLite y el gate de promoción deshabilitada. No acredita
inferencia ni admisión de leases de Core; la identidad de backend que el worker
no observa sigue UNAVAILABLE en su fingerprint.

`transformation::ReferenceTransformationSuite` completa cuatro firmas y **17
checks reales**, con 176 comprobaciones funcionales y 16 wire. Una fixture binaria
propia de 12 MiB+5 se transforma a Base64, se decodifica con Rust y contrasta SHA,
size, provenance, manifiesto JCS completo y URI del JobState privado existente.
Prueba plan en seco, presupuesto/bindings/permisos, plan alterado/expirado, input y
output corruptos, replay inmutable, cancelación con checkpoint, crash y reinicio
desde cero conservando el intento previo. Inyecta fallos reales de outbox al
admitir, completar y registrar un error: la recuperación conserva la causa original
de integridad antes del error secundario. Los intentos inciertos retienen recursos;
sólo un plan nuevo verifica el input y completa otro intento. EOF cancela y drena.
El ledger termina con seis jobs terminales y sin locks/reservas. El inspector SQLx
usa NativeIo y el recibo recorre Core/SQLite/gate de promoción deshabilitada.
No acredita conversión de modelos ni admisión/reservas globales de Core.

`benchmark::ReferenceBenchmarkSuite` completa las tres firmas y **17 checks reales**,
con 995 comprobaciones funcionales y 16 wire. Mide seis peticiones HTTP al echo
Rust propio (un warmup y cinco repeticiones); el inspector contrasta tiempos
monotónicos, bytes wire, fixture, fórmulas de media/desviación/mediana/p95/IC95 y
muestras durables del SQLite privado. El informe publicado y el devuelto se
comparan por JCS: 0 y 0.0 tienen la misma identidad canónica. Tokens, inferencia,
RAM/VRAM y backend no observado conservan null/UNAVAILABLE.

Prueba informes falsificados y archivo alterado, validación tras reinicio sin
red, ocho fallos HTTP, colisión de publicación, cancelación/EOF, crash tras muestra
durable, triggers de muestra/outbox y muerte/reinicio del backend propio. Un
informe de medición FAIL es un run COMPLETED; un fallo de persistencia es FAILED.
Los 19 runs terminan (13 COMPLETED, cuatro FAILED, dos CANCELLED), sin locks ni
reservas. Core/SQLite confirma el recibo y el gate de promoción deshabilitada.
No acredita inferencia ni admisión global de benchmark Core.

El catálogo persistente y la inyección del gate físico en selección/fuentes ya
están compuestos. Falta automatizar los recursos de los oráculos y los perfiles
generales de producto, y componer los consumidores de ejecución.
Después, integrar
el runner formal P026, ejecutar `-Real` y registrar PromptEvidence, conservando
los bloqueos de dependencias sin REAL_PASS. No habilitar una entrada pública que
acepte veredictos externos como sustituto de ese trabajo.
