# P020 — Adapter manifest y protocolo v1

**Fecha**: 2026-10-12 · **Host**: A9_MEGA (PC IA) · **Estado**: **BLOCKED_REAL**. Todas las pruebas pasan, incluida la real (8 pasos con un adaptador como proceso), pero sus dependencias P002, P003, P004 y P009 no tienen REAL_PASS. [evidence.json](P020_adapter-manifest-y-protocolo-v1.evidence.json) · [protocolo](../adapters/protocol-v1.md)

## Qué se construyó (`crates/larm-adapter-sdk`)

- **`manifest.rs`**:
  - `AdapterManifest` exactamente según el schema;
  - la tabla de las 38 firmas publicadas, cargada de `adapter-methods.json` con sus obligatorias comunes y por familia;
  - las 15 estrategias de instalación: las 12 canónicas de §52 y las 3 extensiones del contrato 11;
  - `require_strategy` y `require_method`, que rechazan con `UNSUPPORTED_CAPABILITY` antes de invocar al worker;
  - `RuntimeAdapterCapabilities` y `ModelSourceAdapterCapabilities`, extensibles con espacio de nombres y que conservan las desconocidas.
- **`protocol.rs`**:
  - NDJSON JSON-RPC 2.0: una línea por frame, 1 MiB e ids UUIDv7;
  - códigos -32700, -32600, -32601, -32602 y -32000 (con `data` = `LarmError`), y su traducción a `LarmError`;
  - `core.progress` de forma cerrada;
  - handshake con nonce aleatorio, versión mayor y comprobación frente al manifiesto;
  - cancelación y heartbeat;
  - traducción de `initialize` (legado).
- **`server.rs`**: bucle `serve` para escribir adaptadores:
  - handshake obligatorio primero;
  - heartbeat y cancel resueltos por el SDK;
  - métodos no anunciados con -32601;
  - una tarea y una sola respuesta por petición;
  - frames rotos sin cortar la sesión.
- **`examples/demo_adapter.rs`**: adaptador real de la familia benchmark.
- **Documentación y schemas**:
  - `docs/adapters/protocol-v1.md`;
  - `docs/adapters/schemas/*`: 9 schemas generados desde `larm.schema.json` con `tools/p020_schemas.py`, sin redactar ningún DTO a mano;
  - `docs/adapters/examples/{valid,invalid}`: 24 ejemplos.

## Defecto encontrado por las pruebas

Un `core.progress` con un campo de texto libre (`message`) era inválido según el schema, pero el SDK lo aceptaba. El contrato prohíbe mensajes de usuario en el progreso. Ahora `decode` exige la forma cerrada de `Progress` y solo admite esa notificación. Lo encontró la prueba que pasa los mismos ejemplos por el schema y por el SDK.

## Decisiones y discrepancias

- **Nombre del error**: el contrato 03 escribe `CAPABILITY_UNSUPPORTED`, pero el catálogo de errores del contrato 07 (la fuente de los códigos) lo llama `UNSUPPORTED_CAPABILITY`. Se usa el del catálogo.
- **Estrategias de instalación**: solo la familia runtime tiene métodos de instalación, así que las demás deben declarar `installation_strategies` vacío («una familia que no instala declara lista vacía»).
- **Cancelación**: el catálogo no tiene código `CANCELLED`. La petición cancelada responde -32000 con `CONFLICT` `request_cancelled` (reintentable).
- **Alcance**: el host (proceso por adaptador, cola, concurrencia y Job Object) es de P021. Aquí están el contrato, el SDK y el lado del adaptador.

## Pruebas

| Prueba del prompt | Dónde |
|---|---|
| Schema validation | `tools/p020_schemas.py check` (jsonschema), la prueba de ejemplos en Rust y la real (cada frame validado) |
| Version mismatch | Handshake en ambos sentidos (unitaria e integración) y real (adaptador que anuncia 2.0) |
| Unknown capability preserved | Roundtrip del manifiesto, conjuntos de capacidades, handshake y real |
| Malformed JSON | Unitaria, integración y real (-32700 con id null y la sesión sigue) |
| Estrategia no anunciada (contrato 11) | `estrategia_no_anunciada_antes_del_worker` |

**Negativos**:

- campos de más y enum desconocido;
- `protocol_version` 2.0;
- rutas absolutas, con `..` o con shell;
- métodos inventados o de otra familia, y obligatorios ausentes;
- capacidades sin espacio de nombres;
- estrategias repetidas o desconocidas;
- secretos en claro y orígenes con ruta;
- firma no Ed25519;
- handshake de otro adaptador;
- parámetros inválidos;
- frames de más de 1 MiB;
- ids que no son UUIDv7;
- notificaciones no previstas.

**Mutaciones**: cada una se aplicó y se restauró el código. Las 7 hacen fallar al menos una prueba:

1. sin comprobar el nonce;
2. sin comprobar la versión;
3. handshake opcional;
4. un método no anunciado llega al handler;
5. progreso de forma libre;
6. cualquier estrategia aceptada;
7. métodos inventados en el manifiesto.

La de la forma del progreso no se detectaba al principio con la variante «solo el método»; se añadió el caso.

## Prueba real (`tools/p020_real.py`, 8 pasos PASS)

Un host Python independiente lanza `demo_adapter.exe` como proceso y valida cada frame con los schemas generados.

1. **Manifiestos**: el bueno lleva el `package_sha256` real del ejecutable; el de la estrategia NPM sale con exit 2, stdout vacío y el motivo en stderr.
2. **Handshake obligatorio**: una petición antes del handshake recibe `handshake_required`.
3. **Handshake**: nonce exacto y capacidad desconocida conservada.
4. **JSON mal formado**: -32700 y la sesión sigue.
5. **Métodos no anunciados**: tres casos, todos -32601 con `UNSUPPORTED_CAPABILITY`.
6. **Ejecución**:
   - progreso válido;
   - heartbeat con el id en curso;
   - cancel con `cancelled=true` y una respuesta por id.
7. **stdout**: 13 líneas, todas frames; al cerrar stdin, exit 0.
8. **Versión incompatible**: el adaptador que anuncia 2.0 se rechaza.

## Pendiente

- **Host de adaptadores**: proceso por adaptador y versión, cola de 64, 4 RPC concurrentes, Job Object, heartbeat de 10 s con 3 fallos y `ADAPTER_CRASHED`. Es P021.
- **Verificación de la firma Ed25519 y del `package_sha256` al instalar un paquete**: la importación de adaptadores (`POST /adapters/import`, P028).
- **Comandos reales de UV y CONDA/MAMBA**: P099.
