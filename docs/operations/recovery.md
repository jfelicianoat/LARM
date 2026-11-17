# Guía de recuperación del daemon LARM (P132)

## Orden de arranque contractual (§103)

El daemon registra cada paso en el orden obligatorio. Un salto o un paso
desconocido aborta el arranque con `startup_order_violation`:

1. `database_migrations` — migraciones SQLite idempotentes y atómicas.
2. `storage_reconcile` — reconciliación de volúmenes y ubicaciones.
3. `adapter_registry` — registro firmado de adaptadores y revocaciones.
4. `runtime_installations` — jobs runtime.install pendientes re-despachados.
5. `instances` — reconciliación de instancias runtime vivas.
6. `downloads_transforms` — descargas reanudables y transformaciones
   interrumpidas a FAILED_RECOVERABLE.
7. `event_service` — despacho de eventos desde el outbox.

## Protección de crash loop (runtime/adapter, contrato 04)

La protección de crash loop es para los procesos runtime/adapter, no para el
arranque del propio daemon: el supervisor (`larm-runtime/src/supervisor.rs`,
`CrashRestartTracker`) permite **3 reinicios por 10 minutos** con backoff
**[1, 5, 15] segundos** y tras el cuarto fallo marca la instancia `FAILED`.
Los reinicios controlados (operador o tests) no cuentan: sólo las salidas no
esperadas del proceso.

## Modo offline

Si el inventario local no se puede servir, el daemon arranca degradado; si la
red a fuentes remotas no está disponible, el inventario y los runtimes locales
siguen servibles y las búsquedas remotas se rechazan con
`remote_search_disabled_offline`. El modo es observado (`decide_mode`), no
configurado a mano.

## Procesos huérfanos

Tras un reinicio del host, ningún proceso de la sesión anterior sigue siendo
propiedad de LARM (el boot epoch del host cambia). Sólo se considera huérfano
eliminable un proceso cuya identidad completa coincida con el registro de
spawns: pid + SHA-256 del ejecutable + boot epoch + clase de job. Un pid
reciclado, un ejecutable distinto o un boot distinto se clasifican `FOREIGN` y
no se tocan (sin kill por ambigüedad).

## Transformaciones interrumpidas (H07)

Una transformación interrumpida y no reanudable se persiste como
`FAILED_RECOVERABLE` con la causa y el journal del intento anterior. El
reinicio exige input íntegro, nuevo preflight, guard/reserva y attempt
incrementado; `COMPLETED`/`CANCELLED` permanecen terminales. La GUI muestra la
acción Reiniciar sólo para FAILED_RECOVERABLE.

## Descargas

Las descargas seguras se reanudan desde su último byte duradero; las que
cambiaron de fuente o tamaño se reinician con el preflight completo.
