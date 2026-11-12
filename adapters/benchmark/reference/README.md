# Referencia de benchmark HTTP

Paquete Python autónomo con las tres firmas `benchmark.describe/run/validate`.
Mide roundtrip HTTP, decodificación y validación de una fixture contra el ejecutable
Rust `larm-reference-echo`. La capacidad `vendor.reference.echo` tiene schemas de
entrada/salida, mapping, estimador y fixture fijada, compartidos con protocol.
El codec HTTP es independiente. Es un benchmark real de ese transporte controlado;
los benchmarks de inferencia/modelos y la evaluación funcional conservan P110–P112.

Cada run hace **un calentamiento y cinco repeticiones**, sin caché de respuestas.
Registra inicio/fin en monotonic ns, resultado y causas por muestra. El calentamiento
no entra en los agregados: mediana, p95 nearest-rank, media, desviación muestral e
IC95% mean ± 2.776·sd/√5, límite inferior ≥0. Menos de cinco duraciones positivas no
genera un agregado aprobado. Los errores se conservan en las muestras, no se eliminan
para mejorar estadísticas. Un fallo funcional produce FAIL aunque otras llamadas
hayan funcionado.

Settings fijados: seed42, temperature0, top_p1, batch1, benchmark timeout1800s y
socket timeout2s. Echo no aplica parámetros de generación; se declara esa limitación.
TTFT/tokens/generación/carga y RAM/VRAM del backend quedan null con razones, sin
estimar tokens a partir del texto ni rebautizar latencia como TTFT. El fingerprint
mantiene desconocidos modelo, binario, backend, hardware y drivers no observados.
`inference_qualification_eligible=false` y el scope del informe evitan presentar el
PASS de esta suite como aprobación de inferencia, comparación o promoción.

El hash RFC8785 del fingerprint usa name/value/status; evidence_refs y timestamps
son trazabilidad separada. El snapshot de suite/fixture/settings/perfil se fija al
ejecutar; no se lee un catálogo posterior para reconstruirlo. Los IDs del perfil
son bindings privados de fixture, sin variantes/runtimes/leases admitidos en Core.

El work separa profile (read-only), state y output. El paquete es inmutable;
único origen HTTP loopback literal, sin proxies/redirects/credenciales/procesos.
Request16KiB, reply32KiB, JSON estricto y Content-Length comprobado. Permisos de
rutas por componentes y rechazo de reparse points; el host mantiene timeout/gracia/
kill. La prueba TRUSTED_ONLY no acredita aislamiento AppContainer P130.

SQLite privado FULL mantiene medición, seis muestras/observaciones, journal, eventos,
outbox, lock y reserva. Ownership física antes de reconciliar. DB hasta 1 MiB,
documentos hasta 128 KiB y reserva conservadora 3 MiB más margen 1 GiB; máximo32
mediciones por work de referencia para conservar historial sin borrado automático.
No sustituye admisión/políticas/reservas globales de Core. Un nuevo run toma un ID
nuevo; no reanuda una medición interrumpida ni hereda sus tiempos.

Las observaciones reales se publican con fsync y rename de un directorio completo
sin sobrescribir, junto al informe. `artifact_refs` apunta a un archivo existente
y su SHA se conserva en metrics. Validate comprueba la medición COMPLETED del ledger,
hash del informe, journal/outbox, snapshots, muestras, artefactos y fórmulas. Relee
bytes de forma acotada; también funciona tras reiniciar el worker. Un informe
externo inventado, modificado, incompleto o con evidencia alterada no valida.

Cancel/EOF no retornan un informe aprobado. Tras crash se conserva evidencia y se
marca FAILED sólo cuando el nuevo proceso toma el lock físico; entonces libera
recursos de la medición terminada. Un commit/outbox incierto conserva estado y
recursos, bloquea otro run hasta recuperación y no convierte un directorio completo
en medición confirmada. La causa redactada usa un registro fsync+rename separado
del commit fallido. No hay JobReceipt ni URL de API de Core ficticios.

Desde la raíz del repositorio:

```powershell
cargo build -p larm-adapter-sdk -p larm-daemon --bins
.venv-tools/Scripts/python.exe tools/test_reference_benchmark.py
.venv-tools/Scripts/python.exe tools/p026_benchmark_real.py
cargo test -p larm-daemon --test reference_adapters
```

El runner observa seis peticiones reales por run mediante un proxy independiente,
estadísticas con `statistics` de stdlib, wire SHA, persistencia/validación/restart,
HTTP/auth/rate/redirect/JSON/tamaño/tipo/framing, fallo real de rename, falsificación,
cancel/EOF/crash, backend detenido/reiniciado y fault injection de admisión/muestra/
outbox final. El test del host verifica fórmulas y hash con Rust. Recibos en
`docs/reports/P026_runs/benchmark-real.json`. La cualificación conjunta de P026 y
promoción de adaptadores P024 siguen pendientes.
