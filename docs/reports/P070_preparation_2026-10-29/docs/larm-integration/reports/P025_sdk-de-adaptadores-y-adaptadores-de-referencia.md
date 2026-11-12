# P025 — SDK de adaptadores y adaptadores de referencia

**Estado calculado: BLOCKED_REAL, sólo por dependencias P020/P021/P024/P043.**
Las cinco funciones del checklist están implementadas y sus pruebas ejecutadas:
SDK Rust/Python, runtime HTTP real, fuente JSON/HTTP con estado durable propio,
runner común aplicado a ambas referencias y guía paso a paso. **14 grupos PASS,
0 FAIL/omisiones; quality gate completo 13/13 PASS** el 2026-10-17 en Windows.
El incremento no recibe REAL_PASS mientras sus dependencias no lo tengan.

[Evidencia canónica](P025_sdk-de-adaptadores-y-adaptadores-de-referencia.evidence.json) ·
[Quality gate](check_P025_real.json) ·
[Runtime real](P025_runs/runtime-real.json) ·
[Source real](P025_runs/source-real.json) ·
[Guía de autoría](../adapters/authoring.md)

## Cambios y compatibilidad

El SDK Python carga ambos schemas, la tabla de métodos y los frames de P020 desde
el paquete, sin red. Valida familia/métodos requeridos, handshake, requests y
replies. NDJSON UTF-8 tiene límite de 1 MiB antes de parsear y drenaje acotado;
rechaza JSON/UTF-8 corrupto, claves duplicadas, NaN e IDs inválidos y recupera la
sesión. Un mayor incompatible válido estructuralmente produce PROTOCOL_ERROR.
No imprime params, entorno, rutas ni excepciones del callback.

Cuatro callbacks y cola 64; heartbeat/cancel no esperan esa cola. Reservar un ID
activo antes de encolar evita que un duplicado reemplace su respuesta/cancelación.
Cancelar en cola impide ejecutar el handler. EOF cancela, espera cooperación y
ejecuta shutdown. Timeout, kill y permisos conservan el host como propietario.

Rust recibe los mismos límites de lectura, cancelación en EOF y protección de IDs.
El sondeo de versión atiende cancel/timeout, termina su árbol y lee como máximo
1 MiB+1 por stdout/stderr antes de rechazar. El decoder de producción exige id,
exactamente result/error, códigos RPC publicados, Error válido y nullable required.
Conserva result:null. Las fixtures usan ese decoder, sin un filtro en el test que
cambie su veredicto. La prueba de progreso antigua usa ahora UUIDv7 válido.

Se retiró una restricción adicional de SourceReply::Download que exigía siempre
la ruta de jobs de Core. El schema canónico JobReceipt exige un status_url no vacío
y se comparte con workers: una URL de estado propio también es válida. La nueva
prueba acepta estado file/HTTPS de worker y rechaza vacío; el test protegido lee el
JobState y los bytes reales del recibo. No se amplió schema ni se inventó un job
del API. No hubo cambios de API pública, dominio, DB administrativa ni interfaz.

## Runtime Rust externo

adapters/runtime/reference empaqueta el worker genérico P023, echo.exe, fixture
propia y documento declarativo con hashes reales. Anuncia vendor.reference.echo,
BINARY y reference.echo; no realiza inferencia ni declara OpenAI/backends/memoria
verificados. El servidor real sirve GET /health, POST /echo y POST /shutdown sólo
en loopback, con tamaños/timeouts acotados. Métodos no implementados devuelven
UNSUPPORTED_CAPABILITY; no se simula instalación de un runtime de terceros.

**25 pasos nativos PASS**, incluidos siete oráculos comunes: handshake obligatorio,
nonce/capabilities, método no anunciado, params inválidos, schema de respuesta,
progreso/heartbeat y cancelación. Los demás contrastan versión/SHA y hash de plan
independientemente; modelo ausente, límite no soportado, plan alterado, un solo uso,
generación, echo HTTP y READY, parada/puerto, muerte real del hijo propio y arranque
posterior, EOF con instancia viva, paquete/input inmutables y 2 MiB de salida de
versión rechazados. El inventario de hijos vuelve a su baseline: conhost.exe ya
existía antes del sondeo y no se confunde con un proceso residual.

El API existente registra/lista la referencia INSTALLED/UNTRUSTED y materializa un
paquete cuyo worker ejecuta echo. Se contrasta su hash y el cierre de la instancia.
El principal temporal se revoca; sólo se detiene el daemon propio de la prueba.
No hay casos especiales por adapter_id en Core ni promoción automática.

## Fuente Python autónoma

adapters/source/reference implementa las nueve firmas P043 sobre un catálogo JSON
local y fixtures binarias propias por HTTP. Paginación ligada a filtro y hash del
catálogo; OR de capacidades/AND de texto; metadata desconocida/null preservada con
secretos de fixture redactados, sin eliminar tokenizer. Get_model, revisiones,
manifiesto, auth_probe, sync/ETag y tamaños devuelven DTOs canónicos. GUID/espacio
se observan en Windows. Se descuentan archivos existentes sólo con tamaño/SHA
verificados; eligible=false conserva la falta de admisión global P050.

El paquete copia Python 3.14.6 y las dependencias ya fijadas, con licencias y lock,
sin instalar globalmente. -I/-B y ._pth explícito aíslan imports del entorno y
evitan escribir bytecode en el paquete. Se incluyen SDK, contratos y código.
Catálogo, perfil, staging, SQLite y status quedan en work. Source/storage UUIDv7
pueden configurarse antes del primer arranque sin cambiar el hash; el ledger fija
el digest del perfil y rechaza reutilizarlo con otra identidad.

Plan con TTL 900 s y hash que liga revisión/snapshot, manifest, archivos,
destino, auth/licencia, prioridad y presupuesto. Un plan completado repite el
recibo original; uno fallido conserva el error. Reintentar exige nuevo plan y
preflight y aumenta el intento del mismo job fallido. El lock físico exclusivo
del work y la serialización de mutaciones evitan dos dueños de sus bytes.

Ledger SQLite **privado del worker**, application_id propio: planes/jobs,
journal, eventos/outbox, reservas/locks y checkpoints atómicos. fsync precede a
persistir offset. Completar/fallar y liberar recursos comparte transacción. Un
commit final fallido conserva VERIFYING/journal/recursos. Otro proceso con el lock
registra ADAPTER_CRASHED para trabajos interrumpidos antes de liberar; nunca usa
un timeout como prueba de IO terminado. Conserva partial verificable para nuevo
intento. El recibo COMPLETED apunta a un JobState canónico file:/// realmente
existente, sin inventar una ruta de job del Core.

El transporte exige tamaño/SHA conocidos y ETag fuerte para Range/If-Range;
rechaza cambios de identidad, checksum, paths/junctions y orígenes no autorizados.
No hereda proxies/tokens ni sigue redirects. Publica sin sobrescribir otro archivo.
Cancelar elimina sólo partials enumerados del job. Retry-After 900 permanece 900;
no hay reintento automático. La referencia está limitada a Windows y fixtures de
64 MiB por modelo; su presupuesto/reserva privada no sustituye P050.

**30 pasos source nativos PASS**: paquete autónomo; seis páginas y metadata;
filtros/schema; catálogo cambiado invalida cursor/plan; tamaños/revisiones/sync;
401 real y presupuesto sin admisión; plan caducado; hash/tamper; descarga/RPC y
JobState reales; eventos/outbox/recursos; replay/reinicio; estimación rehashea bytes
corruptos del mismo tamaño; interrupción/checkpoint; reanudación nuevo intento con
Range; cancel/limpieza; trigger de outbox en aceptación revierte todo; trigger al
completar conserva recursos; recuperación reutiliza publicación verificada; muerte
real del proceso propio y recuperación/reanudación; perfil ligado; conformance
común, ZIP descubierto por Core y paquete inmutable sin Authorization heredada.

## Host, instalación y runner

Dos tests del composition root daemon usan GuardedWorker, WhoAmI real y concesión
TRUSTED_ONLY temporal ligada a package hash/publicador/permisos, sin secretos.
Antes se observa el rechazo sin concesión. Source también prueba origen/secret
no concedidos antes del RPC; descarga tipada, SHA independiente y SQLite privado
con outbox/recursos liberados. Runtime arranca, observa READY y para el echo real.

La prueba source crea un origen INSTALLED explícito de fixture (importación inicial
P028 no acreditada) y ejecuta update-adapter real a 1.0.1: profiler, reserva global
P024, extracción/verificación, publicación y registro. Descubre dos versiones por
los puertos existentes, confirma candidata INSTALLED/UNTRUSTED sin selección ni
recursos vivos y arranca el worker de la carpeta efectivamente instalada. No
modifica código de Core para descubrir/usar el adaptador ni fabrica VALIDATED.

tools/adapter-conformance/runner.py aplica ocho oráculos comunes y permite casos
JSON y workdir propios. Session valida frames/DTO/nonce y cierre/stdout; no guarda
texto stderr. Ambas referencias usan el mismo runner; source incluye su cierre
limpio explícito, runtime comprueba EOF con instancias vivas. El ejecutor directo
es para paquetes controlados, no un sandbox ni cualificación oficial P026.
authoring.md documenta familia/SDK, permisos, paquetes, trabajo durable, pruebas
de negocio, conformance, instalación y evidencia sin tocar Core.

## Verificación registrada

tools/p025_evidence.py ejecuta **14 grupos PASS**: build real; SDK Python **9**;
SDK Rust **54** (27 lib, 1 con 24 ejemplos, 7 sesión, 7 frontera source y 12 source
protocol); host source **6**; referencias protegidas **2**; runtime **25 pasos**;
source **30 pasos**; transporte **9**; schemas **18 válidos/102 negativos**;
paquetes P024 **9**; OpenAPI, arquitectura, lock y secretos. Los logs incluyen
comandos, expected/observed, duración, exit y hashes; las evidencias incluyen
inputs normativos, host/fecha y hash de environment.lock.json.

tools/check.ps1 -Prompt P025 -Real: **13/13 PASS**, fmt/clippy/tests del workspace
y shell, arquitectura/copias del paquete, lint/typecheck/**39 tests UI**/build y
runner real. Todos los PromptEvidence existentes validan contra el schema.
Ambos tokens siguen recuperables desde Credential Manager y ausentes de los
**4.384 archivos** inspeccionados en la última ejecución completa. Nunca se imprimen sus valores. El recibo parcial
preparation.checks.json queda histórico; su antiguo comando delega al runner completo.

## Límites y continuación

Bloqueo formal sólo por dependencias P020/P021/P024/P043 sin REAL_PASS. Continuar
P026 según execution_order: conformidad completa para cinco familias y promoción.
No se acreditan aquí importación inicial P028, AppContainer/Job Object P130,
cuentas P045, admisión/velocidad agregada/cola de descargas P050 ni upstreams externos.
La interfaz no cambió. El SDK/las referencias no cualifican runtimes IA reales ni
el uso funcional de Agora, propietario P118. El objetivo de la app sigue activo.
