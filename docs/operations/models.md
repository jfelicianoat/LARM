# Registro de modelos (P040)

El registro separa la identidad de un modelo de sus publicaciones y de sus revisiones (spec §17, contrato 01):

| Entidad | Qué es | Identidad |
|---|---|---|
| `canonical_model` | El modelo conceptual (Qwen-X) | `upstream_identity` explícita, si existe |
| `model_distribution` | Su publicación en una fuente (Hugging Face, ModelScope…) | `(source_id, upstream_namespace, upstream_model_id)` |
| `model_revision` | Una revisión de esa publicación | `(distribution_id, revision_key)` inmutable |
| `artifact_provenance` | De dónde sale un fichero concreto | Una fila por artefacto y origen |

## Agrupar sin inventar

- Un modelo con `upstream_identity` se agrupa con el que ya la tiene: el mismo modelo publicado en dos fuentes es **un** modelo con dos distribuciones.
- Sin identidad upstream, **nunca se fusiona por nombre**. Se crea otro modelo y los dos se anotan mutuamente en `possible_duplicates`.
- Una publicación ya registrada identifica a su modelo, así que repetir una importación no crea nada.
- Una publicación que ya pertenece a otro modelo es `CONFLICT` `distribution_belongs_to_other_model`.

## Revisiones

- `revision_key` es un commit, un digest o una versión de la fuente. No cambia nunca: la misma clave con otro `manifest_hash` es `CONFLICT` `revision_key_immutable`.
- `pending:{UUIDv7}`: observación sin pin. Solo puede estar `PROVISIONAL`, `SUPERSEDED` o `REJECTED`; no se activa.
- `content:{bundle_hash}`: revisión final tras verificar los bytes; el hash debe ser el `manifest_hash`.
- Una distribución tiene a lo sumo una revisión `CURRENT`.
- La metadata original de la fuente se guarda tal cual en `metadata`.

## Identificadores extensibles

- **Capacidades**: segmentos en minúsculas separados por `.` (`text.generate`). Los alias del contrato (`ocr`, `vision`, `embedding`…) se guardan con su id. Una capacidad sin contrato (`audio.diarize`) se acepta y se informa como `unqualified_capabilities`: no cuenta como soportada hasta tener schema y cualificación.
- **Formato, cuantización y arquitectura**: 1–64 caracteres `[A-Za-z0-9_.+-]`, guardados tal cual. No son enums cerrados.

## Procedencia (§19)

`artifact_provenance` guarda fuente, revisión, licencia (id y hash), confianza (`UNTRUSTED`, `DECLARED`, `VERIFIED`, `REVOKED`), actor y evidencias. El linaje de conversión va en `parents` + `tool_id`/`tool_version` (siempre juntos) + `parameters`. Repositorio, autor original, uploader, URL, gated, modelo base, cuantizador y fecha de descarga van en `metadata` con esos nombres.

La interfaz distingue el origen (`origin_class`):

| Clase | Cuándo |
|---|---|
| Editor oficial | Verificado, y el uploader es el autor original |
| Distribución verificada | Verificado, subido por otro |
| Conversión de la comunidad | Tiene padres (es una conversión) o solo está declarado |
| Origen desconocido | Sin fuente, no fiable o revocado |

No se infiere ni la licencia ni el tamaño.

## API (solo lectura, scope `read`)

| Ruta | Filtros |
|---|---|
| `GET /api/v1/models`, `/models/{id}` | `q`, `capability` (acepta alias) |
| `GET /api/v1/model-distributions`, `/{id}` | `q`, `source_id` |
| `GET /api/v1/model-revisions`, `/{id}` | `state` |

Un filtro no previsto para la ruta es 400. `GET /models` devuelve grupos `CanonicalModel`; las acciones sobre un modelo (`move`, `verify`, …) usan el id de la variante.

## Importar

```
larm-daemon models-import --file modelos.json
```

El fichero es una lista de `{name, upstream_identity, capabilities, metadata, distributions: [{source_id, upstream_namespace, upstream_model_id, license_id, metadata, revisions: [{revision_key, upstream_commit, manifest_hash, state, metadata}]}]}`. El comando aplica las mismas reglas, es idempotente e imprime un informe por modelo. Las fuentes deben existir (su API es de P042).

# Variantes, bundles e identidad (P041)

## Variante y bundle

Una variante (`model_variant`) es una representación concreta: formato, cuantización y arquitectura. Sus ficheros son artefactos (`model_artifact`). Cada artefacto lleva su rol en `metadata.role`:

`weights`, `tokenizer`, `processor`, `mmproj`, `vae`, `text_encoder`, `vocoder`, `config`, `external_data` u `other`.

El bundle (`model_bundle`) agrupa todo lo que la variante necesita para ejecutarse (spec §21):

- **`manifest_hash`**: sha256 RFC 8785 de la lista ordenada `{relative_path, sha256, size_bytes, required}` de **todos** los artefactos.
- **`bundle_hash` de la variante**: el mismo cálculo, pero solo con los **requeridos**. Es parte de su identidad (contrato 01).
- **Rutas**: POSIX relativas, sin `\`, `..`, absolutos, unidad, ADS ni duplicados sin distinguir mayúsculas.
- **Datos incompletos**: si un artefacto no tiene sha256 o tamaño, no hay bundle. No se inventa.

### Estado del bundle

| Estado | Cuándo |
|---|---|
| `INCOMPLETE` | Al registrarse, hasta verificar los bytes. También si falta un requerido (un shard, el tokenizador…) o un rol necesario |
| `CORRUPT` | Algún fichero tiene otros bytes |
| `VERIFIED` | Todos los requeridos coinciden. Un opcional ausente no lo impide |

### Roles necesarios según el formato

Si el formato no se conoce, no se exige nada.

| Formato | Roles necesarios |
|---|---|
| GGUF y ONNX | `weights` |
| GGUF con `vision.understand` | `weights` y `mmproj` |
| SafeTensors, Transformers y PyTorch | `weights`, `config` y `tokenizer` |
| Diffusers | `weights`, `vae`, `text_encoder` y `config` |
| Cualquiera con `speech.synthesize` | Añade `vocoder` |

### Verificar

```
larm-daemon bundle-verify --variant <id> --dir <carpeta>
```

Hashea cada artefacto y guarda el estado con CAS. La evidencia queda en `metadata.verification` del bundle: ausentes, distintos, roles que faltan y lo observado.

## Resolución de identidad (§18)

Al registrar un modelo sin identidad upstream, se compara con los candidatos:

- homónimos;
- modelos que comparten algún hash de artefacto;
- modelos con la misma arquitectura, autor, modelo base, configuración o tokenizador;
- modelos cuya base es este, o que son su base.

| Grado | Cuándo | Qué se hace |
|---|---|---|
| `IDENTICAL` | Misma identidad upstream explícita, o mismos hashes requeridos con la misma configuración y tokenizador | Se agrupa (único grado con fusión automática) |
| `DERIVED_MODEL` | Uno declara al otro como base, o deriva de sus bytes (hashes padre) | Otro modelo, con `metadata.lineage` |
| `PROBABLE_SAME_MODEL` | Misma arquitectura y algún indicio (autor, base, configuración, tokenizador o nombre), o mismos pesos con otra configuración | Otro modelo; `possible_duplicates` en los dos |
| `UNRELATED` | Distinta arquitectura, o identidades upstream distintas sin base común | Otro modelo, sin anotar |
| `UNKNOWN` | Falta evidencia | Solo se anota como duplicado si coincide el nombre |

Cada resolución se guarda con sus motivos en `metadata.identity_resolutions`, y la evidencia en `metadata.identity_evidence`.

## API (scope `read`)

| Ruta | Filtros |
|---|---|
| `GET /api/v1/variants`, `/variants/{id}` | `state` (ciclo de vida), `capability` |
| `GET /api/v1/models/{id}/variants` | `state`, `capability`. El `{id}` es el del modelo canónico |
| `GET /api/v1/model-bundles`, `/{id}` | `state` |
| `GET /api/v1/model-artifacts`, `/{id}` | `q` |
