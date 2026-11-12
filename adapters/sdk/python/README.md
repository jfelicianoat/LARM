# SDK Python — P025

Servidor Adapter Protocol 1.0 sin dependencia del Core. Requiere Python 3.14
(UUIDv7) y las versiones fijadas de jsonschema/referencing en tools/requirements.txt.
Se ejecuta en un entorno propio; no instala paquetes en Python global.

`Contracts(Path(".../docs"))` carga los dos schemas normativos, la tabla de
métodos, frames P020 y 76 schemas/índice de docs/adapters/method-schemas. P026
comprueba hashes/bindings al arrancar; falta/corrupción no tienen fallback. Las
38 firmas validan entradas/salidas con x-maximum-decimal (i64::MAX donde lo fija
el contrato). Sólo referencias locales, sin resolución de red/archivos externos.
Esas entradas se deben incluir en el
paquete de desarrollo del adaptador; no se buscan ni descargan en red. Una
referencia no disponible produce error, nunca validación permisiva.

`ProtocolServer(contracts, manifest, handler).serve(sys.stdin.buffer,
sys.stdout.buffer)` valida manifiesto, familia y métodos obligatorios, handshake,
requests y respuestas. `handler.call(method, params, ctx)` devuelve el DTO exacto
o lanza `DomainError`; `ctx.check_cancelled()` y `ctx.progress(...)` permiten
cancelación y progreso. `handler.shutdown()` libera recursos propios al cerrar
stdin. Máximo cuatro callbacks y 64 en cola; core.cancel/heartbeat no esperan esa
cola. Un ID activo duplicado no reemplaza la llamada original.

stdout contiene únicamente frames. El SDK no imprime excepciones ni datos del
callback. Cada handler sigue siendo responsable de sus permisos de archivos,
red y procesos. El host conserva timeout, gracia, kill e aislamiento del SO.
No implica conformidad P026 ni soporte de fuentes o runtimes de terceros.

Cuando el manifiesto anuncie capacidades, `handler.registry` debe ser un
`CapabilityRegistry` con schemas, mapping implementado y estimador registrado
para cada una. El servidor valida identidad/major y ejecuta los estimadores sobre
las fixtures antes de aceptar la sesión. Capacidades vacías permiten un launcher
sin soporte de inferencia. La existencia del registro no es cualificación.

`handler.open_session()` es opcional y se invoca una sola vez, después de pasar
esas comprobaciones. Abrir ahí los locks, bases privadas y recuperación evita
crear estado con una negociación rechazada. Si falla, no se admiten callbacks;
`shutdown()` debe tolerar una sesión que nunca llegó a abrirse.

La fuente Python de `adapters/source/reference` implementa las nueve firmas sobre
un catálogo JSON y HTTP de prueba, con ledger SQLite privado. Su paquete incluye
intérprete/dependencias fijados y se ejecuta con -I/-B. Ambas referencias pasan
el runner común y pruebas mediante el host protegido. La guía paso a paso está
en `docs/adapters/authoring.md`; el informe P025 conserva los límites de evidencia
y las dependencias sin REAL_PASS.
