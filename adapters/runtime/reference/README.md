# Runtime de referencia — P025

El SDK Rust gestiona un proceso HTTP echo real. Es un ejemplo controlado y no
realiza inferencia: anuncia `vendor.reference.echo`, formato BINARY y perfil
reference.echo. No declara OpenAI, capacidad de texto IA ni compatibilidad de un
runtime de terceros. El archivo modelo es una fixture propia, sin pesos externos.

`echo.rs` compila como `larm-reference-echo`. Sirve sólo en 127.0.0.1:
GET /health, POST /echo (JSON real) y POST /shutdown. Requests y bodies están
acotados, con timeouts y conexiones cerradas. `--version` imprime una sola línea.

`main.rs` compila como `larm-reference-runtime`: compone `GenericRuntime` con el
registro echo explícito, mapping y estimador de buffers (memoria total UNKNOWN).
`package.py:create(repo, root)` copia ese wrapper a `bin/larm-generic-adapter.exe`,
nombre requerido por el schema, y añade el descriptor, echo, declarative.json,
fixture y manifest con hash real. Reutiliza P023 sin excepciones en Core.
No instala software global ni hereda
confianza. El worker presenta todos los métodos requeridos y devuelve
UNSUPPORTED_CAPABILITY para instalar, cargar un modelo IA, inspeccionar pesos o
estimar memoria, operaciones que este ejemplo no implementa. No simula su éxito.

Una solicitud normal permite consultar versión, planificar/arrancar, observar
health y parar. Al cerrar stdin se detienen sus instancias. Las pruebas de P025
verifican el eco HTTP independientemente y que el proceso/puerto terminan,
incluida la cancelación del sondeo de versión y el rechazo de 2 MiB de salida.
La consola propia del worker se inventaría antes/después del sondeo para no
confundirla con un hijo residual. La confianza, promoción de una
instalación y conformidad oficial conservan los gates P022/P026.

Reproducción desde la raíz: `cargo build -p larm-adapter-sdk -p larm-daemon --bins`
y `.venv-tools/Scripts/python.exe tools/p025_runtime_real.py`. La prueba también
registra el documento por el API existente y descubre la instalación UNTRUSTED.
El importador materializa su worker genérico propio: la capacidad declarada sigue
siendo candidata y su handshake se rechaza sin el registro del wrapper. El runner
también importa un launcher con capacidades vacías y ejecuta ese paquete real,
comprobando hash y cierre de proceso. El principal temporal se revoca.
No utiliza una excepción por id dentro de Core ni cambia su código.
