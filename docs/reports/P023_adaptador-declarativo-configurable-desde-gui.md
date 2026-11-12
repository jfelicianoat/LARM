# P023 — Adaptador declarativo configurable desde GUI

**Fecha**: 2026-10-15 · **Host**: A9_MEGA (PC IA) · **Estado**: **BLOCKED_REAL** — Pasan 18 pruebas (10 reales, entre ellas una de extremo a extremo con el daemon y un runtime real, Ollama) y quedan 3 bloqueadas. Además, las dependencias P020, P021 y P022 no tienen REAL_PASS. [evidence.json](P023_adaptador-declarativo-configurable-desde-gui.evidence.json) · [operación](../adapters/declarative.md)

## Qué se construyó

**Documento y validación** (`crates/larm-adapter-sdk/src/declarative.rs`)

- `DeclarativeAdapter` con la forma exacta del schema (`deny_unknown_fields`, y las claves anulables también son obligatorias).
- **Manifiesto fijo de un declarativo**:
  - familia `runtime`;
  - ejecutable `bin/larm-generic-adapter.exe` sin argumentos;
  - sin estrategias de instalación y sin firma;
  - raíces absolutas;
  - `capabilities` igual al manifiesto.
- **Ejecutables**: rutas absolutas de un `.exe` real dentro de `read_roots`. `.bat` y `.cmd` se rechazan porque irían por `cmd.exe`. Los dos ejecutables del documento son toda la allowlist de procesos.
- **Placeholders tipados**: `MODEL_PATH`, `CONFIG_PATH`, `PORT`, `THREADS` y `CONTEXT`. `{{`/`}}` son llaves literales. Cualquier otro nombre o una llave suelta se rechaza. Los placeholders que admite cada campo están en el schema del asistente. No hay shell ni reexpansión.
- **Health y parada**: solo loopback http, sin usuario, query ni fragmento. Parada `SIGNAL` o `HTTP`; `RPC` es `UNSUPPORTED_CAPABILITY` (P103).
- **Entorno (H04)**:
  - `EnvironmentConfig` del dominio;
  - referencias declaradas en `secret_refs`;
  - un nombre que indica un secreto solo admite referencia;
  - con perfil, el binding del perfil sustituye al del mismo nombre y cada variable efectiva tiene que estar en las dos allowlists.
- **Simulación sin ejecutar**: `simulate`/`prepare_start`. `adapter_options` sigue el `launch_options_schema` del declarativo (`model_path`, `config_path`, `port`). `runtime_arguments` tiene que ir vacío. Un límite de memoria es `UNSUPPORTED_CAPABILITY`.
- **Paquete**: `materialize` crea `<data_root>/adapters/<id>/<version>/` (worker, `declarative.json`, `manifest.json` con el `package_sha256` real según la regla de P022). Se prepara en una carpeta temporal y se renombra; si algo falla, no queda nada.
- **Schema publicado del asistente**: [`adapters/runtime/generic/wizard.schema.json`](../../adapters/runtime/generic/wizard.schema.json). Una prueba lo mantiene coherente con el código.

**Worker genérico** (`crates/larm-adapter-sdk/src/generic.rs`, binario `larm-generic-adapter` en `adapters/runtime/generic/main.rs`)

- `runtime.list_versions`: sondeo de 10 s como mucho; canal `custom` (H01); `artifact_hash` = sha256 del ejecutable.
- `runtime.launch_plan`: valida antes de IO; plan de un solo uso durante 900 s, ligado a su hash.
- `runtime.start`, `runtime.health` y `runtime.stop`: estados `READY`, `LOADING`, `DEGRADED` y `FAILED`. La parada HTTP tiene 5 s de gracia; después se termina el árbol.
- El resto de métodos obligatorios devuelve `UNSUPPORTED_CAPABILITY` `declarative_method_not_applicable`, nunca un éxito vacío.
- **Entorno de los procesos**: se vacía; solo lleva el mínimo de Windows y las variables efectivas.

**Persistencia y API**

- Dominio: `AdapterInstallation` y el puerto `AdapterInstallationRepository`.
- `larm-db`: `SqlAdapterInstallations`; el alta escribe `adapter.updated` en la misma transacción.
- `larm-core::adapters::DeclarativeRegistry`, por orden:
  1. valida;
  2. comprueba las rutas reales (`path_outside_roots`, `path_missing`);
  3. comprueba que la versión no exista (`adapter_version_exists`);
  4. crea el paquete;
  5. registra la instalación como `UNTRUSTED`/`INSTALLED`, con `metadata` `DECLARATIVE`/`CUSTOM`/`TRUSTED_ONLY`, `document_hash` y `package_path`. Si el registro falla, se borra el paquete.
- `POST /api/v1/adapters/declarative` (`adapters:write`, `Idempotency-Key`) → 201 `adapter_installation`. Marcada como implementada en OpenAPI y servida por el daemon, que usa el `larm-generic-adapter.exe` que tiene al lado.

## Defectos y huecos encontrados

1. **Con un runtime real: `EXACT_TEXT` daba una versión falsa.** Mi primera versión tomaba la primera línea no vacía. `ollama --version` 0.40.3 sin servidor imprime «Warning: could not connect to a running Ollama instance» y, en la línea siguiente, la versión, así que LARM habría registrado el aviso como versión. Ahora la salida recortada tiene que ser una sola línea; si no, `version_unparseable` (versión desconocida). Hay una prueba con ese texto real y una mutación.
2. **Con la prueba real: una referencia de credencial bloquea también el sondeo.** El contrato 11 aplica al sondeo la configuración del adaptador, y una referencia sin resolver bloquea la ejecución hasta su enrolamiento. Así que un declarativo con `credential_reference` no se puede sondear ni arrancar hasta P045. Es lo que dice el contrato y queda documentado como limitación.
3. **Hueco de prueba detectado por mutación: conflicto de versión.** Comprobar la fila era redundante mientras existía la carpeta. Si la carpeta se borra, sin esa comprobación se habría creado una segunda fila para la misma versión. Ahora hay prueba.
4. **Con las mutaciones: un runtime huérfano conserva el stdout del worker.** Al lanzar un proceso, Rust estable en Windows hereda todos los handles heredables, también los pipes de stdin y stdout del worker. Comprobado en real: con el worker muerto y el runtime vivo, el stdout del worker no da EOF hasta que muere el runtime. Por eso una prueba que entraba en pánico se quedaba colgada en vez de fallar.
   - **Lo arreglado**: el worker termina el árbol de todas sus instancias al cerrarse la sesión. Hay una prueba real y una mutación que quita esa protección y `kill_on_drop`. Las pruebas apuntan los PID que lanzan y los terminan aunque entren en pánico.
   - **Lo pendiente**: si el worker muere de golpe, el runtime sobrevive. Evitarlo exige limitar la herencia (`unsafe`, prohibido en el SDK por ADR-0010) o el Job Object kill-on-close de P130. El host detecta la caída igualmente, por la salida del proceso (P021).
5. **Defectos del propio runner de mutaciones.** Tenía tres:
   - **No recompilaba el worker**: las pruebas del host usan el `larm-generic-adapter.exe` ya compilado, así que las mutaciones del worker no llegaban al binario, y 6 «sobrevivieron» sin haberse probado.
   - **Daba por detectada cualquier salida con error**, aunque ninguna prueba fallara.
   - **No tenía tiempo máximo por paso.**

   Ahora recompila el worker con cada mutación y lo restaura al final, solo cuenta como detectada una prueba que falla, entra en pánico o vence el tiempo, conserva los finales de línea y limita cada paso a 600 s.

## Decisiones

- **Sintaxis de placeholders `{NOMBRE}`**: el contrato fija los nombres, no la sintaxis. Las llaves dobles son literales.
- **Allowlist de procesos de un declarativo** = sus dos ejecutables, dentro de las raíces. `process_allowlist` del manifiesto solo admite rutas del paquete (P020), y un declarativo usa un ejecutable externo que ya existe.
- **`adapter_options` del declarativo** = `{model_path, config_path, port}`: es su `launch_options_schema`, publicado en el schema del asistente. La resolución de `MODEL_PATH` desde una variante es de P034.
- **Health y parada solo en loopback** sin `network_origins`: son el endpoint del propio proceso, y el contrato 03 exige que los endpoints de modelo escuchen en loopback y que la URL no lleve tokens.
- **Un nombre de variable que indica un secreto solo admite referencia**: la allowlist no admite secretos como texto (H04).
- **Estado `INSTALLED`, confianza `UNTRUSTED`**: el contrato no fija transiciones de `adapter_installation`. `VALIDATED` llega con la suite de conformidad (P026).
- **Exportar** = el documento del paquete; **importar** = otra alta, que no hereda confianza. Los endpoints `/adapters/export` e `/adapters/import` son de P028.
- **Límite de memoria → `UNSUPPORTED_CAPABILITY`**: sin Job Object con límites (P130) no se puede respetar, y no se da por respetado.

## Pruebas

| Prueba del prompt | Prueba | Tipo |
|---|---|---|
| Valid/invalid manifest | `manifiesto_del_declarativo` (15 casos), `schema_cerrado_y_claves_anulables_obligatorias`, `documento_valido_y_roundtrip` | UNIT |
| Placeholder injection attempt | `placeholders_tipados_y_desconocidos`, `ejecutables_y_rutas`, `simulacion_sin_ejecutar_e_inyeccion`; en real, `entorno_no_autorizado_inyeccion_y_credencial` (el worker y el host rechazan `"& calc.exe`, `..` y rutas fuera de las raíces) | UNIT y REAL |
| Probe fake executable | `sondeo_de_version_con_ejecutable_falso`: los modos `EXACT_TEXT` y `JSON_FIELD`, `artifact_hash` real, canal `custom`, sondeo colgado → `Timeout` a los 10 s | REAL |
| Health fake server | `arranque_entorno_health_y_parada_reales` (`READY`) y `health_degradado_caido_y_parada_http` (503 → `DEGRADED`, parada HTTP, runtime muerto → `FAILED`) | REAL |
| H04: variable no secreta desde la GUI, guardar, exportar, importar y que llegue al proceso | `tools/p023_real.py`. Alta por la API real con un principal real. `LARM_TEST=desde-la-gui` llega al proceso, y no llegan ni la variable del worker ni el secreto. Exportar e importar dan una alta `UNTRUSTED` con el mismo entorno. La pantalla de la GUI es de P037 | REAL |
| H04: nombres o referencias no autorizados; la exportación sin secretos | `entorno_h04` (casos H04-055..058), `autorizacion_y_validacion_antes_de_io` y los pasos `negativos-sin-efectos` y `exportacion-sin-secretos` del script real | UNIT, CONTRACT y REAL |
| Paquete manipulado | `paquete_manipulado_no_se_autoriza` (`package_hash_mismatch`) | REAL |
| Cierre de sesión | `cerrar_la_sesion_del_worker_termina_sus_runtimes` | REAL |
| API | `alta_idempotente_y_paquete_en_disco`, `autorizacion_y_validacion_antes_de_io`, `exportar_e_importar_sin_secretos_ni_confianza` | CONTRACT |
| Persistencia | `alta_lectura_y_evento_en_la_misma_transaccion` (SQLite con la migración del contrato) | INTEGRATION |

**Prueba real de extremo a extremo** (`tools/p023_real.py`, 20 pasos PASS, [pasos](P023_runs/serve-steps.json)). Sobre el daemon real con su base de datos:

- **Alta y persistencia**:
  - el hash del paquete se recalcula en Python, como oráculo independiente;
  - la fila y `adapter.updated` quedan en SQLite;
  - la repetición con la misma clave es idempotente;
  - el alta de una versión existente devuelve 409.
- **Rechazos sin efectos**: el principal solo-lectura recibe 403, y seis documentos inválidos reciben 400 sin crear fila ni carpeta.
- **Exportar e importar**: el documento exportado no contiene secretos, y la importación da una alta `UNTRUSTED`.
- **El worker del paquete creado por el daemon**, manejado por un cliente JSON-RPC escrito en Python:
  - handshake con nonce, sondeo, plan, arranque y health `READY`;
  - la variable llega al proceso;
  - parada, y el puerto queda cerrado;
  - con una referencia sin resolver, el sondeo y el arranque se bloquean sin lanzar nada.
- **Runtime real, Ollama 0.40.3**, aislado: puerto propio y `OLLAMA_MODELS`/`USERPROFILE` temporales declarados en el entorno.
  - La versión queda desconocida (`version_unparseable`), no se toma un aviso como versión.
  - `ollama serve` arranca y health da `READY`, con `/api/version` igual a la versión que obtiene el oráculo.
  - Se para, y la instancia del usuario sigue en el mismo estado.

**Mutaciones**: 25 de 25 detectadas ([mutations.json](P023_runs/mutations.json)): placeholders, `.bat`, metacaracteres, loopback, tokens en la URL, allowlists, secretos, referencias, raíces, `runtime_arguments`, límite de memoria, `EXACT_TEXT`, hash del paquete, entorno heredado, credencial, runtimes huérfanos, plan reutilizable, `plan_hash`, health, proceso muerto, canal, rutas del alta, confianza y conflicto.

**Puertas**: `tools/check.ps1 -Prompt P023 -Real`, 13 de 13 en verde: fmt, clippy `-D warnings`, `cargo test --workspace`, arquitectura, copia del paquete, el shell de escritorio, y lint, typecheck, test y build del frontend, que no se tocó. Además, coherencia del lock, regresión de P021/P022 y `validate_evidence` PASS.

## Bloqueado y pendiente

- **`P023-REAL-gui`**: la pantalla del asistente es de P037 (H04 también es de P037/P140). Aquí se probó la misma operación por la API que usa la GUI, con un principal real, y el schema del asistente queda publicado.
- **`P023-REAL-credencial-en-worker`**: resolver `credential_reference` dentro del worker es de P045. Hoy una referencia bloquea sin lanzar nada; está comprobado en real.
- **`P023-REAL-api-export-import`**: los endpoints son de P028.
- **Dependencias**: P020, P021 y P022 no tienen REAL_PASS.
- Lista y detalle de adaptadores (P024), sondeo por API y conformidad (P026), AppContainer y Job Object (P130).

## Compatibilidad

Todo es aditivo:

- operación `POST /adapters/declarative` del contrato;
- entidad de dominio y puerto nuevos;
- dependencia `larm-core → larm-adapter-sdk`, permitida por el grafo;
- `chrono` en el SDK, ya fijado en el lock.

No cambian el protocolo v1, el manifiesto ni las migraciones: `adapter_installation` ya estaba en `0001`.
