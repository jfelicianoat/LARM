# Referencia de transformación binaria

Paquete Python autónomo fuera de Core con las cuatro firmas de la familia:
`transformation.describe`, `plan`, `execute` y `verify`. Convierte un archivo propio
de hasta 64 MiB de `BINARY` a `BASE64` mediante streaming RFC 4648 sin saltos.
Es una conversión real de bytes para probar el contrato; las herramientas de
conversión/cuantización de modelos siguen en sus fases propietarias.

`package.py:create` recibe bytes de una fixture del caller y crea un paquete y work
nuevos. Fija Python/dependencias/licencias/SDK/contratos, manifiesto y descriptor de
`vendor.reference.base64` con schemas, mapping, estimador y fixture. El input tiene
SHA/tamaño fijados en un perfil con IDs privados; no son variantes ni ubicaciones
registradas en Core. El estimador acota buffers, conservando memoria total UNKNOWN.

El input y el perfil tienen sólo permiso de lectura. Los únicos write roots son
`work/state` y `work/output`. No hay permisos de red, secretos, procesos ni scripts
de parámetros; `parameters` es `{}`, tool `1.0.0`, output `BASE64`. Se bloquean
componentes reparse/junctions, rutas fuera de roots y sobrescrituras. La referencia
está acotada a Windows y publicación en el mismo volumen. El host mantiene timeout,
cancel/gracia/terminación; las comprobaciones de rutas no acreditan AppContainer.

El plan dura 900 s, fija input/output hashes, perfil, parámetros y presupuesto,
y no escribe output. Execute comprueba hash, caducidad e input nuevamente antes de
admitir. El tamaño Base64 es exacto; el margen de control es conservador (3 MiB),
DB de hasta 1 MiB mediante page cap real, sidecars de hasta 16 KiB y rollback journal.
Se exige espacio físico observado más margen max(1 GiB, ceil 5 %), antes de fases y
bloques. `eligible=false`: no se observan reservas/políticas globales de Core. Los
temporales retenidos ya ocupan disco; no se descuentan otra vez como reserva viva.

El ledger privado SQLite usa transacciones FULL con job, intento, journal, evento
`transformation.state_changed`, outbox, lock y reserva. El lock físico de un byte
evita dos workers sobre el mismo work. Antes de confirmar un checkpoint se hace
fsync del bloque. La publicación es rename de un directorio completo verificado,
sin reemplazar uno anterior: payload, procedencia y ArtifactManifest. El hash del
bundle coincide con `BundleEntry` de Rust: tamaños numéricos en la representación
canónica, aunque el DTO de bytes use strings. Verify vuelve a leer los archivos.

`PLANNED→QUEUED→RUNNING→VERIFYING→COMPLETED`; pausa no soportada.
Cancel pasa por CANCELLING/CANCELLED y limpia sólo los tres archivos enumerados de
su intento. Fallos conservan causa redactada, entrada, plan, hashes, temporales,
journal, locks y reserva para reconciliar. Tras interrupción, el nuevo dueño físico
marca FAILED_RECOVERABLE. El reinicio exige nuevo preflight/input íntegro/plan,
incrementa attempt y genera otra salida desde cero. No reanuda offsets. Los intentos
y eventos anteriores permanecen; los terminales COMPLETED/CANCELLED no se reviven.

Un commit/outbox incierto no devuelve recibo exitoso ni libera recursos. Una causa
redactada se conserva en un sidecar con fsync+rename si no puede guardarse en la transacción;
si falla registrar un error, se conserva la causa original antes que la secundaria.
Un prefijo `.tmp` incompleto se conserva sin tratarlo como metadata confirmada.
Una publicación completa anterior puede quedar retenida sin cualificación hasta
reiniciar. Nunca se presenta un output parcial como artefacto verificado.

Execute devuelve COMPLETED sólo después del trabajo real. `status_url` es file://
de un JobState privado existente, sin inventar un job/API/reserva de Core. La
procedencia conserva source/output SHA, herramienta/versión, parámetros, fecha,
actor, intento y códigos de logs seguros. No concede disponibilidad de un modelo.

Desde la raíz del repositorio:

```powershell
.venv-tools/Scripts/python.exe tools/test_reference_transformation.py
.venv-tools/Scripts/python.exe tools/p026_transformation_real.py
cargo test -p larm-daemon --test reference_adapters
```

El runner usa un worker real y 12 MiB+5 bytes propios para observar cancelación,
checkpoint/crash/reinicio, corrupción externa controlada y fallos SQL de aceptación,
finalización y registro de errores. El test del host protegido compara manifiesto
y bytes con el dominio/decodificador Rust y niega escritura al original y red.
Los recibos quedan en `docs/reports/P026_runs`; la cualificación conjunta P026,
la promoción P024 y el pipeline de modelos de producto siguen pendientes.
