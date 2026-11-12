# Adaptadores declarativos (P023)

Un runtime sencillo se da de alta con datos, sin programar (spec §13). LARM crea un paquete con el worker genérico y lo ejecuta como cualquier adaptador: proceso aparte, protocolo v1, permisos de P022.

- Schema publicado del asistente: [`adapters/runtime/generic/wizard.schema.json`](../../adapters/runtime/generic/wizard.schema.json), que restringe `DeclarativeAdapter` (`larm.schema.json`).
- Código: [`declarative.rs`](../../crates/larm-adapter-sdk/src/declarative.rs) (documento, validación, simulación, paquete), [`generic.rs`](../../crates/larm-adapter-sdk/src/generic.rs) (worker) y [`main.rs`](../../adapters/runtime/generic/main.rs) (binario `larm-generic-adapter`).
- API: `POST /api/v1/adapters/declarative` (`adapters:write`, `Idempotency-Key`) → 201 `adapter_installation`. La lista, el detalle, la exportación/importación por API y el sondeo son de P024/P026/P028.

## Qué se puede declarar

| Campo de la spec | Campo del documento | Regla |
|---|---|---|
| Nombre | `manifest.id`, `manifest.version` | `[a-z0-9][a-z0-9._-]{0,63}`; la versión también es nombre de carpeta |
| Ejecutable | `start.executable`, `version_probe.executable` | Ruta absoluta de un `.exe` real dentro de `manifest.permissions.read_roots`. Nunca `.bat`/`.cmd` (irían por `cmd.exe`) |
| Argumentos | `start.args` | Lista: cada elemento es un argumento. Placeholders tipados |
| Comando de versión | `version_probe` | Sin placeholders. `EXACT_TEXT`: la salida recortada de stdout (o stderr si está vacío), que tiene que ser una sola línea; varias líneas dejan la versión desconocida (`version_unparseable`), nunca la primera, que puede ser un aviso (pasa con `ollama --version` sin servidor). `JSON_FIELD`: JSON Pointer (`/a/b`) o clave de primer nivel. 10 s como mucho |
| Comando de arranque | `start` | `cwd` absoluto dentro de las raíces |
| Comando de parada | `stop` | `SIGNAL` (termina el árbol) o `HTTP` (POST al endpoint y 5 s de gracia; después, el árbol). `RPC` → `UNSUPPORTED_CAPABILITY` (P103) |
| Endpoint / Health endpoint | `health.url`, `stop.endpoint` | Solo `http://127.0.0.1`, `localhost` o `[::1]` con puerto o `{PORT}`. Sin usuario, query ni fragmento: la URL no lleva tokens |
| Protocolos | `protocol_profile_id` | Identificador candidato; no implica soporte (P036/P099) |
| Formatos | `formats` | Al menos uno, sin repetir |
| Variables de entorno | `environment` | `EnvironmentConfig` (contrato 11 H04) |

El manifiesto de un declarativo es fijo en lo que no es del usuario: familia `runtime`, ejecutable `bin/larm-generic-adapter.exe` sin argumentos, `installation_strategies: []` (no instala; usa un ejecutable existente), `signature: null` y raíces absolutas. `capabilities` tiene que coincidir con `manifest.capabilities`, que es lo que anuncia el handshake.

## Placeholders

`{MODEL_PATH}`, `{CONFIG_PATH}`, `{PORT}`, `{THREADS}` y `{CONTEXT}`. `{{` y `}}` son llaves literales. Cualquier otro nombre, o una llave sin pareja, se rechaza al validar.

| Placeholder | Valor | Dónde |
|---|---|---|
| `MODEL_PATH`, `CONFIG_PATH` | `adapter_options.model_path` / `config_path`: fichero real dentro de `read_roots`, sin `..` ni metacaracteres | `start.args` |
| `PORT` | `adapter_options.port` (1..65535) | `start.args`, `health.url`, `stop.endpoint` |
| `THREADS` | `LaunchParameters.threads` | `start.args` |
| `CONTEXT` | `LaunchParameters.context` (null = sin valor) | `start.args` |

No hay shell ni expansión de variables: `$`, `%PATH%` o `&` dentro de un argumento son texto. El valor se inserta tal cual dentro de su argumento y no se vuelve a interpretar. `adapter_options` solo admite esas tres claves (`launch_options_schema` del declarativo), y `runtime_arguments` tiene que ir vacío: un declarativo no publica `runtime_arguments_schema`, y así un argumento extra no puede sustituir a `MODEL_PATH` o al puerto.

## Entorno

- Cada variable está en `allowlist`. Lleva `value` (no secreto) o `credential_reference` bajo `larm/` que además figure en `manifest.permissions.secret_refs`.
- Un nombre que indica un secreto (`TOKEN`, `SECRET`, `PASSWORD`, `PASSWD`, `API_KEY`, `APIKEY`, `CREDENTIAL`, `PRIVATE_KEY`) solo admite referencia.
- Sondeo y parada usan solo el entorno del adaptador. Para arrancar, el binding del perfil sustituye al del mismo nombre, y cada variable efectiva tiene que estar en las dos allowlists: unirlas no amplía permisos.
- El proceso arranca con el entorno vaciado, más el mínimo de Windows (`SystemRoot`, `SystemDrive`, `windir`, `NUMBER_OF_PROCESSORS`, `PROCESSOR_ARCHITECTURE`, `TEMP`, `TMP`) y las variables efectivas. No hereda nada del worker ni del usuario.
- **Limitación actual**: el worker aún no resuelve secretos (lo hará P045). Una `credential_reference` en el entorno efectivo bloquea el sondeo y el arranque con `RESOURCE_UNAVAILABLE` `credential_not_enrolled`, sin lanzar nada. Esto aplica la regla del contrato 11: «una referencia no disponible en destino bloquea ejecución hasta enrolamiento».

## Alta y paquete

1. Se valida el documento sin IO (schema cerrado, reglas anteriores).
2. Se comprueba que los ejecutables y `cwd` existen dentro de las raíces (`path_outside_roots`, `path_missing`).
3. Si esa versión ya existe: `CONFLICT` `adapter_version_exists`.
4. Se crea `<data_root>/adapters/<id>/<version>/` en una carpeta temporal que se renombra al final, con tres ficheros:
   - `bin/larm-generic-adapter.exe`: copia del que acompaña al daemon;
   - `declarative.json`: el documento, con `package_sha256` a ceros, porque el hash no puede contenerse a sí mismo;
   - `manifest.json`: con el `package_sha256` real (regla de P022, que excluye `manifest.json`).
5. `adapter_installation` se guarda con estos valores, junto con el evento `adapter.updated` en la misma transacción:
   - `UNTRUSTED`, `INSTALLED`;
   - `metadata`: `kind=DECLARATIVE`, `signature_state=CUSTOM`, `execution_mode=TRUSTED_ONLY`, `document_hash` y `package_path`.

   Si falla, se borra el paquete.

Para ejecutarlo hace falta la concesión TRUSTED_ONLY de P022, porque un declarativo no lleva firma de publicador. Cambiar `declarative.json` cambia el hash del paquete, y la política lo rechaza (`package_hash_mismatch`).

**Exportar** es el documento del paquete: contiene valores no secretos y referencias, nunca un secreto. **Importar** es otra alta. Nunca hereda confianza y vuelve a calcular el hash.

## Worker genérico

El worker genérico implementa el lanzamiento y la supervisión de procesos. Para
usarlo como launcher, declarar `capabilities: []` tanto en el documento como en su
manifiesto. Una capacidad no vacía requiere una composición explícita con
`GenericRuntime::with_capability_registry` y su mapping/estimador implementados.
Sin ese registro, el SDK rechaza el handshake con `capability_descriptor_missing`
antes del sondeo. Importar un documento conserva sus capacidades declaradas como
candidatas; no importa código ni descriptores externos en el worker genérico.

La referencia runtime usa un wrapper Rust con contrato echo explícito. Su fábrica
lo empaqueta en `bin/larm-generic-adapter.exe`, nombre requerido por el schema
declarativo. No equivale a obtener ese wrapper al importar sólo el documento:
el importador materializa el worker genérico distribuido por LARM.

| Método | Comportamiento |
|---|---|
| `runtime.list_versions` | Ejecuta el sondeo. Una `runtime_version` en canal `custom` (H01: lo que no se puede interpretar no es `stable`), `promotion NONE`, `artifact_hash` = sha256 del ejecutable. Pedir otro canal devuelve lista vacía |
| `runtime.launch_plan` | Valida antes de IO y simula. `LaunchPlan` con `env_allowlist`, `environment` (solo valores no secretos y referencias), `effective_memory_limit_bytes: null` y `memory_estimate` `UNKNOWN`. Plan de un solo uso durante 900 s |
| `runtime.start` | Por `plan_id` y `plan_hash`. Un plan usado, caducado o con otro hash se rechaza |
| `runtime.health` | Proceso muerto: `FAILED`. Sin respuesta: `LOADING`. Otro estado HTTP u otro valor: `DEGRADED`. Si coincide: `READY` |
| `runtime.stop` | `HTTP` o `SIGNAL`; al final siempre termina el árbol |
| Resto de obligatorios | `UNSUPPORTED_CAPABILITY` `declarative_method_not_applicable`: un declarativo no instala, ni inspecciona modelos, ni estima memoria |

Un `memory_limit_bytes` o un `max_memory_bytes` no nulos devuelven `UNSUPPORTED_CAPABILITY` `memory_limit_unsupported`. El worker no puede imponerlos (Job Object con límites: P130) y no los da por respetados.

## Ciclo de vida de los procesos

- Al cerrarse la sesión del worker (el host cierra stdin), el worker termina el árbol de todas sus instancias antes de salir. Una parada ordenada del host no deja runtimes huérfanos; lo comprueba una prueba real.
- **Limitación (hasta P130)**: si el worker muere de golpe, sus runtimes siguen vivos. Además, conservan heredado el pipe de stdout del worker, porque Rust estable crea los procesos heredando todos los handles heredables y limitarlo exige `unsafe`, que el SDK no admite (ADR-0010). Se ha observado en real: con el worker muerto y el runtime vivo, el stdout del worker no da EOF hasta que muere el runtime. El host detecta igualmente la caída por la salida del proceso (P021). Matar el árbol con su worker es el Job Object kill-on-close de P130.
