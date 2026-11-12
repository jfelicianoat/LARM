# P100 — Servicio durable de Runtime Lease

Actualización focal 2026-10-29: el puente rechaza aliases remotos de Ollama en catálogo/admisión/dispatch/keep_alive, desactiva redirects y revalida la lease ACTIVE después del IO de metadatos ante una liberación concurrente. Verificación final121/121 runtime-lib (diez regresiones), controles estáticos focales y nativa1/1 de sólo metadatos contra Ollama0.40.11 PASS. No acredita el recorrido durable/Broker completo ni cambia el bloqueo de este informe. [Pruebas y alcance actuales P070](P070_ollama-runtime-host-adapter.md).

Fecha: 2026-10-24  
Estado: `BLOCKED_REAL`

Se añadió la base de persistencia transaccional de leases para preparar la sustitución del estado en memoria de `ExternalAdoptionBridge`. El repositorio SQLite usa las tablas contractuales existentes `runtime_lease`, `idempotency`, `event` y `event_outbox`; esta fase no añade migraciones ni cambia las rutas públicas.

## Cambios

- [RuntimeLeaseRepository](../../crates/larm-domain/src/ports.rs) define operaciones durables de creación, lectura/listado por propietario, transición condicional, renovación idempotente, vencimiento en dos fases y expiración de generaciones no reconciliadas.
- [SqlRuntimeLeases](../../crates/larm-db/src/runtime_leases.rs) registra `REQUESTED`, la respuesta idempotente y el job en una transacción; la renovación ACTIVE usa CAS e idempotencia y escribe `runtime_lease.renewed` con outbox; `expire_due` persiste `ACTIVE → EXPIRING → EXPIRED` con la gracia; la liberación reutiliza la transición CAS `ACTIVE → RELEASED`. Antes de persistir `ACTIVE`, SQLite comprueba que la instancia está `READY` o `BUSY` en la generación solicitada, que el endpoint está `QUALIFIED` y pertenece a esa instancia, y que el vencimiento es futuro.
- `ExternalAdoptionBridge` recibe el repositorio desde el composition root del daemon. `GET /leases/{id}` recupera desde SQLite las leases ausentes en memoria; `GET /leases` combina las filas del propietario con el estado actual en memoria, conserva la precedencia del proceso y recorre páginas de 200 sin truncar la lista.
- El daemon conecta el puente con SQLite para admitir solicitudes durables: una transacción escribe la lease `REQUESTED`, la respuesta idempotente y el job `QUEUED`; después persiste `REQUESTED → ADMITTING` y los cambios de fase del job. La respuesta 202 solo se devuelve tras guardar la admisión.
- La activación durable ya está enlazada con P033/P036: el worker pide referencias cualificadas al daemon, que valida el host, runtime, instalación adoptada, backend, variante y perfil locales. La sonda HTTP de P036 confirma salud y `/v1/models`; sólo entonces SQLite inserta `RuntimeInstance` `READY`, `InferenceEndpoint` `QUALIFIED` y sus eventos/outbox en una transacción. La lease pasa a `ACTIVE` con esos mismos IDs y generación.
- El puente usa el repositorio para renovar y liberar leases durables, y su sweeper aplica el vencimiento SQLite y revoca la credencial al final de la gracia. La API rechaza liberar leases en `REQUESTED`, `ADMITTING` o `EXPIRING`; sólo permite `ACTIVE → RELEASED`, en línea con el ciclo contractual.
- La admisión durable falla cerrada si falta el resolvedor productivo, no coincide el catálogo local o la sonda no cualifica el endpoint. En esos casos el job registra `FAILED` y la fila pasa de `ADMITTING` a `FAILED`; no se devuelve una respuesta `ACTIVE` respaldada sólo por IDs de memoria.
- [lease_recovery.rs](../../crates/larm-runtime/src/lease_recovery.rs) conecta la invalidación de inicio a SQLite y Credential Manager. El daemon ejecuta esa reconciliación antes de montar las rutas; si no puede revocar un secreto antiguo, falla cerrado y no empieza a aceptar tráfico. El mismo identificador de generación se pasa al puente Broker y al repositorio de lecturas.
- Se cubren conflictos de idempotencia, restricciones de propietario/revisión/generación, referencias ausentes, instancia no lista, generación obsoleta, endpoint ausente/no cualificado/asociado a otra instancia, vencimiento expirado, activación válida, recuperación conservadora tras reinicio, claves de idempotencia expiradas y rollback del evento/outbox.
- Una prueba de integración crea 201 leases en SQLite, cambia la generación como haría un reinicio y verifica `GET`, `LIST` paginado, aislamiento por propietario y precedencia de un registro vigente en memoria.

## Validación

La evidencia vigente registra **15 PASS, 0 FAIL y 1 REAL BLOCKED**. Pasan los checks de los 80 schemas de métodos y 36 schemas de capacidades, las siete pruebas unitarias de `runtime_leases`, las pruebas focales de admisión/recovery/renovación, el resolvedor SQL con sonda HTTP local, todos los targets de `larm-db`, formato, Clippy del workspace y las 67 copias contractuales 3.9.1. La puerta `tools/check.ps1 -Prompt P100 -Real -SkipFrontend -SkipDesktop` terminó **7/7 PASS**; `cargo test --workspace -j 1 -- --test-threads=1` completó **1.188 pruebas, 0 fallos**. El recibo JSON se valida contra `PromptEvidence`; la puerta no ejecutó los tests de escritorio ni frontend porque esta fase no los modifica.

El estado sigue `BLOCKED_REAL`. Renovación, vencimiento, liberación y activación tienen operaciones SQLite conectadas al puente; la nueva integración local activa la lease sólo con referencias persistidas y cualificadas. Falta ejecutar el ciclo con el daemon Windows y un runtime externo instalado, y alcanzar REAL_PASS en P033, P035, P036, P042, P019 y P090; P101 sigue condicionado por esas dependencias.

La aceptación real debe crear, renovar, vencer, recuperar tras reinicio y liberar leases por el daemon Windows con un runtime externo instalado, y comprobar que SQL, reservas, credenciales y outbox quedan consistentes. Las pruebas de esta revisión usan SQLite temporal y un runtime HTTP simulado en loopback. No se iniciaron AI Broker, Agora, Ollama ni el daemon productivo, y no se usaron credenciales externas.

No se usaron ni guardaron tokens de AI Broker o Agora. El puente de adopción todavía no utiliza estas credenciales para crear o mutar leases durables.

Evidencia: [PromptEvidence P100](P100_runtime-lease-service.evidence.json) · [gate P100](check_P100_real.json). Estado global: [STATE.md](../implementation/STATE.md).
