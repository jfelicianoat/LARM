# Referencia de protocolo HTTP

Worker Python autónomo fuera de Core para Adapter Protocol 1.0. Implementa
`protocol.describe`, `protocol.qualify`, `protocol.invoke` y `protocol.cancel`
contra el ejecutable Rust `larm-reference-echo`. No implementa inferencia.

`package.py:create` crea un paquete nuevo: intérprete y dependencias fijados,
licencias, SDK, contratos locales, manifiesto, política HTTP y descriptor de
`vendor.reference.echo`. El directorio de trabajo contiene un perfil separado con
endpoint/variant/lease UUIDv7 de la fixture. Esas identidades ligan el worker a su
perfil privado; no son admisión ni leases otorgadas por Core.

El descriptor declara schemas cerrados de entrada/salida, hashes RFC 8785, mapping,
estimador y fixture fijada. El SDK los comprueba antes del handshake. El estimador
acota buffers; devuelve memoria total desconocida, sin afirmar que limite la
memoria del proceso. Véase la [guía de capacidades](../../../docs/adapters/capabilities.md).

La política permite un único origen HTTP de loopback literal y `/health`, `/echo`.
No se heredan proxies ni credenciales y se rechazan redirects. Requests de hasta
16 KiB, replies de hasta 32 KiB, timeout de IO de dos segundos, JSON UTF-8 sin
claves duplicadas ni números no finitos. Un fallo nunca incluye el cuerpo privado
del servidor. `Retry-After` numérico observado se conserva como detalle del error;
la referencia no reintenta automáticamente una invocación.

`qualify` ejecuta health y la fixture HTTP y compara la salida esperada. Sólo
cualifica ese transporte controlado; el fingerprint mantiene desconocida la
identidad del backend remoto. No promueve un AdapterRef, no certifica un modelo y
no transforma un testigo de schema en evidencia de inferencia. El paquete no tiene
permiso de escritura, procesos hijos ni secretos. Echo no produce jobs durables.

`protocol.cancel` usa el ID semántico de la invocación; `core.cancel`, el UUID RPC.
El registro activo rechaza duplicados concurrentes y se limpia al terminar,
cancelar o fallar. El servidor sigue atendiendo heartbeat/cancel durante el IO.
El host conserva timeout, gracia y terminación si el callback no coopera.

Desde la raíz del repositorio:

```powershell
cargo build -p larm-adapter-sdk -p larm-daemon --bins
.venv-tools/Scripts/python.exe tools/p026_protocol_real.py
cargo test -p larm-daemon --test reference_adapters
```

El primer runner posee backend Rust, proxy de fallos y worker en Windows; registra
peticiones reales, hashes, cancelación, timeout, errores HTTP, crash/recuperación y
paquete intacto en `docs/reports/P026_runs/protocol-real.json`. El test Rust cruza
el host protegido con concesión temporal ligada al hash del paquete y permisos
concretos. P130 conserva el aislamiento AppContainer.
