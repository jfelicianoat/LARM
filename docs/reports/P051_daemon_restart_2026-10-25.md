# P051 — descarga válida recuperada después de terminar el daemon

Estado de P051: `BLOCKED_REAL`. Esta revalidación focal pasó en Windows, host `A9_MEGA`, el 2026-10-25. No sustituye las aceptaciones pendientes de la fase ni su puerta global histórica.

## Comportamiento comprobado

La prueba usa un servidor HTTP local y archivos propios, el paquete Python de referencia, dos procesos reales del daemon y una base SQLite temporal. El paquete se cualifica mediante `POST /adapters/{id}/probe` del primer daemon con P022/P026. La cualificación se realiza desde ese ejecutable porque sus huellas incluyen el binario, los límites, los permisos y las entradas; un recibo emitido por el binario de tests no autoriza al daemon.

El catálogo de la fixture se prepara en SQLite. El preflight se genera con `DownloadPlanPreview` y un sondeo nativo del disco; el repositorio acepta el plan sellado con confirmación explícita, idempotencia, reserva, journal y outbox. Esta preparación no prueba las rutas HTTP de preflight/activación. El consumidor que realiza la transferencia sí es el del daemon, con `SourceWorker` cualificado y comprobaciones físicas antes y después de cada RPC.

Tras observar bytes durables se termina el primer daemon. Se añade una cola al temporal sin publicar un checkpoint para ella. El segundo daemon recupera el job y termina la transferencia:

| Observación | Resultado |
| --- | --- |
| Checkpoint tras la caída | 65.536 bytes, fuente/revisión y ETag conservados |
| Primera lectura HTTP tras reiniciar | `Range: bytes=65536-`, `If-Range` igual al ETag original |
| Estado y número de intento | `COMPLETED`, intento 2 |
| Archivo final | 2.097.152 bytes, idéntico al archivo propio esperado |
| SHA-256 | `5647f05ec18958947d32874eeb788fa396a05d0bab7c1b71f112ceb7e9b31eee` |
| Procedencia | fuente y revisión originales persistidas |
| Journal | `COMPLETED` |
| Reservas al completar | 0 |
| Evento de recuperación | 1 |
| Eventos del job sin pareja outbox | 0 |
| Temporal parcial | eliminado después de la promoción a CAS |
| Principal de prueba | credencial efímera revocada; token sólo en memoria |

El recibo y las peticiones HTTP están en [result.json](P051_runs/daemon-restart/01a1753b-790c-72e2-95c8-e127bb661a6f/result.json) y [resumed-requests.json](P051_runs/daemon-restart/01a1753b-790c-72e2-95c8-e127bb661a6f/resumed-requests.json). Los logs de ambos procesos se conservan junto a ellos. El recibo incluye el hash del daemon y la cualificación emitida por su API.

## Corrección encontrada durante la prueba

El adaptador de referencia descontaba su caché privada del tamaño publicado por `source.download_plan`. Después de la cualificación ya tenía el archivo, por lo que devolvía cero bytes y el SDK rechazaba el plan: `SourceDownloadSpec` declara el contenido completo que Core puede solicitar por bloques. Se corrigió el productor para que ese plan no descuente la caché privada. `source.estimate_size` conserva la observación de esa caché; el preflight y los checkpoints de Core siguen siendo propios.

La prueba corta descarga primero a la caché del adaptador, comprueba la estimación de cero bytes pendientes y solicita otro plan a través del host protegido. El plan declara el tamaño completo y pasa la validación tipada del SDK.

## Verificación

- `cargo test -p larm-daemon --test reference_adapters source_python_por_host_protegido_y_plan_tipado -j1 -- --exact --test-threads=1`: 1/1 PASS.
- `cargo test -q -p larm-daemon --test reference_adapters source_suite_nativa_completa_descarga_recupera_y_confirma_core_sqlite -j1 -- --exact --test-threads=1 --nocapture`: 1/1 PASS, 315,07 s. Incluye la receta source y el reinicio descrito arriba.
- `cargo test -p larm-daemon --test lifecycle -j1 -- --test-threads=1`: 4/4 PASS después de compartir el helper de procesos.
- `cargo clippy -p larm-daemon --all-targets -j1 -- -D warnings`: PASS.
- `cargo fmt --all -- --check`: PASS.
- Sintaxis de `tools/p051_evidence.py`: PASS; el runner incorpora `P051-DAEMON-VALID-TRANSFER-RESTART` para ejecuciones posteriores.

Los intentos previos detectaron el contexto distinto de cualificación, el workspace privado ya ocupado y el plan inválido por caché; después de corregir la preparación y el productor, un intento alcanzó 1,6 MiB reanudados pero excedió los 60 s de la prueba. El timeout se amplió a 180 s conservando las comprobaciones físicas de integridad; la ejecución final completó todas las aserciones.

## Límites abiertos

No se repitieron `cargo test --workspace`, el runner formal completo P051 ni `tools/check.ps1 -Prompt P051 -Real` en este incremento. El recibo formal previo de 29 PASS/0 FAIL/2 BLOCKED sigue siendo histórico; esta revalidación aporta la evidencia focal enlazada.

Faltan la cualificación de un upstream real, autenticación/licencias, los casos integrados de disco lleno, ETag/hash alterados, pausa/cancelación y medición de dos transferencias H08, además de `REAL_PASS` de P015/P043/P019/P029. Por ello P051 y el objetivo completo permanecen abiertos.
