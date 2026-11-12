# P026 — Conformance SDK para las cinco familias de plugins

**Estado: BLOCKED_REAL, únicamente por la dependencia P025.** El 2026-10-20
pasaron las **52 pruebas propias**: 51 grupos de preparación y una auditoría
independiente final. El estado lo calcula PromptEvidence; no se fuerza REAL_PASS.
La puerta `tools/check.ps1 -Prompt P026 -Real` terminó **13/13 PASS**.

Se revalidaron **631 tests Rust en 83 grupos**, sin fallos ni omisiones,
fmt/clippy, arquitectura, 53 copias normativas, lock y shell Tauri. La interfaz
pasa lint, typecheck, **39 tests** y build. Los 51 logs y **284 fuentes/contratos**
quedan ligados al recibo; sus hashes antes y después son idénticos.

Las cinco recetas compiladas reconstruidas desde perfiles persistentes recorren
host protegido, router autenticado, Core y SQLite: runtime 17 observaciones/15
firmas, source 21/9, protocol 17/4, transformation 17/4 y benchmark 17/3. Son
**89 observaciones completas**, **45 wire parciales** que no cualifican por sí
solas y **45 comprobaciones físicas** de habilitación/promoción. Se contrastan
Run, Proof/Outcome, perfiles, aprobación consumida, recibos, jobs y journals.
La matriz adicional SDK/host completa **145 casos** y negociación/ownership 36.
Los ejecutores controlados del framework conservan su clasificación CODE_PASS.

Tres escenarios daemon/TCP reales pasan **64 comprobaciones**: sondeo sin perfil
14, perfil runtime/reinicio/selección 25 y catálogo de fuentes habilitadas 25.
El escaneo completo recuperó las dos credenciales autorizadas desde Windows
Credential Manager y comprobó **210.398 archivos / 3.752.266.648 bytes** sin
coincidencias, en 47,9 s. Sus valores no se escriben en la evidencia.

[PromptEvidence actual](P026_conformance-sdk-para-las-cinco-familias-de-plugins.evidence.json) ·
[Preparación completa: 51 grupos](P026_runs/preparation.checks.json) ·
[Puerta general -Real](check_P026_real.json) ·
[Fuentes HTTP: 25 checks](P026_runs/source-api-real.json) ·
[Auditoría final](P026_runs/P026-CONTRACT-independent-native-audit.log) ·
[Runtime y auditoría](P026_runs/native-runtime-audit.json) ·
[Source y auditoría](P026_runs/native-source-audit.json) ·
[Protocol y auditoría](P026_runs/native-protocol-audit.json) ·
[Transformation y auditoría](P026_runs/native-transformation-audit.json) ·
[Benchmark y auditoría](P026_runs/native-benchmark-audit.json) ·
[Guía SDK](../adapters/authoring.md).

Las preparaciones anteriores 37/37 y 11/11, y el gate sin -Real 12/12, son
históricos. Sus recibos, logs e informes originales están preservados en el
[snapshot previo a la ejecución formal](P026_runs/formal-baselines/20261020T120026Z/index.json).
El objetivo de desarrollo de toda LARM sigue activo. La siguiente fase del
manifiesto es P030; sus dependencias también conservan sus estados reales.

## Sondeo público durable verificado

`POST /api/v1/adapters/{id}/probe` exige `adapters:write`, Idempotency-Key y el
Probe contractual exacto. Rechaza campos extra/duplicados, referencias anulables
ausentes, destino distinto y perfil no registrado antes de aceptar trabajo.
Responde 202 con el JobReceipt original QUEUED; `/api/v1/jobs/{id}` filtra por
propietario/admin. El replay liga actor/cuerpo/destino durante 30 días y no
repite workers, eventos ni escrituras.

La aceptación sobrevive a la desconexión del observador. El servicio supervisado
reclama QUEUED en SQLite y espera el IO nativo antes de cerrar un error. El kernel
confirma instalación, recibo/journal de conformidad y estado terminal del job en
una transacción con eventos/outbox. Un commit incierto bloquea la elegibilidad
anterior del mismo snapshot hasta revalidación; una petición posterior denegada
no elimina esa restricción. Un rechazo previo de permisos conserva la validación
histórica. Los errores públicos omiten texto privado y conservan su categoría.
Shutdown cierra admisión y espera tareas/IO. Un RUNNING de un proceso anterior
no se reejecuta ni se declara cerrado por tiempo: indica recovery_required.

Pasan **tres tests API**, **siete de persistencia** y **dos de servicio** con
fault injection, aceptación cancelada, IO retenido, commit/replay y privacidad.
Sus ejecutores controlados son CODE_PASS. Las cinco recetas nativas pasan por el
mismo servicio/router/guard con credenciales efímeras en memoria; se auditan los
jobs, sus recibos y replay junto con los oráculos físicos. Estos recorridos no
abren un listener TCP.

El [daemon/TCP real](P026_runs/probe-api-real.json) pasa **14 checks** con SQLite
y credenciales temporales de Windows: auth/scopes, DTO, ausencia de suite,
instalación inalterada, journal/outbox, replay/conflicto, privacidad, shutdown y
reapertura. En este caso la instalación no tiene un perfil registrado y el job debe fallar
UNSUPPORTED_CAPABILITY sin cualificar el paquete. Las credenciales y procesos del
harness son propios y se cierran al terminar. OpenAPI coincide con la ruta.

El recibo actual verifica **631 tests Rust en 83 grupos**, 145 casos wire/host,
fmt/clippy, arquitectura, copias normativas, lock y los checks públicos anteriores.
La auditoría Python reconstruye las 134 observaciones completas/parciales,
Proof/Outcome, recibos y jobs de las cinco familias. Se conservan cinco intentos
fallidos del recolector, además de informes de ejecuciones individuales; ninguno
se reetiqueta PASS. Un watcher del harness que leía un control reemplazado
atómicamente ahora reintenta PermissionError/FileNotFound durante un segundo;
los datos inválidos siguen siendo fatales. La regresión posterior pasa.

## Perfiles persistentes y selección real

El registro local `register-adapter-probe-profile` consume una aprobación P022
con CAS y guarda perfil/evento/outbox en la misma transacción. Su fichero contiene
datos de una de las cinco recetas compiladas; no acepta código, Proof ni PASS.
Valida el paquete publicado, actor, manifiesto y referencias de input. READY
significa perfil registrado y conserva INSTALLED/UNTRUSTED. La emisión inicial y
cada sustitución requieren una aprobación disponible. Un sondeo activo impide
cambiar su contexto; errores de outbox o escrituras ignoradas revierten también
el consumo y la sustitución. Pasan **siete tests SQLite** y **cuatro de contratos
internos**; estos tests no cualifican workers.

`PersistentTargets` reconstruye la receta y sus raíces desde SQLite en cada uso.
El hash JCS del registro completo entra en el descriptor de suite; renovar perfil
o concesión cambia el binding y requiere revalidar. La lectura comprueba el
snapshot de aprobación consumido. El TTL corto de aprobar un uso se aplica al
emitir; después P022 aplica el expires_at explícito de la concesión y el actor,
sin convertirla en autorización indefinida. No se heredan variables ni secretos.

Las cinco referencias reales ya usan ese registro y un resolver nuevo sin catálogo
en memoria. Los informes de cada job conservan el perfil, su SHA, la aprobación
consumida y el journal completo de Core; la auditoría Python reconstruye los hashes
Run/Outcome/recibo y contrasta actor/manifiesto/consumo. Incluye ambos resultados
runtime, también el negativo. Las 89 observaciones completas y 45 parciales y los
45 checks físicos siguen pasando.

El [recorrido de perfil por daemon/TCP](P026_runs/profile-api-real.json) pasa
**25 checks**: rechaza receta/campos/paquete/input/actor ajenos sin efectos;
registra y consume una vez; reinicia el daemon y ejecuta la receta runtime real;
confirma 17 observaciones, VALIDATED/UNTRUSTED y paquete sin cambios; replay no
crea otro worker y otro actor no hereda la concesión. Otro reinicio conserva el
job completo. La fila inicial de instalación es una fixture explícita, no acredita
P028/importación. Los recursos son propios y las credenciales temporales se revocan.

La selección CLI acepta la identidad de la concesión y reconstruye el mismo
contexto en otro proceso. Promoción/habilitación/no-op pasan por el gate físico;
mutar un input deniega el no-op sin escrituras, desactivar sigue permitido, y
restaurar bytes permite habilitar de nuevo. La fixture termina deshabilitada.
El catálogo HTTP de fuentes recibe el mismo runner que los sondeos, con admisión
compartida; su escenario de habilitación pública sigue pendiente. Preparar recursos
auxiliares de oráculos y perfiles generales automáticamente, y conectar futuros
consumidores de ejecución, sigue siendo trabajo de la app.

El intento que no compiló la prueba nueva de escritura ignorada se conserva con
sus once logs en [el recibo fallido](P026_runs/native-engine-attempts/20261019T044245Z-profile-test-sql-literal/checks.json).
SQLx exige sentencias literales: el test usa dos literales fijos sin eludir esa
protección. Después pasan los siete tests focales y las once puertas completas.
El [checkpoint anterior](P026_runs/native-engine-checkpoints/20261019T0418Z-public-probe/checks.json)
conserva los diez logs del incremento público previo como evidencia histórica.

## Implementación nativa, revalidada el 2026-10-20

`ReferenceRuntimeSuite` sólo admite `reference.echo` y su perfil exacto
`vendor.reference.echo`, ligado junto con fixtures, inputs y contexto por el motor
nativo. Ejecuta los 16 casos wire y **48 comprobaciones funcionales**: versión
observada por otro proceso/hash del binario; diez métodos no aplicables con DTOs
válidos y rechazo explícito; progreso/cancelación de un sondeo con hijo real;
permisos, sentinel sin cambios y modelo ausente; memoria UNKNOWN/límite no
soportado; compromiso JCS y parámetros del plan, arranque/PID/imagen, salud READY,
eco HTTP independiente, parada/puerto disponible, plan de un solo uso y generación;
crash de un sondeo, cierre de hijos y reinicio sin recuperar el plan anterior,
seguido de nuevo arranque/eco/parada. Las cinco operaciones aplicables y los diez
rechazos cubren las 15 firmas; no se anuncian operaciones no soportadas.

Los tests del composition root invocan el router HTTP autenticado, la cola durable,
Core, el runner P022 y SQLite migrado reales; usan credenciales efímeras en memoria.
El recibo positivo deja VALIDATED, conserva UNTRUSTED y pasa el gate de promoción
con la selección deshabilitada. Una segunda suite con expectativa de versión
deliberadamente incorrecta conserva el error original en el informe nativo,
publica un error seguro con recibo fallido y verifica la vuelta a
INSTALLED, revisión siguiente y habilitación rechazada. Ambos journals se
conservan y evento/outbox coinciden. Las fixtures se cierran y eliminan; no se
promueve ninguna instalación del operador. Los recibos de cada job conservan
aceptación, resultado y replay. El listener TCP se prueba por separado.

Los informes conservan Run SHA y checkpoints de cancel/crash. Se reconstruyeron
con Python RFC 8785 los hashes de las 17 observaciones, Proof/Outcome y ambos
recibos, además de las 45 observaciones wire parciales de los cinco originales.
Esta receta acredita launcher/echo y planes/instancias volátiles; no inferencia ni
ledger de jobs Core. La observación de imágenes propias reutiliza `sysinfo`
0.39.6 ya fijado en el workspace; no incorpora otra versión de dependencia.

`ReferenceSourceSuite` ejecuta **16 casos wire y 99 comprobaciones funcionales**
sobre `reference.catalog` y su perfil exacto: nueve métodos y cinco capacidades.
Comprueba paginación, filtros OR/AND, metadatos desconocidos/null y redacción,
revisión y manifiesto con JCS independiente, tamaño/SHA fijados, estimado sin
elegibilidad Core, sincronización/ETag, permisos, 401 y presupuesto insuficiente
antes de admitir trabajo. Descarga bytes propios, verifica publicación y JobState,
replay tras reinicio, corte HTTP y reanudación Range/If-Range, progreso/cancelación,
fallos reales de outbox en aceptación y commit final, recursos retenidos en estado
incierto, recuperación sin retransmitir bytes verificados y crash con partial
durable. Otro intento recupera el mismo job y verifica sus bytes.

El inspector Rust lee directamente el SQLite privado con SQLx **0.9.0**, ya fijado;
su conexión queda registrada como IO auxiliar de la sesión nativa. El control HTTP
y los bytes esperados pertenecen al harness fuera de los roots del worker y se
ligan por descriptor/inputs; sólo provocan respuestas HTTP reales, nunca PASS.
Al terminar hay **cinco jobs terminales** (cuatro COMPLETED y uno CANCELLED), sin
locks/reservas y con eventos/outbox correspondientes. La traza HTTP prueba
Range/If-Range y ausencia de cabecera de autorización sin almacenar su valor.
La auditoría Python RFC 8785 reconstruye Run, las 21 observaciones y Proof/Outcome.
El test real de Core/SQLite confirma VALIDATED/UNTRUSTED, recibo vigente y gate de
promoción con selección deshabilitada. Acredita el ledger de referencia, no
admisión/descargas de producto P050 ni un upstream remoto.

`ReferenceProtocolSuite` completa **17 observaciones**, las **cuatro firmas
protocol**, **100 comprobaciones funcionales y 16 wire** con Python/SDK autónomos
y un backend echo Rust real. Contrasta política/perfil y schema IDs, invoke con
ID semántico, respuesta/fixture fijada y fingerprints JCS; mantiene tokens null e
identidad de backend UNAVAILABLE cuando el worker no la observa. Rechaza endpoint,
lease, capacidad, input, streaming/generación y permisos ajenos antes de la red.
La generación negativa usa el objeto Generation válido del contrato, para probar
opción no soportada en el callback y no el rechazo previo de un DTO inválido.

El proxy de fixture provoca 401/429 (Retry-After preservado), redirect no seguido,
JSON inválido, output fuera de schema, tamaño, Content-Type y Content-Length
incorrectos. No suministra PASS: las respuestas válidas vienen del echo real,
cuyo PID/imagen se observan. Una llamada retenida permite comprobar progreso,
heartbeat, duplicado semántico sin otro POST, `protocol.cancel`, `core.cancel`,
timeout HTTP real y reutilización del mismo ID. También se mata el worker propio,
se reinicia sin mapa activo residual y se mata/reinicia el backend propio. La
indisponibilidad admite error HTTP del proxy o fallo de conexión, ambos
UPSTREAM_UNAVAILABLE/retryable; se conserva la causa exacta observada.

No cambia el work ni su sentinel; sólo hay requests `/health` y `/echo`, sin
autorización. El informe conserva respuestas, fingerprints completos, ambos IDs,
errores/traza HTTP y generaciones del backend. La auditoría Python RFC 8785
reconstruye las 17 observaciones, siete fingerprints, seis respuestas, Run y
Proof/Outcome; Core/SQLite confirma recibo y gate de promoción deshabilitada.
Esto cualifica transporte echo, no inferencia ni admisión de leases Core.

`ReferenceTransformationSuite` completa **17 observaciones**, sus **cuatro firmas**
y **176 comprobaciones funcionales + 16 wire**. Transforma una fixture propia de
12 MiB+5 a Base64 y contrasta bytes/decodificación mediante Rust independiente,
SHA/tamaños, provenance, manifiesto JCS completo (incluido el artefacto opcional)
y URI exacta del JobState privado existente. Rechaza recursos, parámetros, formato,
versión, presupuesto, permisos, plan alterado/expirado e input cambiado antes de
admitir; un plan seco no publica archivos. También detecta output corrupto y
preserva recibos y errores de un plan tras completar otro intento.

Prueba cancelación después de un checkpoint fsync, crash del worker, conservación
de partial y metadata `.tmp` no confirmada, nuevo intento sin reanudar offsets y
EOF con cancelación/drenaje. Inyecta outbox fallido al admitir, al completar y al
registrar un error: la aceptación revierte toda su transacción; un commit incierto
retiene journal/lock/reserva y la publicación anterior; la recuperación conserva
la causa original INTEGRITY_FAILED antes del error secundario de persistencia.
Sólo un preflight/plan nuevo genera otra salida y libera recursos al completar.

El ledger termina con **seis jobs terminales, diez intentos/journals**, cuatro
COMPLETED y dos CANCELLED, sin locks/reservas; cada evento tiene su outbox. Se
conservan hashes y metadatos de cuatro salidas completas, que la auditoría Python
reconstruye junto con las 17 observaciones y Run/Proof/Outcome. La conexión SQLx
participa del cierre NativeIo. Core/SQLite confirma VALIDATED/UNTRUSTED y el gate
de promoción con selección deshabilitada. `base64` 0.23.1 y `url` 2.5.8 reutilizan
versiones ya fijadas. No acredita conversión de modelos ni admisión/reservas de
producto, y ninguna instalación del operador se promueve.

`ReferenceBenchmarkSuite` completa **17 observaciones**, las **tres firmas** y
**995 comprobaciones funcionales + 16 wire** sobre el paquete registrado bajo
P022. Mide HTTP contra el echo Rust propio: un warmup y cinco repeticiones por
informe. Contrasta muestras monotónicas enteras en ns, seis peticiones reales,
wire SHA, fixture, hashes del artefacto y filas SQLite. El oráculo Rust calcula
media, desviación muestral, mediana, p95 e IC95; otro verificador Python usa
`statistics` sobre las muestras persistidas. Compara informe publicado/devuelto
por JCS, conservando la identidad numérica válida de 0 y 0.0.

Rechaza recursos/hashes/permisos ajenos antes de HTTP y valida sin nuevas
peticiones. Detecta cinco informes falsificados, alteración física del artefacto
y replay tras reinicio. Provoca 401, 429, redirect, JSON/schema/tamaño/tipo/longitud
inválidos, colisión de publicación, cancelación y EOF tras muestra durable,
crash/reinicio y muerte/reinicio del backend propio observado por PID/imagen.
Triggers reales abortan aceptación, muestra y outbox final: conservan recursos en
estado incierto y la causa original de integridad; al reiniciar se liberan y otro
run recibe ID/artefacto nuevos sin sobrescribir el anterior.

El ledger termina con **19 runs** (13 COMPLETED, cuatro FAILED, dos CANCELLED),
**93 muestras durables y 53 eventos/outbox**, sin locks/reservas. Los 13 informes
de medición tienen cuatro PASS y nueve FAIL; un informe FAIL sigue siendo un
run COMPLETED, distinto de un fallo de ejecución/persistencia. Tokens, TTFT,
memoria y backend no observados conservan null/UNAVAILABLE; ninguna medición
obtiene elegibilidad de inferencia. Core/SQLite confirma VALIDATED/UNTRUSTED y el
gate de promoción con selección deshabilitada. No acredita admisión global de
benchmark Core. La receta registrada ya se ejecuta desde la ruta pública.

`tools/p026_native_audit.py` deja reproducible la reconstrucción independiente
de los hashes Run/observaciones/Proof/Outcome y recibos de las cinco recetas,
la revalidación negativa runtime y las 45 observaciones wire parciales: **134
en total**. Contrasta también ledgers, muestras/estadísticas de benchmark,
fingerprints protocol, las cuatro salidas transformation y los jobs públicos, perfiles persistentes y journals completos de Core
con replay de las cinco familias. El recibo formal actual liga 51 logs y 284
fuentes; el checkpoint anterior ligaba once logs y 274 fuentes. Un archivo JSON
con PASS no se importa como autoridad.

El incremento del gate físico queda verificado con las pruebas del **2026-10-18**
y auditoría del **2026-10-19**. `AdapterQualification`/`AdapterQualificationGuard`
son puertos internos: SQLite obtiene el Run de su journal comprobado y exige al
host medir otra vez paquete/manifiesto, suite/ejecutor, perfil, inputs, entorno,
raíces, límites y concesión P022. No recibe PASS del cliente ni arranca un worker.

Promoción, enable (incluido no-op), rollback y creación/edición de fuentes
habilitadas conservan ese guard hasta COMMIT y hacen recheck tras preparar
journal/evento/outbox. Un error revierte todas las escrituras y devuelve la causa
original. Windows niega escritura/borrado sobre los archivos existentes medidos;
se vuelve a leer el inventario para detectar entradas nuevas. Los handles se
cierran antes de liberar admisión. Cancelar al observador no abandona una medición
en curso, y shutdown espera al propietario restante. Esto no sustituye P130.

Las cinco recetas reales añaden **45 comprobaciones físicas**, nueve por familia:
input modificado, archivo añadido al paquete, SQLite/selección sin cambios ante
rechazo, escritura denegada en paquete/input bajo guard, recheck del inventario,
restauración seguida de enable y desactivación final. Cada resultado está ligado
al Run confirmado en su recibo y a la auditoría JCS; son checks de gate separados
de las 89 observaciones funcionales completas y 45 wire parciales.

Cuatro tests nativos adicionales prueban once cambios de bytes/contexto (perfil,
suite, entorno, timeout, grant, actor y work), locks reales, admisión, shutdown,
retry e IO retenido al cancelar. Los tests transaccionales usan puertos CODE_PASS
explícitos; comprueban fallos tempranos/tardíos, error desconocido preservado,
rollback/eventos/outbox, enable no-op, rollback y desactivación sin gate. La
composición pública ahora inyecta el runner en selección local y fuentes HTTP.
El daemon reconstruye las recetas desde SQLite; un perfil ausente/cambiado o una
concesión caducada deniegan activaciones. La prueba de selección usa procesos
reales distintos con el mismo perfil/contexto.

Se conservan dos intentos fallidos del gate físico del 18 de octubre: [timeout de fixture](P026_runs/native-engine-attempts/20261018T1945Z-physical-gate-fixture-timeout/checks.json)
y [observación sólo por PID](P026_runs/native-engine-attempts/20261018T1953Z-physical-gate-pid-observation/checks.json).
El caso de dos reinicios pasó aislado en 4,12 s con un presupuesto global anterior
de 5 s; ahora su presupuesto de fixture es 20 s, conservando cotas RPC/cancel/kill
y el negativo de timeout. La observación de cierre comprueba PID e imagen propia,
trata identidad ilegible como viva y distingue una imagen ajena; se prueba también
contra el propio proceso. El nuevo intento público también detectó un timeout de
fixture en el caso de reply tardío: se conserva su recibo y sus diez logs en
[el intento fallido](P026_runs/native-engine-attempts/20261019T0410Z-probe-fixture-timeout/native-engine.checks.json).
El margen de las suites positivas del framework pasa de 5 a 20 s, conservando
los negativos explícitos y cotas RPC/cancel/kill. Las 18 pruebas focales pasan.
El checkpoint incremental pasó once grupos y 631 tests. La ejecución formal
actual amplía la comprobación a 51 grupos y auditoría final; los intentos fallidos
conservan FAIL.

tools/p026_schemas.py genera 76 schemas autocontenidos, request/response de las 38
firmas, más índice de familia/required/referencias y hashes. Sólo reescribe refs a
$defs locales alcanzables; conserva restricciones. Scope/referencia externos no
soportados impiden generar. check exige nombres/bytes exactos, meta-schema e índice
Rust fresco; no se descarga un schema ni se implementa otro motor JSON Schema.

Rust embebe los 76 documentos mediante un índice generado y compila/cachea sus
validadores. Usa jsonschema **0.59.0**, MIT, Rust mínimo 1.85 (toolchain 1.99), con
default-features=false: sin resolución HTTP/file. Licencia/checksum/fecha se
contrastaron con el [registro oficial](https://crates.io/api/v1/crates/jsonschema/0.59.0)
y la API con [documentación oficial](https://docs.rs/jsonschema/0.59.0/jsonschema/).
Cargo.lock fija también las transitivas. environment.lock conserva el inventario
original y registra esta extensión P026; pasan **47 dependencias** coherentes.

El servidor Rust valida parámetros antes del callback y respuestas antes de enviar.
Inválidos de entrada son -32602; una salida inválida se normaliza a PROTOCOL_ERROR
sin el valor original y sin habilitar falsamente una sesión. Handshake incompatible
sigue rechazado antes de IO. Se conservan IDs, framing, cancel, EOF y traducción
de legado initialize de P020. El host conserva permisos, timeout y terminación.

El servidor Rust limita ahora **4 callbacks y 64 solicitudes en espera**. Controles
handshake/heartbeat/cancel independientes de esa admisión. Saturación devuelve
RESOURCE_UNAVAILABLE retryable/adapter_queue_full sin reemplazar un ID existente.
Un ID y su plaza se conservan hasta escribir la respuesta; stdout lento no permite
reusar prematuramente la identidad. Cancelar una llamada en espera la despierta y
resuelve sin ejecutar su callback, aunque las cuatro plazas sigan ocupadas.

EOF o error de lectura cancela y espera las tareas de la sesión. El seguimiento
con JoinSet/RAII libera admisión si una tarea se interrumpe, evita handles separados
y no inicia otra llamada tras observar el fallo. Se conservan frames de entrada
recibidos a medias: no se cancela esa lectura para recoger una tarea terminada.
Callbacks no cooperativos siguen bajo timeout/gracia/kill del host; estos límites
de solicitudes no acreditan aislamiento del proceso P130.

Python carga los mismos documentos y verifica hashes de fuentes/archivos, método,
familia, required, referencia y nombre exacto. Ausencia/corrupción/binding antiguo
falla sin fallback. Referencias externas están prohibidas incluso si se altera el
hash de un schema en el índice. Los paquetes autónomos source/protocol incluyen
estos archivos.

Ambos motores implementan la extensión publicada **x-maximum-decimal**. Una cadena
de 19 dígitos no basta para cumplir i64::MAX; 9223372036854775808 y
9999999999999999999 se rechazan. Python usa comparación acotada de cadenas; Rust,
parse u64 y comparación. El oráculo de tests usa enteros Python acotados, separado
del SDK, además del motor original de los schemas. No se cambió el límite ni se
redujo el contrato a restricciones estándar que ignorasen esa extensión.

## Capacidades y referencia de protocolo

`CapabilityRegistry` en Rust y Python comprueba descriptores privados cerrados:
schemas input/output, versión, hashes RFC 8785, mapping implementado, estimador
ejecutable y fixture fijada. Se rechazan referencias externas, duplicados, campos
extra, hashes/fixtures alterados y mappings/estimadores ausentes. Los alias son de
lectura; las 18 identidades publicadas exigen versión y schemas canónicos exactos.
No basta renombrar un echo como embedding. El generador produce **36 schemas** y
el índice Rust; los cinco tests de cada SDK comparten dos descriptores y **ocho
vectores JCS** con floats, límites y orden UTF-16. El ejemplo embedding es sólo
un testigo de contrato, sin ejecución de modelo.

El estimador recibe input validado y debe devolver `MemoryEstimate` válido antes
de anunciar. Las cifras desconocidas siguen siendo null/UNKNOWN; el helper no
concede memoria ni limita el proceso. El perfil es determinista y los errores de
schema no incluyen los datos privados rechazados. Los servidores de ambos SDK
exigen ahora el registro para todas las capacidades anunciadas por las cinco
referencias. Rust lo obtiene de `AdapterHandler::capability_registry` (también
reenviado por source); Python de `handler.registry`. Major e identidad se validan
antes de ejecutar estimadores. Un estimador inválido no habilita callbacks.
[Guía de descriptores](../adapters/capabilities.md).

Runtime incorpora una composición Rust explícita de `GenericRuntime` con el
descriptor echo, mapping y estimador implementados. Source registra cinco
operaciones: search, revisions, resume, sha256 y sync. El generador fija seis
descriptores con hashes y fixtures válidas. Los testigos source sólo son de schema:
sus jobs/planes reales los preparan los runners funcionales. La estimación source
declara buffers acotados de 2 MiB y memoria total UNKNOWN; no inventa un consumo
total ni restringe queries opacas usando su serialización para estimar memoria.

Los workers Python con estado abren locks y bases privadas en `open_session()`
después de validar major/identidad/capacidades/estimadores. El SDK lo llama una
sola vez tras negociación válida; un fallo de apertura no autoriza operaciones ni
filtra una excepción privada. Un segundo handshake válido no vuelve a recuperar
ni abrir la base. Se conservan las comprobaciones de permisos antes de IO.

El worker genérico materializado por el importador declarativo no adquiere una
implementación de capacidades por recibir un documento. Conserva las capacidades
como candidatas y rechaza negociar si no tiene registro. El test negativo del
host comprueba el rechazo antes del sondeo. Las fixtures de launcher, sin
inferencia implementada, declaran capacidades vacías; las fixtures de transporte
con capacidades mantienen descriptores/estimadores explícitos y sus respuestas
BLOCKED_REAL. No se borran capacidades de documentos del usuario.

La referencia runtime completa sigue ejecutando echo con el wrapper de referencia.
Su importación por la API se comprueba como candidata sin contrato operativo;
una segunda importación de launcher sin capacidades ejecuta el proceso HTTP real,
verifica hashes y termina la instancia al cerrar stdin. El schema exige el nombre
`bin/larm-generic-adapter.exe`; la fábrica conserva ese nombre y copia los bytes
reales del wrapper. Core sigue materializando su binario genérico propio.

La regresión encontró una particularidad real de Windows: leer el primer byte del
archivo de ownership de otro worker falla antes de poder intentar el lock. Las
tres referencias con estado adquieren ahora el lock físico antes de leer el
marcador. Un competidor recibe CONFLICT sin recuperar ni modificar DB/artefactos;
un marcador ajeno sigue rechazado, sin sobrescribirlo y liberando el lock del
intento fallido. La validación del marcador no se elimina.

Python usa `rfc8785==0.1.4`, instalado sólo en la venv del repositorio con hash
obligatorio. Versión, SHA del wheel y licencia Apache-2.0 quedan fijados en
environment.lock, requirements y los paquetes autónomos; datos contrastados con
[PyPI oficial](https://pypi.org/pypi/rfc8785/0.1.4/json). Ambos motores conservan el
límite de enteros del dominio (±2^53) y rechazan números no finitos.

La [referencia protocol](../../adapters/protocol/reference/README.md) implementa
sus **cuatro firmas** en un worker Python autónomo contra el ejecutable echo Rust
real. Tiene una capacidad propia con descriptor, mapping, estimador y fixture.
Sólo loopback literal, origen fijo, sin proxies/secretos/redirects, request 16 KiB,
reply 32 KiB, timeout IO 2 s y JSON estricto. Comprueba endpoint/variant/lease de su
perfil privado y opciones/schema antes de IO. No son leases admitidas por Core.

`qualify` ejecuta health y POST echo con fixture fijada; sólo cualifica este
transporte controlado. El fingerprint conserva desconocida la identidad del
backend y tokens no medidos. No certifica inferencia ni promueve el adaptador.
`protocol.cancel` señala el ID semántico de la llamada; `core.cancel`, el RPC.
Duplicados concurrentes no sustituyen el original; timeout/cancel/fallo liberan el
registro activo. Echo no modifica estado durable ni produce jobs/reservas ficticios.

El runner observa **24 pasos reales**: HTTP hasta el backend Rust, errores
401/429/302, Retry-After 900 conservado, JSON inválido/shape privado/tamaño/tipo/framing,
fixture incorrecta antes de IO, mala salida sin cualificación, cancel/duplicado,
timeout real y reintento explícito del mismo ID, muerte/reinicio del backend,
descriptor alterado sin handshake, controles comunes/EOF y paquete intacto.
[Recibo de protocolo](P026_runs/protocol-real.json). La comprobación de permisos
también cruza el host protegido; no acredita AppContainer P130.

## Transformación real y recuperación

La [referencia transformation](../../adapters/transformation/reference/README.md)
implementa describe/plan/execute/verify con Base64 streaming de un archivo binario
propio de hasta 64 MiB. Tiene descriptor completo y estimador de buffers; memoria
total UNKNOWN. Input/perfil sólo lectura, state/output como únicas raíces de
escritura, sin red, secretos, scripts ni procesos. Pausa no soportada. Es una
conversión real de fixture; P065/P093 conservan conversión/cuantización de modelos
y admisión de producto. No hay cambio de contrato/API/DB de Core ni interfaz.

Plan de 900 s con SHA/tamaños de entrada y salida, herramienta/versión, parámetros,
perfil y presupuesto. Execute revalida todo antes de admitir. El volumen se observa
realmente y se exige margen max(1 GiB, ceil 5 %) antes de fases/bloques. El payload
tiene tamaño exacto, el control una cota conservadora de 3 MiB con DB de hasta 1 MiB
y sidecars de hasta 16 KiB. `eligible=false`: las reservas/políticas globales de Core
no se acreditan con este ledger privado.

SQLite FULL mantiene job/intento/journal/evento `transformation.state_changed`,
outbox/lock/reserva en transacciones. Ownership físico antes de reconciliar;
checkpoint sólo después de fsync. Directorio de salida completo verificado y rename
sin sobrescritura. El manifiesto coincide con `BundleEntry` del dominio: bytes
numéricos al calcular el hash, aunque el DTO los represente como strings. El test
Rust usa el dominio y un decodificador independiente para comparar SHA, tamaños,
manifiesto y bytes recuperados. Base64 se añade sólo como dev-dependency del daemon,
reutilizando la versión ya fijada del workspace.

Fallos conservan causa, input, plan/hashes, temporales, journal y recursos. Tras
interrupción el nuevo dueño marca FAILED_RECOVERABLE; sólo input íntegro y nuevo
plan/reserva/attempt/generation permiten reiniciar desde cero. Intentos anteriores
y eventos permanecen, sin revivir COMPLETED/CANCELLED ni reanudar offsets. Cancel
pasa por CANCELLING/CANCELLED y retira sólo archivos enumerados del intento propio.

Un outbox/commit incierto no afirma éxito ni libera recursos. La causa redactada
se conserva fuera de la transacción fallida; si falla registrar un error, se
conserva la causa original antes que el error secundario. El registro usa
fsync+rename; recovery conserva pero no interpreta un `.tmp` incompleto como
metadata confirmada. JobReceipt sólo se emite
tras completar, con URL file:// de un JobState privado existente. Verify vuelve a
leer payload, procedencia y manifiesto, sin validar sólo la DB.

[Recibo nativo](P026_runs/transformation-real.json): **28 pasos** con 12 MiB+5 bytes
propios; preflight, binding/schema/budget/expiry, outbox de aceptación atómico,
fallo de escritura real, retry, procedencia, recibo existente/replay, corrupción,
cancel/EOF, muerte real/ownership, segundo worker denegado, input íntegro antes de
reiniciar y fallos de outbox de finalización y de registro de un error original.
**7 tests de archivos** cubren vacío/padding, límites de bloque, prefijo durable,
presupuesto/no sobrescritura, hash/tamaño, hash de bundle y junction real.

Una repetición detectó una carrera del oráculo de corrupción: truncar/rellenar el
input podía producir lectura corta en RUNNING antes del hash final en VERIFYING.
La DB y el sidecar conservaron correctamente esa causa de integridad. Se corrigió
la inyección para sobrescribir sin truncar un byte del primer bloque ya observado;
el caso vuelve a exigir la misma fase y causa original, sin omitir el fallo.

## Benchmark con evidencia durable

La [referencia benchmark](../../adapters/benchmark/reference/README.md) implementa
describe/run/validate sobre el servidor echo Rust real mediante un codec HTTP
independiente de protocol. Comparte la fixture y descriptor completos de
`vendor.reference.echo`; no anuncia capacidades de inferencia ni ejecuta las suites
LLM/ASR de producto. No modifica API, Core, migraciones ni interfaz.

Un calentamiento y cinco repeticiones con inicio/fin monotonic ns. Warmup queda
fuera de media/mediana/p95 nearest-rank/desviación muestral/IC95% con t=2.776 y
límite inferior cero. Duración cero, menos de cinco muestras positivas o errores
funcionales no dan una métrica aprobada. El snapshot fija suite/fixture/settings y
perfil, incluyendo límites y parámetros de generación declarados no aplicables.
Tokens, TTFT, carga y RAM/VRAM del backend siguen null con razones. PASS corresponde
a esta suite HTTP; `inference_qualification_eligible=false` conserva esa separación.

El fingerprint usa sólo name/value/status; refs y timestamps son trazabilidad.
Conserva UNAVAILABLE los componentes de modelo/backend/hardware no observados.
Dos runs pueden conservar identidad observable y tener trazas/tiempos distintos;
esa igualdad no certifica reproducción sobre componentes desconocidos.

Ledger SQLite FULL con run, muestras/observaciones, journal, evento/outbox,
lock/reserva y ownership física. DB hasta 1 MiB, artefactos/informes hasta 128 KiB,
reserva privada conservadora 3 MiB más margen 1 GiB y límite32 mediciones por work
de referencia, sin borrar historial automáticamente. Publicación de directorio
completo fsync+rename sin reemplazar. Las refs apuntan a archivos reales con SHA,
no a un Job/API de Core inventado. Validate relee el ledger COMPLETED, journal/
outbox, hashes, snapshots, muestras y fórmulas, incluso desde otro proceso.

Cancel/EOF no publican informe exitoso. Un crash conserva muestras y journal;
el nuevo dueño marca FAILED y libera recursos sólo tras adquirir el lock físico.
Un commit incierto mantiene recursos y bloquea otro run hasta reconciliar.
Causa original redactada fsync+rename fuera del commit fallido. Nueva medición
usa un ID y preflight propios y vuelve a ejecutar warmup y cinco llamadas; no
reutiliza tiempos ni afirma que un directorio no confirmado pasó el benchmark.

Los cuatro tests puros tienen goldens de fórmulas, warmup, n=5, duraciones positivas,
clamp y separación de identidad/trazabilidad. El runner nativo comprueba peticiones
y wire SHA desde un proxy independiente, estadística con stdlib `statistics`,
falsificación, corrupción de artefactos, validación tras restart, errores HTTP,
fallo real de rename, cancel/EOF/crash, worker competidor, backend muerto/reiniciado
y fallos SQL de admisión/muestra/outbox final: **34 pasos reales PASS**.
El test por host protegido recalcula
estadísticas y fingerprint con Rust y comprueba permisos, archivos y ledger.
[Recibo nativo de benchmark](P026_runs/benchmark-real.json).

Se añadió el caso de JSON completo con Content-Length mayor que los bytes enviados.
Benchmark y protocol rechazan ese framing incorrecto antes de aceptar la respuesta,
aunque la forma JSON y el echo sean correctos; cada runner usa un servidor real.

## Probes nativos sobre los cinco paquetes originales

El transporte de probes comparte con Worker el executable, argumentos, entorno
y ausencia de consola, y conserva P022/actor/hash/contexto antes de arrancar.
`protocol_probes_first` impide que un handshake automático abra estado antes de
probar el inicial. No se modifica el paquete ni se usa la concesión de un original
para lanzar otro binario mutante como si estuviera cualificado.

Pasaron **16 casos por familia, 80 en total**: handshake requerido, major
incompatible, nonce/métodos/capacidades, campos extra, parámetros nulos/extra,
JSON/UTF-8/NaN, ID inválido, método ajeno, cancel de ID desconocido, límite exacto
1 MiB/+1, exceso no JSON y RPC válido tras los errores. Se verifica que no cambien
archivos ni directorios del work antes del handshake compatible y que el EOF
termine con salida correcta y sin replies sobrantes. Los informes guardan
expected/observed, IDs, tamaños, hashes de frames/stdout y snapshots/cierre.

Son **nueve observaciones reales por familia, 45 en total**. El motor exige que
esta cobertura parcial siga rechazándose: no acredita cancelación de un trabajo,
progreso completo, crash, permisos, transiciones/recuperación ni familia/capacidades.
Los casos de mutantes siguen en su matriz independiente de **145 casos**, también
revalidada. [Informe de originales](P026_runs/native-wire-original.json).

Se cerraron además los límites del transporte raw: el proceso que ignora EOF se
termina y drena antes del siguiente arranque; el mismo contexto vuelve a completar
handshake/RPC y cierra cooperativamente. Un reply tardío invalida cobertura y el
cierre conserva el primer error. Este transporte se reutiliza en la receta nativa
runtime/echo, source, protocol y transformation descritas arriba. El siguiente
incremento corresponde a benchmark, sin ampliar infraestructura genérica salvo un defecto observado.

## Motor nativo y cierre de IO

El motor resuelve un plan interno por instalación y sólo acepta suites de código
compiladas con el host. No importa observaciones/PASS desde JSON de cliente o
plugin. Mide el ejecutable actual del host, descriptor de la suite y fixtures
inmutables, perfil completo, manifiesto/paquete, entorno congelado, raíces
resueltas, concesión y límites. Comprueba P022 y actor antes de crear work/lanzar;
repite mediciones antes de cada arranque y al terminar. Sólo evalúa Proof con
cobertura exacta después de cerrar proceso y canales.

La admisión es exclusiva por ID. Cancelar al observador no cancela el propietario
de IO. Shutdown cierra admisión y espera los scopes activos, incluso si falló el
handshake, hubo panic o queda un canal heredado abierto. Los lectores/wait retienen
el scope; un timeout de cleanup no se considera cierre ni produce Execution apta
para confirmar metadata. No sustituye leases/reservas de P019/P050 ni P130.

Worker incorpora una barrera explícita `stop_and_wait`, límites al esperar un
escritor bloqueado y terminación al soltar el último propietario. Cancelar start
termina el proceso aunque no se haya construido Worker. Las pruebas observan un
stderr heredado que termina después de la raíz, backpressure de 512 KiB y cierre
efectivo de procesos. Se conservan framing, errores y la política P022.

`NativeIo` registra el IO propio del oráculo (incluida la conexión al ledger).
La barrera final espera su cierre antes de construir Execution; un timeout/panic
también inicia cleanup con propietario propio. Si tarda más que la gracia, no se
confirma metadata y conserva admisión hasta terminar; `shutdown()` lo espera.
Reiniciar el worker conserva ese recurso. El nuevo test verifica cuatro caminos
con un handle de archivo exclusivo real: timeout, cleanup lento, reinicio y panic.

Los diecisiete tests nativos prueban determinismo/binding, permisos/actor/bytes, perfil
y suite incorrectos, inputs modificados, cobertura incompleta, error original,
timeout, observador cancelado, concurrencia, shutdown, panic, handshake fallido y
crash/reinicio. Ejecutan workers propios reales, pero sus observaciones completas
son sintéticas CODE_PASS para probar el motor; no son los oráculos de producto.
No se registran en el daemon. Los probes previos al handshake están implementados
y probados con originales; runtime/echo, source, protocol, transformation y benchmark
completan familia/capacidad. [Guía de ejecución](../adapters/conformance.md).

## Evidencia y fidelidad de los oráculos

El kernel de confirmación ya está implementado en Domain/Core/SQLite. Core recibe
un ID de instalación y contexto, valida el manifiesto con el SDK y espera al
puerto ejecutor interno. Snapshot y actor deben coincidir antes de confirmar.
No recibe un veredicto PASS del cliente. `Proof`/`Outcome` no son deserializables;
un test de compilación comprueba que JSON no puede construir una prueba.

El binding incluye la instantánea completa registrada, revisión exacta como
string i64, paquete/manifiesto/permisos, capacidades, actor y hashes de suite,
perfil y contexto. Exige cobertura exacta de 15 checks comunes, familia y cada
capacidad, con evidencia SHA; rechaza omisiones, extras, duplicados y fallos.
Comprobar hashes/invariantes no demuestra por sí solo que el ejecutor haya medido
los bytes ni ejecutado los oráculos: eso conserva su propietario de infraestructura.

`SqlAdapterConformance` confirma CAS, VALIDATED/INSTALLED, recibo en metadata,
journal y evento/outbox en una transacción. No cambia trust, manifiesto ni selección.
Un fallo de revalidación retira VALIDATED. Un retry idéntico devuelve el recibo
histórico sin reactivar nada tras una revocación; cambiar actor/veredicto/binding
se rechaza. La lectura verifica los hashes y la coherencia de fase/recursos/actor.
Los errores de auditoría sólo contienen una categoría segura; Core conserva y
devuelve el Error original, incluidos códigos de plugin desconocidos.

El recibo liga además el estado completo confirmado: revisión exacta, manifiesto,
permisos, capacidades, paquete, trust, metadata y timestamps. Sólo excluye de ese
hash el propio recibo. La transacción de promoción/habilitación/rollback exige que
coincida con la instalación actual y con un journal COMPLETED válido. Una fila
VALIDATED sin recibo o con evidencia anterior/alterada no es seleccionable.
Desactivar sigue permitido y una activa no elegible no sustituye al último bueno.
Un journal corrupto de la activa tampoco impide sustituirla por una sana.

Habilitar fuentes exige el mismo recibo vigente y las capacidades del binding
coincidentes con el manifiesto. Configurar una conexión deshabilitada sigue
permitido; registrar un binding cambia metadata/revisión y requiere validar después.

Pasan **6 tests de invariantes y un compile-fail**, **5 de orquestación Core** y
**14 de persistencia**: SQLite real con concurrencia CAS, reinicio, replay,
revocación, ocho mutaciones de snapshot, corrupción, máximo i64 y triggers de
fallo de instalación/journal/evento/outbox; el rollback conserva estado y permite
retry único. Las observaciones y los puertos son fixtures CODE_PASS. No son
cualificación de un worker. Las cinco recetas de referencia añaden
los recorridos reales por Core/SQLite descritos arriba. El caso público de fuentes
habilitadas y el runner formal también pasan en esta revisión. Los consumidores
de ejecución corresponden a sus fases posteriores. El motor conserva las pruebas
de framework descritas y el gate de recibo/journal/snapshot; el registro local,
la selección y las fuentes recorren el daemon real. [Diseño e invariantes](../adapters/conformance.md).

Se revalidaron además **9 tests de selección**, **11 de fuentes** y **18 del
coordinador P024**. Las pruebas rechazan 14 mutaciones del estado ya confirmado
incluso sin incrementar revisión, nueve alteraciones de recibo/journal y
VALIDATED sin evidencia. Comprueban habilitación/rollback, sustitución de una
activa corrupta, configuración deshabilitada y rechazo de capacidades de binding
ajenas. Los recibos anteriores sin hash del estado confirmado requieren
revalidación; no hay migración que conceda elegibilidad automáticamente.

El host comprueba ahora el tamaño durante la lectura de stdout, antes de
deserializar y sin esperar newline/EOF. Superar 1 MiB termina el árbol con
`stdout_frame_too_large`. Un fallo del lector usa `stdout_read_failed`. Stderr
drena sin retener líneas mayores de 16 KiB y mantiene el límite global de 10 MiB;
no publica fragmentos que pudieran eludir la redacción por partir un secreto.
Estos límites de buffers no acreditan una cota total de memoria del proceso.

Cada llamada pendiente conserva el método y el host vuelve a validar el schema
de su resultado antes de entregarlo. Un reply inválido se convierte en
PROTOCOL_ERROR/invalid_adapter_reply sin el payload privado. IDs ajenos o ya
completados, progreso sin llamada pendiente y peticiones del adaptador al host
usan el mismo contador de infracciones: una deja DEGRADED; tres, FAILED y termina
el árbol. La comprobación específica de nonce/major del handshake se conserva.

`tools/p026_wire_real.py` ejecuta una matriz sobre las cinco referencias reales,
en dos capas independientes. Primero envía NDJSON directamente al SDK: handshake,
campos extra, null, JSON/UTF8/no finitos, ID inválido, método no anunciado,
cancelación de ID inexistente, límite exacto de 1 MiB y exceso, resynchronización,
callback válido y EOF con paquete inmutable. Los inputs quedan guardados con SHA.

Después coloca un proxy de test en otro paquete con hash distinto, conservando
el worker real como hijo. `conformance_host` observa el host de producción:
paso normal, una línea corrupta, ID desconocido, respuesta duplicada, progreso
ajeno, petición inversa, tres JSON corruptos, frame excesivo sin LF, resultado
fuera de schema, canal que no atiende cancel, crash del hijo y stderr de 12 MiB
en una sola línea. Expected/observed, hashes de código/binarios/paquetes y los
PID propios quedan registrados. Se comprueba salida real de proxy e hijo por
handle de proceso; una fuga se limpia y deja la prueba FALLIDA, nunca PASS.

El caso de cancel timeout retiene el reply de una operación de familia real y
descarta cancel en el proxy: prueba la respuesta del host a un canal bloqueado,
sin atribuir falta de cooperación al handler real. Los runners funcionales siguen
probando cancelación/progreso/crash durante el trabajo de cada referencia. Los
mutantes no se promueven ni cualifican; la integración de todos los oráculos con
la validación del paquete original y la persistencia de Core sigue pendiente.

El corpus compartido contiene **8.544 casos**, **917 aceptados y 7.627 rechazados**
por el contrato original con su extensión decimal. Hay positivos en las 76
direcciones, mutaciones, límites exactos/overflow/ceros, y fixtures source/common
idénticas. Los testigos de schema no son respuestas funcionales ni cualificación.
Rust y Python pasan el mismo archivo mediante motores independientes.

Dos tests adicionales del servidor real sobre canales dúplex recorren las cinco
familias y las **35 firmas de familia**: contador de callbacks intacto ante null o
campo extra, ejecución sólo con request válida, reply válida, método desconocido,
heartbeat/cierre; y salida malformada de cada familia normalizada sin filtrar el
marcador privado. Son tests del servidor, no cinco workers funcionales Windows.

La activación detectó fixtures anteriores de transporte que usaban iterations/ms
como campos raíz de Benchmark o replies no canónicos. Se adaptaron a Benchmark,
SuiteDescription, BenchmarkOutput y ValidationResult reales. Los parámetros de
espera quedan en un suite_id explícito de fixture. Su salida es BLOCKED_REAL con
fingerprint DECLARED: nunca un benchmark de inferencia aprobado. Se conserva la
conducta de cancel/timeout/crash/env que comprueban los oráculos del host.

El negativo P023 que pedía install_plan con {} no alcanzaba una operación válida;
ahora pide detect_backends con InstallationRef correcto y observa el mismo rechazo
de operación no aplicable. Un DTO inválido recibe ahora invalid_params antes del
adaptador. El test de handshake del handler 2.0 observa invalid_adapter_reply y no
un reply deliberadamente inválido emitido por el propio SDK. No se rebajaron gates.

Antes del motor nativo, tools/p026_prepare_evidence.py ejecutó **37 grupos PASS**, con logs, exit, hashes de
código/contratos/corpus, lock, host y fecha:

- build de worker/echo/daemon y ejemplos;
- schemas, corpus y tres tests diferenciales;
- tres tests de Contracts Python, incluyendo corrupción/ausencia/refs/staleness;
- generación exacta de capacidades y fixtures, seis descriptores runtime/source
  fijados y **5 tests de registro Python**;
- **5 tests Python de negociación**, con procesos reales y trazas de llamadas:
  ausencia de registro/capacidad, estimador inválido/excepción, major/identidad,
  fallo de apertura y capacidades vacías; ningún callback ni apertura prematuros;
- SDK Rust **75 tests**, incluidos cinco de capacidades, cuatro de negociación,
  dos de corpus, dos de
  sesión de cinco familias y rechazo de referencias externas; SDK Python **9 tests**
  de transporte con procesos reales;
- dominio completo **103 tests** (96 existentes, seis de conformidad y un
  compile-fail de Proof no deserializable), **5 de orquestación Core** y **10 de
  commit SQLite** con fixtures CODE_PASS; no son cualificación de workers;
- **7 tests de límites del servidor** dentro de esos 75: 4+64 y rechazo de exceso,
  cancel/EOF sin ejecutar la cola, plazas reutilizadas con pico observado cuatro,
  ID conservado con stdout bloqueado, error real del lector, tarea interrumpida y
  rechazo de otra llamada posterior, además de NDJSON parcial conservado;
- host **34 tests**: **29 con procesos reales** (declarative 7, policy 5, source 6,
  worker 11), cuatro de framing y un inventario; límite exacto, exceso sin LF/EOF,
  drenaje/reencuadre de stderr y error real de la interfaz de lectura inyectado;
  se conserva rechazo de capacidades sin registro antes del sondeo;
- protocolo por proceso: **8 pasos** P020 con DTOs canónicos;
- **7 pasos de saturación de un proceso Rust real**: sólo cuatro IDs emiten
  progreso con 68 admitidos; heartbeat, rechazo de la 69, cancel de cola sin
  ejecutar, nueva plaza, EOF con respuesta de todas las llamadas, paquete
  conservado y cierre sin pipes retenidos. Es una fixture de transporte con
  capacidades funcionales vacías, no la referencia benchmark ni su cualificación.
  [Recibo nativo](P026_runs/server-real.json) con hash de binario/servidor/script;
- referencias por host protegido: **5 tests** de las cinco familias, incluida
  instalación source P024 real desde origen inicial explícito de fixture, sin
  promover; el protocolo sólo cualifica el transporte controlado;
- **36 pasos nativos de negociación**: major incompatible sin estado, negociación
  y callback válidos en las cinco familias, bundle ausente, estimador inexistente,
  hash de schema cambiado y referencia externa; además, para las tres familias
  con ledger, segundo propietario rechazado sin recuperación ni mutación y marcador
  ajeno conservado con liberación del intento fallido. Paquetes propios restaurados,
  hashes/identidades/capacidades en el [recibo](P026_runs/negotiation-real.json);
- **145 casos wire/host reales**, **29 por familia** (17 SDK y 12 host), con
  inputs, expected/observed, hashes y procesos observados. Una línea corrupta
  deja DEGRADED y entrega el reply válido; tres infracciones terminan el árbol.
  El primer reply válido se conserva en los cinco casos de respuesta duplicada,
  y los duplicados provocan FAILED. Los cinco excesos de stdout se rechazan sin
  LF/EOF; los cinco stderr de 12 MiB se drenan sin conservar la línea. El caso
  de progreso ajeno cuenta sólo IDs extraños y admite el progreso legítimo real.
  [Recibo completo](P026_runs/wire-real.json);
- **20 pasos de regresión declarativa**, incluido Ollama real en puerto/carpeta
  aislados, importación/exportación de documento sin confianza heredada y cierre
  del proceso sin alterar la instancia del usuario;
- runtime **26 pasos** y source **30 pasos reales**, con HTTP, archivos, SQLite,
  reanudación/cancel, fallos de outbox, muerte/recuperación, hashes y paquetes
  instalados/inmutables;
- **9 tests de transporte de source**, revalidados tras extraer las comprobaciones
  compartidas de rutas/origen/redirect al SDK; sin cambiar los permisos;
- protocol **24 pasos reales**, detallados arriba;
- transformation **28 pasos reales** y **7 tests de archivos**, con ledger de
  intentos/journal/outbox/reservas, recuperación y hash contrastado con Rust;
- benchmark **34 pasos reales**, **4 tests de estadísticas/fingerprint** y oráculos
  independientes de peticiones, wire SHA, fórmulas, persistencia y recuperación;
- 53 copias normativas, arquitectura, 47 dependencias y secretos.

La preparación formal actual vuelve a ejecutar estas regresiones junto con los
perfiles persistentes, gates físicos y escenarios HTTP. Los **51 grupos pasan**;
la auditoría adicional reconstruye los cinco recibos y verifica que las tres
pruebas daemon/TCP son frescas, con el mismo binario y hashes de sus trazas.
La preparación está ligada por SHA-256 a las auditorías; no cambia ninguna de
sus 284 fuentes durante la ejecución. El gate general -Real pasa 13/13, incluidos
los 39 tests UI y el shell Tauri. El lock y el paquete normativo siguen íntegros.
La evidencia histórica de P025 no se convierte en REAL_PASS por esta regresión.

Los escaneos históricos de 90.110 archivos, 337 NDJSON y 2.099 archivos/1.456
NDJSON quedan conservados. El escaneo completo actual cubre 210.398 archivos,
3.752.266.648 bytes y cero coincidencias de las dos credenciales autorizadas.
Los valores sólo se recuperan en memoria desde sus entradas exactas de Credential
Manager. La verificación final focal cubre además los informes escritos después
del escaneo y liga sus hashes a los recibos actuales.

El primer intento de esta revisión usó Python de sistema sin `jsonschema`: ese
harness no llegó a ejecutarse, aunque Rust pasó. Se conservan sus ocho logs en
[el recibo del intento](P026_runs/native-engine-attempts/20261018T143135Z-missing-python-dependency/checks.json).
El recibo actual usa `.venv-tools` y todos los grupos pasan. El oráculo de puerto
observa que el bind local sea posible: un timeout TCP filtrado por firewall no se
interpreta como prueba de puerto libre.

## Catálogo HTTP de fuentes y cierre formal del 2026-10-20

`tools/p026_source_api_real.py` ejecuta un daemon real contra su propio servidor
HTTP y un paquete source en su carpeta de prueba. Registra el binding antes de
conformidad, consume la aprobación P022 al guardar el perfil, ejecuta sus 21
observaciones nativas y sólo entonces habilita fuentes. El fixture inicial de
instalación es explícito; no sustituye importación P028 ni descargas P050.

Sus **25 checks PASS** comprueban auth/scopes, recibo previo, creación y edición
habilitadas, replay sin efectos, If-Match ausente 428/inválido 400/stale 409,
actor ajeno rechazado, input cambiado rechazado sin filas/eventos/outbox/replay,
configuración deshabilitada permitida y desactivación sin gate. Restaurar los
bytes permite reactivar. Un fallo real de outbox revierte toda la mutación;
el reintento confirma una sola vez. Tras reiniciar el daemon, el mismo perfil
persistente vuelve a autorizar el guard físico. Al terminar, sus tres fuentes
quedan deshabilitadas, sin mutar el paquete ni repetir transferencias del ledger.

La traza retenida contiene método/ruta/rol, hash de petición, key de idempotencia,
If-Match, status y respuesta; no contiene cabeceras Bearer ni los tokens. Una
reconstrucción independiente JCS contrasta Run/Outcome/recibo. Los procesos y
principales temporales son propios y se cierran/revocan al finalizar. El primer
intento focal esperaba incorrectamente 400 en lugar del 428 contractual para
If-Match ausente; se corrigió el oráculo y se conserva su recibo fallido.

El primer -Real formal pasó **49 grupos**, pero agotó los 600 s del escaneo
secuencial; la auditoría rechazó correctamente el recibo incompleto. Sus 54
archivos originales permanecen en el [índice del intento fallido](P026_runs/formal-attempts/20261020T122927Z-secret-scan-timeout/index.json).
El verificador ahora lee bloques binarios con solapamiento y ocho lectores
acotados, incluye NDJSON/CSV/SQLite/WAL y propaga fallos de acceso. Seis pruebas
comprueban fronteras UTF-8/UTF-16, patrones largos, lectura completa, concurrencia,
coincidencias, errores y alcance. El presupuesto de 1.800 s afecta sólo al escaneo
de verificación; no modifica cotas de RPC/cancelación/suite de producto.
La repetición completa terminó con **51 grupos + auditoría PASS**, sin alterar
fuentes. Ningún intento fallido se reetiqueta como aprobado.

## Continuación del desarrollo

P026 conserva **BLOCKED_REAL sólo por P025**, conforme al contrato 00. No quedan
pruebas propias pendientes en este incremento. El manifiesto continúa por
P030/P031/P027. Los consumidores de ejecución deben conservar estos guards;
la automatización de recursos auxiliares y perfiles generales corresponde al
desarrollo de producto. P028/P045/P050/P130 conservan sus responsabilidades.
Estas referencias no acreditan inferencia, conversión de modelos, upstreams,
reservas globales ni admisión de ejecución Core. El objetivo completo sigue activo.
