# Protocolo de adaptadores v1

Contrato 03 de LARM. Implementado en `crates/larm-adapter-sdk`. Los schemas generados están en [`schemas/`](schemas/) y los ejemplos válidos e inválidos, en [`examples/`](examples/). Se regeneran con `python tools/p020_schemas.py write`; `check` comprueba que están al día y valida los ejemplos.

## Manifiesto (`AdapterManifest`)

Lleva exactamente los campos del schema, sin campos adicionales:

`id`, `version`, `family`, `protocol_version` (`"1.0"`), `publisher`, `executable {relative_path, args}`, `platforms`, `architectures`, `methods`, `permissions {read_roots, write_roots, network_origins, process_allowlist, secret_refs}`, `capabilities`, `package_sha256`, `signature {algorithm: Ed25519, key_id, value} | null` e `installation_strategies`.

Reglas que el schema no expresa:

| Campo | Regla |
|---|---|
| `family` | `runtime`, `source`, `protocol`, `transformation` o `benchmark` |
| `methods` | Solo las 40 firmas publicadas en `adapter-methods.json`, de su familia o comunes. Deben estar las tres comunes (`core.handshake`, `core.cancel`, `core.heartbeat`) y las obligatorias de su familia. No hay contrato público alternativo |
| `executable.relative_path` y `process_allowlist` | Rutas relativas dentro del paquete: sin rutas absolutas, unidad, `..` ni caracteres de shell |
| `capabilities` | Identificadores con espacio de nombres (`runtime.gguf`, `vendor.acme.npu`). Las desconocidas **se conservan** sin interpretarlas (`RuntimeAdapterCapabilities` y `ModelSourceAdapterCapabilities`) |
| `installation_strategies` | Las doce canónicas de §52 (`PREBUILT_BINARY`, `GITHUB_RELEASE`, `SOURCE_BUILD`, `WINGET`, `PIP`, `UV`, `CONDA`, `MAMBA`, `NUGET`, `WSL`, `CONTAINER`, `CUSTOM`) y las tres extensiones del contrato 11 (`MSI_MANAGED`, `PYTHON_VENV`, `EXTERNAL_ADOPTION`), sin repetir. Una familia que no instala declara la lista vacía |
| `permissions.network_origins` | Solo orígenes `http(s)://host[:puerto]` |
| `permissions.secret_refs` | Referencias `larm/…` de Credential Manager, nunca valores |

Antes de invocar al worker, `require_strategy` rechaza una estrategia que el manifiesto no anuncia con `UNSUPPORTED_CAPABILITY` (422), y `require_method` hace lo mismo con un método.

`runtime.install_status` es una firma opcional de runtime para recuperar un job de instalación. Recibe únicamente `plan_id` y `plan_hash` y devuelve el `JobReceipt` ligado al mismo ID y a `/api/v1/jobs/{id}`. Debe consultar el registro durable del adaptador sin volver a iniciar ni reintentar la instalación. Si no está anunciada, el estado es desconocido, la consulta falla o la respuesta sigue en `QUEUED`/`RUNNING`, LARM conserva el job y sus reservas. Sólo una respuesta terminal válida y el cierre limpio de la sesión permiten completar o fallar el job.

## Transporte

- **NDJSON sobre stdio**: una línea UTF-8 por frame, a lo sumo 1 MiB.
- **stdout** solo lleva frames. Los logs van a stderr (máximo 10 MiB).
- **Peticiones**: `{"jsonrpc":"2.0","id":<UUIDv7>,"method":…,"params":…}`.
- **Respuestas**: una sola por `id`, con `result` o con `error`, nunca ambos.
- **Progreso**: la única notificación es `core.progress`, sin `id`, con `{request_id, phase, completed_bytes, total_bytes, message_code}`. Nunca lleva texto para el usuario ni credenciales; un progreso con otra forma se rechaza.

## Errores

| Código | Cuándo |
|---|---|
| `-32700` | JSON mal formado, no UTF-8 o frame demasiado grande (`id` null). La sesión sigue |
| `-32600` | JSON que no es un frame válido (`jsonrpc` distinto de 2.0, id que no es UUIDv7, campos de más) |
| `-32601` | Método inexistente o no anunciado. `data` lleva `UNSUPPORTED_CAPABILITY`, nunca un éxito vacío |
| `-32602` | Parámetros inválidos |
| `-32000` | Error de dominio: `data` es el `LarmError` completo |

El contrato 03 escribe `CAPABILITY_UNSUPPORTED`; el catálogo de errores del contrato 07 lo llama `UNSUPPORTED_CAPABILITY`, y se usa el del catálogo.

## Sesión

1. El host envía `core.handshake {protocol_version, adapter_id, nonce}` con un nonce aleatorio. Cualquier otra petición anterior recibe `-32000` `handshake_required`.
2. El adaptador responde `{protocol_version, adapter_version, methods, capabilities, nonce}`. El host rechaza la sesión, antes de cualquier IO, en estos casos:
   - otra versión mayor (`protocol_version_mismatch`);
   - otro nonce (`nonce_mismatch`);
   - otra versión del adaptador;
   - métodos fuera de su manifiesto.

   El adaptador, por su parte, rechaza un host de otra versión mayor o un `adapter_id` que no es el suyo.
3. `core.heartbeat` devuelve `{alive, running_request_ids}`. El host lo envía cada 10 s, y 3 ausencias son `ADAPTER_CRASHED`.
4. `core.cancel {request_id}` devuelve `{cancelled}`:
   - si la petición está en curso, `true`, y esa petición termina con su única respuesta (`request_cancelled`);
   - si ya no está en curso, `false`;
   - el host concede una gracia de 5 s antes de matar el proceso.
5. `initialize` (legado) solo se traduce a `core.handshake`; no es un método público.

## Escribir un adaptador

Hay que implementar `AdapterHandler` (manifiesto y `call` para los métodos de familia) y llamar a `server::serve(stdin, stdout, handler)`. El SDK resuelve el handshake, el heartbeat, la cancelación, los métodos no anunciados, los frames rotos y una sola respuesta por id.

Ejemplo: `crates/larm-adapter-sdk/examples/demo_adapter.rs`, de la familia benchmark.
