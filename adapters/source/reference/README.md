# Fuente de referencia — P025

Worker Python autónomo con las nueve firmas source de P043. Busca y pagina un
catálogo JSON local; devuelve inspección, revisiones, manifiesto, tamaños y sync
con ETag; sondea autenticación pública; sella planes y descarga fixtures HTTP.
Cada modelo es BINARY y anuncia `vendor.reference.*`, sin inventar inferencia.

`package.py:create(repo, package, work, base_url, artifacts)` crea carpetas nuevas.
Copia Python 3.14.6 y las dependencias ya fijadas, licencias y dependencies.lock.json;
no instala software. bin/python.exe usa -I -B -m main y un ._pth cerrado. El paquete
contiene SDK, schemas, código y política. SQLite, catálogo y estado están en work.

work/profile.json contiene sólo source_id y storage_location_id UUIDv7. Puede
configurarse antes del primer arranque sin cambiar permisos ni hash del paquete.
El ledger fija su digest y rechaza cambiar de perfil sobre un work utilizado.
Otro perfil requiere otra carpeta. Catálogo: work/catalog/catalog.json. Salida:
work/storage/<manifest_hash>/…. Los símbolos reference-package/work/catalog deben
resolverse a esas raíces en el host, autoridad de permisos y ejecución P022.

Los planes caducan a los 900 s y ligan snapshot, artefactos, revisión/hash,
destino, auth/licencia, presupuesto, prioridad y reanudación. Un plan exitoso
devuelve su recibo original; uno fallido conserva su error. Un nuevo plan admite
un nuevo intento del mismo job fallido. Un lock físico exclusivo protege el work.

El ledger **privado del worker** guarda job, journal, eventos/outbox, reserva
local y checkpoints después del fsync. Al reiniciar registra ADAPTER_CRASHED
para trabajos interrumpidos antes de liberar sus recursos. Un fallo incierto
del commit final conserva recursos hasta recuperar. El JobReceipt COMPLETED
apunta por file:///… a un JobState canónico realmente legible del worker.
P050 conserva la admisión, jobs y reservas globales del Core; sus consumidores
no deben abrir ciegamente una URL arbitraria suministrada por un worker.

transfer.py exige tamaño y SHA256 conocidos, ETag fuerte para reanudar y hasta
64 MiB por modelo de prueba. Comprueba raíces, componentes, junctions y origen
exacto. No sigue redirecciones ni usa proxies o tokens heredados. Nunca sobrescribe
un destino distinto. Cancelar elimina sólo los partials enumerados del job. El
429 conserva Retry-After observado sin reintentos automáticos. Auth distinta de
NONE y licencias pendientes se rechazan; P045 sigue siendo su propietario.

StorageEstimate descuenta sólo archivos finales verificados por tamaño y hash;
utiliza GUID/espacio observados en Windows y el margen publicado. eligible=false
conserva explícitamente la falta de admisión P050 y reservas globales.

```powershell
cargo build -p larm-adapter-sdk -p larm-daemon --bins
.venv-tools/Scripts/python.exe tools/p025_source_real.py
cargo test -p larm-daemon --test reference_adapters
```

El script ejecuta conformance común y 30 oráculos de catálogo/HTTP/SQLite: corte
de conexión, cancelación, triggers que abortan transacciones, muerte del proceso
propio y recuperación. El ZIP autónomo también se descubre por P024. La prueba
Rust instala una actualización P024 real desde un origen INSTALLED explícito de
fixture, descubre la candidata y usa su worker por el host protegido, con concesión
TRUSTED_ONLY ligada al hash. No acredita importación inicial P028, promoción P026
ni aislamiento AppContainer. Ver docs/adapters/authoring.md e informe P025.
