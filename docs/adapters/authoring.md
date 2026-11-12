# Crear un adaptador sin modificar Core

Las cinco referencias son paquetes externos al Core: runtime en Rust;
source/protocol/transformation/benchmark en Python. El proceso usa stdin/stdout
con Adapter Protocol 1.0.
El host P021/P022 valida paquete, confianza y accesos; Core no contiene condiciones
por sus IDs.

## 1. Elegir familia y contrato

Leer docs/contracts/03_ADAPTERS_SECURITY.md, adapter-methods.json y los schemas
canónicos. Usar exactamente request_schema y response_schema de cada método.
mandatory_common y los métodos requeridos de la familia deben figurar en el
manifiesto. La tabla contiene runtime, source, protocol, transformation y benchmark.
Los DTO cerrados rechazan extras; metadata abierta conserva desconocidos/null.

Las fixtures compartidas están en tests/fixtures/adapters y
tests/fixtures/source/protocol.json. source-v1.md explica negociación de búsqueda,
filtros diferidos y planes. Anunciar sólo capacidades implementadas y probadas.
Una operación ausente devuelve UNSUPPORTED_CAPABILITY, nunca éxito simulado.

La [guía de capacidades](capabilities.md) describe el registro compartido de P026:
schemas/hashes, mapping, estimador y fixture antes de anunciar. Hay implementación
en ambos SDK y uso real en las cinco referencias; la cualificación completa sigue
pendiente. Un descriptor válido
no acredita soporte de inferencia.

## 2. Implementar el handler

Para Rust, depender de larm-adapter-sdk sin importar DB, API ni daemon. Usar sus
tipos de protocolo, Server y handlers. La referencia runtime reutiliza el worker
genérico P023 con un documento declarativo y un proceso echo HTTP real; ver
adapters/runtime/reference. Para un handler Rust completo consultar también
crates/larm-adapter-sdk/src/source/handler.rs.

Para Python, incluir adapters/sdk/python/larm_adapter_sdk y los contratos locales:
los dos schemas canónicos, adapter-methods.json y los tres frames de
docs/adapters/schemas y los 80 schemas más index.json de docs/adapters/method-schemas.
P026 también genera el índice Rust, embebido en el binario. Python exige hashes y
bindings exactos, sin fallback. Ambos SDK interpretan x-maximum-decimal además de
restricciones estándar antes de ejecutar callbacks. Fijar versiones. El handler:

```python
class Handler:
    def __init__(self, registry):
        self.registry = registry

    def call(self, method, params, ctx):
        ctx.check_cancelled()
        # Despachar a IO real y devolver el DTO exacto del método.
        # ctx.progress(phase, completed_bytes, total_bytes, message_code)
        return dispatch(method, params, ctx)

    def shutdown(self):
        close_owned_resources()

contracts = Contracts(package / "docs")
manifest = contracts.manifest(read_manifest(package))
ProtocolServer(contracts, manifest, Handler(registry)).serve(
    sys.stdin.buffer, sys.stdout.buffer
)
```

registry es el registro de capacidades construido según la guía enlazada.
dispatch, read_manifest y close_owned_resources son responsabilidades del adaptador,
no funciones del SDK. Bytes: strings decimales; IDs: UUIDv7; fechas: UTC con ms.
El SDK valida entradas/salidas. Usar DomainError para errores esperados. No imprimir
params, secretos, rutas o excepciones internas. stdout contiene sólo NDJSON UTF-8
de hasta 1 MiB; stderr, diagnósticos acotados y redactados. No descargar schemas.

Comprobar cancelación entre bloques de IO, durante esperas y antes de publicar.
Emitir progreso real con el contexto. Heartbeat/cancel deben responder aunque haya
callbacks activos. EOF cancela trabajo cooperativo y ejecuta shutdown. El host
posee timeout, gracia y kill del árbol; un timeout no demuestra trabajo terminado.

El servidor Rust admite como máximo cuatro callbacks y 64 peticiones en espera.
Handshake/heartbeat/cancel no consumen esas plazas. Cola llena produce
RESOURCE_UNAVAILABLE retryable con reason adapter_queue_full, sin sustituir el ID
ni la cancelación de una petición ya admitida. Cancelar una petición en espera la
retira sin ejecutar el handler. El ID y la plaza se mantienen hasta escribir la
respuesta, también si stdout tarda; no reutilizar IDs de solicitudes pendientes.

EOF y errores de lectura cancelan y esperan las tareas propias. Un callback
interrumpido libera su admisión y hace fallar la sesión; no se ejecuta otra llamada
tras observarlo. El lector conserva un NDJSON recibido a medias al terminar otro
callback. El adaptador debe cooperar con cancelación: el host mantiene su gracia
y terminación del proceso si no lo hace. Las cotas de callbacks no sustituyen el
aislamiento y límites del proceso de P130. Oráculos: server_limits.rs y
tools/p026_server_real.py (proceso Rust real, fixture sólo de transporte).

## 3. Definir permisos y estado

El manifiesto declara raíces simbólicas de lectura/escritura, orígenes exactos,
ejecutables permitidos y referencias de secretos. El host resuelve símbolos a
rutas conocidas: metadata no concede acceso a una ruta arbitraria. El worker
comprueba sus accesos y no hereda shell, proxy, credenciales o confianza. Validar
componentes y rechazar reparse points antes de IO. El ejecutable Python anunciado
es el intérprete relativo fijado con argumentos estructurados, sin shell.

Separar paquete inmutable y work mutable. La fuente de referencia configura sólo
source/storage UUID en work/profile.json antes del primer arranque y liga su ledger
a ese perfil. Otro perfil exige otra carpeta; permisos/política están en el paquete.

Persistir plan/hash, revisión, auth/licencia, archivos, destino, prioridad y
presupuesto. No devolver QUEUED sin trabajo durable. La referencia completa una
fixture y devuelve JobReceipt con URL file de un JobState propio existente. El
schema compartido no exige la ruta de jobs de Core: quien la devuelva debe haber
admitido ese job en Core. P050 conserva admisión, reservas globales y descarga de
producto; los eventos/outbox y reservas privados de la referencia no lo sustituyen.
No abrir ciegamente las URLs de estado de workers externos.

Hacer fsync de partials antes del checkpoint. Verificar tamaño/SHA antes de publicar
sin sobrescribir. Ante commit incierto conservar journal/recursos para reconciliar.
Recuperar tras tomar propiedad física del work; no liberar por tiempo. Reanudar
únicamente con identidad fijada y revalidada. Reintentar requiere preflight/plan
nuevo. Cancelar retira sólo temporales enumerados del trabajo propio.

## 4. Construir el paquete

Crear una carpeta nueva con manifest.json, ejecutable relativo y todos sus archivos.
registry.md describe el formato y regla P022: package_sha256 cubre rutas POSIX en
minúsculas y hashes SHA256 ordenados, excluyendo manifest.json. El hash del ZIP es
distinto y se usa en AdapterImport. No incluir tokens ni confianza en metadata libre.

adapters/runtime/reference/package.py:create copia los binarios Rust compilados y
genera el documento declarativo. adapters/source/reference/package.py:create recibe
repo, paquete nuevo, work nuevo, origen HTTP y artefactos propios; copia Python y
dependencias ya fijados, licencias, SDK, contratos y fuente. Genera catálogo/perfil
fuera del paquete. No instala software global ni descarga runtimes. ._pth explícito
y -I/-B evitan importar entorno del usuario o escribir bytecode dentro del paquete.
El paquete Python de referencia está acotado a Windows x86_64 y fixtures de 64 MiB.

Los scripts reales invocan las factories y registran carpetas/hashes en
docs/reports/P025_runs y P026_runs. Desde la raíz del repositorio:

```powershell
cargo build -p larm-adapter-sdk -p larm-daemon --bins
.venv-tools/Scripts/python.exe tools/p025_runtime_real.py
.venv-tools/Scripts/python.exe tools/p025_source_real.py
.venv-tools/Scripts/python.exe tools/p026_protocol_real.py
.venv-tools/Scripts/python.exe tools/p026_transformation_real.py
.venv-tools/Scripts/python.exe tools/p026_benchmark_real.py
cargo test -p larm-daemon --test reference_adapters
```

Inspeccionar registro/paquete sin instalar ni ejecutar contenido:

```powershell
target/debug/larm-daemon.exe inspect-adapter-registry --folder C:\Pruebas\registro
target/debug/larm-daemon.exe inspect-adapter-package --file C:\Pruebas\registro\source.zip --expected-hash <SHA256_ZIP>
```

P024 instala actualizaciones side-by-side y deja candidatas INSTALLED/UNTRUSTED.
El test source P025 parte de una instalación inicial explícita de fixture y ejecuta
la actualización real, profiler/SQLite/reservas/publicación, antes de usar el paquete
resultante por el host. No simula VALIDATED ni promoción. Importación inicial por
API/exportación conserva P028; no inventar una ruta de import. Para runtime
declarativo ya existe POST /api/v1/adapters/declarative.

## 5. Ejecutar conformance y probar efectos reales

Ambos SDK rechazan capacidades anunciadas sin registro explícito. Registrar los
schemas, mapping y función de estimación; no deducir soporte por nombre de
capacidad ni perfil. En Python, reservar locks/estado mutable en `open_session()`;
el servidor sólo lo llama tras una negociación compatible y contratos válidos.
La inicialización puede leer los documentos necesarios para validarlos, pero no
debe iniciar un backend, trabajo, recuperación ni publicar artefactos. Ver
[contratos de capacidades](capabilities.md).

tools/adapter-conformance/runner.py acepta cualquier paquete con métodos comunes y
una operación válida cancelable. El JSON de casos tiene cuatro claves: valid_method,
valid_params, cancel_method y cancel_params. Los valores son requests reales según
la tabla. La operación cancelable debe emitir progreso y permitir recibir cancel.
Source puede preparar antes un download_plan persistido y abrir otra sesión para
el runner, que necesita comprobar la ausencia inicial de handshake.

```powershell
.venv-tools/Scripts/python.exe tools/adapter-conformance/runner.py --package C:\Pruebas\paquete --workdir C:\Pruebas\work --cases C:\Pruebas\cases.json --out C:\Pruebas\conformance.json
```

Ocho oráculos comunes: handshake obligatorio, nonce/métodos/capabilities, método
no anunciado, params inválidos, respuesta válida, progreso/heartbeat, cancelación y
cierre limpio. Este ejecutor lanza paquetes controlados; no es un sandbox. Para
paquetes no confiables usar primero el host y sus gates de permisos.

Añadir oráculos independientes de bytes, puerto/proceso, DB y efectos de la familia.
Probar schema inválido, autorización insuficiente, estado inválido, timeout/cancel,
IO/proceso muerto y recuperación; inyectar fallos de outbox/commit. No limitarse a
repetir el algoritmo. Las cinco referencias pasan por GuardedWorker
con WhoAmI real y
concesión TRUSTED_ONLY temporal ligada a hash/publicador/permisos. P130 conserva
AppContainer; esta prueba no acredita ese aislamiento.

`tools/p026_wire_real.py` ejecuta la matriz de mensajes/errores sobre las cinco
referencias reales, seguida de fallos del canal mediante proxies de prueba. Guarda
inputs, hashes, expected/observed, bytes de binarios y PID propios; comprueba
limite exacto/exceso, IDs, duplicados, progreso ajeno, timeout/cancel no atendido,
crash y stderr con líneas de gran tamaño. El proxy es una mutación con hash
distinto: esa evidencia no cualifica el paquete original ni habilita promoción.

## 6. Registrar evidencia

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File tools/check.ps1 -Prompt P025 -Real
```

El runner guarda logs, hashes, inputs, fecha, host y resultados mediante PromptEvidence.
P026 posee cualificación de las cinco familias y promoción. Echo/descarga de fixture
no acreditan inferencia, upstream remoto, secretos P045 ni descargas de producto P050.
Conservar BLOCKED_REAL mientras una dependencia no tenga REAL_PASS.

La [confirmación de conformidad](conformance.md) describe el binding interno,
Core y el commit CAS/journal/outbox. Su ejecutor nativo y conexión con la promoción
siguen en curso; un recibo proporcionado por el cliente no concede VALIDATED.
