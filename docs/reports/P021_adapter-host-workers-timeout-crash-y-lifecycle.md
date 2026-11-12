# P021 — Adapter host, workers, timeout, crash y lifecycle

**Fecha**: 2026-10-12 · **Host**: A9_MEGA (PC IA) · **Estado**: **BLOCKED_REAL**. Todas las pruebas pasan, y las del host se ejecutan con procesos reales en Windows, pero su dependencia P020 no tiene REAL_PASS. [evidence.json](P021_adapter-host-workers-timeout-crash-y-lifecycle.evidence.json) · [operación](../adapters/host.md)

## Qué se construyó (`crates/larm-adapter-host/src/worker.rs`)

- **Estados del worker**: `STARTING`, `READY`, `DEGRADED`, `FAILED` y `STOPPED`, con su motivo. Son del proceso y nunca tocan el estado de la instalación.
- **Lanzamiento**:
  - ejecutable resuelto (canónico) **dentro** del paquete, con los argumentos estructurados del manifiesto y sin shell;
  - entorno vaciado más un mínimo explícito, la carpeta temporal propia y las variables del perfil, con los nombres validados;
  - sin ventana y `kill_on_drop`.
- **Handshake** obligatorio en 10 s: nonce, versión y métodos frente al manifiesto. Si falla: `FAILED` y se mata el proceso.
- **Llamadas**:
  - el método debe estar en el manifiesto;
  - cola de 64 y 4 llamadas en paralelo;
  - serialización por `resource_id`;
  - timeout por operación: `core.cancel`, gracia de 5 s y, solo como último recurso, kill del árbol.
- **Cancelación** por id (`call_request` + `cancel`).
- **stdout**: una línea que no es un frame deja el worker `DEGRADED`; tres, `FAILED`.
- **stderr**: se drena siempre. Cada línea se redacta y se publica como log etiquetado. Lo que pasa de 10 MiB se descarta y se cuenta, y se conservan las 50 primeras y las 150 últimas líneas.
- **Heartbeat** cada 10 s; 3 ausencias → `FAILED`.
- **Proceso que sale** solo: `FAILED` y `ADAPTER_CRASHED` para las llamadas en curso.
- **Windows Job Object**: se crea antes del spawn, se asigna al worker y se conserva junto al handle `Child`. `KILL_ON_JOB_CLOSE` termina sus descendientes; tras salir la raíz, se drenan hasta un segundo los canales heredados y se cierran los procesos restantes. `TreeKill` sigue como fallback.
- **Adaptador falso**: `examples/fake_adapter.rs`, con 9 modos de fallo, y su fixture en `tests/fixtures/fake_adapter/`.

## Defectos encontrados por las pruebas

1. **Logs de arranque perdidos**: los logs solo se publicaban a los suscriptores, así que lo escrito antes de suscribirse se perdía. Se añadió el búfer de primeras y últimas líneas.
2. **Pruebas contra un ejecutable viejo**: `cargo test --test worker` no recompila los ejemplos, y una prueba se ejecutó contra un adaptador falso desactualizado. La evidencia compila el ejemplo antes de las pruebas.

## Decisiones

- **Job Object**: el wrapper seguro `win32job` permite activar `KILL_ON_JOB_CLOSE` sin añadir `unsafe` al workspace. La asignación ocurre inmediatamente después del spawn; los descendientes creados después heredan el Job. La prueba Windows mata solo el worker raíz y confirma que el nieto termina al cerrar el Job. Esto contiene el ciclo de vida, pero no confina disco ni red; los límites de recursos y AppContainer siguen en P130. Un worker que lance un proceso antes de que el host lo asigne todavía puede escapar de este Job.
- **Umbral de stdout corrupto**: 3 líneas. Es el mismo umbral de 3 que los heartbeats; el contrato no fija otro.

## Pruebas (procesos reales en el PC IA)

| Prueba del prompt | Prueba |
|---|---|
| Handshake success/fail | `handshake_correcto_y_llamadas` y `handshake_fallido` (sin respuesta, otro nonce, ejecutable ausente, variable inyectable) |
| Timeout | `timeout_con_cancelacion_atendida_y_sin_atender` |
| Crash | `crash_del_proceso` |
| Malformed stdout | `stdout_corrupto` |
| Huge stderr | `stderr_enorme_no_bloquea_y_se_limita` (12 MiB; secreto redactado) |
| Cancellation | `cancelacion_pedida_por_el_host` |

Además:

- `heartbeat_ausente`;
- `entorno_minimo_explicito`: ni `PATH`, ni `USERPROFILE`, ni `APPDATA`;
- `cola_acotada`: cola llena y mutaciones en serie;
- `matar_es_el_ultimo_recurso_y_mata_el_arbol`: el worker y su nieto desaparecen;
- `job_object_al_cerrar_worker_termina_descendiente_aunque_muera_el_padre`: el Job Object mata al nieto aunque el fallback de la prueba solo termine la raíz.

**Mutaciones**: cada una se aplicó y se restauró el código. Las 10 hacen fallar o bloquear las pruebas:

1. heredar el entorno;
2. no comprobar el handshake;
3. matar sin cancelar antes;
4. tolerar stdout corrupto;
5. stderr sin límite;
6. sin redactar;
7. ignorar el heartbeat;
8. no matar el árbol: la suite se queda colgada, porque el nieto retiene las tuberías;
9. sin serializar;
10. cola sin límite.

Dos mutaciones iniciales estaban mal escritas (una era la identidad y otra no compilaba); se rehicieron.

## Pendiente

- **Límites de memoria/procesos y AppContainer**: P130.
- **Integración del host con el daemon y el registro de instalaciones de adaptadores**: los prompts siguientes de la familia de adaptadores.
- **Reinicio con nueva generación y recuperación de journal/locks tras un crash**: lo consumirán runtime y descargas. El worker ya expone su generación y el motivo del fallo.

## Incremento 2026-10-22

El Job Object kill-on-close ahora contiene los descendientes del worker en Windows, con una espera acotada de un segundo para drenar canales heredados. La suite `worker` pasó 16/16 y Clippy pasó para todos los objetivos del crate; la sesión cualificada de runtime pasó su prueba de ciclo RPC. La suite completa de `larm-adapter-host` no se pudo aceptar: tres pruebas declarativas de health quedaron en `LOADING` porque el entorno deniega conexiones loopback incluso al fixture iniciado manualmente. P021 continúa `BLOCKED_REAL` por P020 y no se afirma aislamiento del sistema operativo.
