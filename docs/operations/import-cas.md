# Importación, CAS y deduplicación (P017)

## Importar una carpeta de modelos

```
larm-daemon models-import-dir --dir <carpeta> --location <ubicación> --source <fuente> [--mode adopt|copy] [--host-managed]
```

| Modo | Qué hace | Propiedad | `registration_mode` |
|---|---|---|---|
| `adopt` (por defecto) | Registra cada fichero donde está. No mueve, no copia y no cambia ni un byte ni una fecha. La carpeta debe estar dentro de la ubicación indicada | `EXTERNAL`, o `HOST_MANAGED` con `--host-managed` (por ejemplo, la caché de Lemonade u Ollama) | `ADOPTED` (en la metadata de la variante y de cada ubicación) |
| `copy` | Copia verificada al CAS de la ubicación. El original no se toca | `LARM_MANAGED`, o `SHARED` si el blob lo usan varios artefactos | `MANAGED` |

`--source` es una fuente registrada (la «carpeta local»). Su API llega en P042.

### Qué reconoce el escáner

El escáner reconoce los ficheros por su contenido y su nombre, y nunca ejecuta ni deserializa código:

- **GGUF**: por la magia `GGUF`. De la cabecera, leída con límites, toma `general.architecture` y `general.name`. Agrupa los shards `-NNNNN-of-NNNNN`, y el `mmproj` acompaña a los pesos de su carpeta. La cuantización sale del nombre (`Q4_K_M`, `Q8_0`…).
- **SafeTensors**: por la cabecera JSON con su longitud. También los índices `*.index.json` con `weight_map`.
- **ONNX** y sus `external_data`.
- **PyTorch** (`.bin`, `.pt`, `.pth`): zip o pickle. Un pickle se marca como `unsafe_pickle` y no se abre.
- **Configuración y tokenizador**: `config.json` (de él toma `model_type` como arquitectura) y `tokenizer.json`/`tokenizer.model`. Los auxiliares del tokenizador son opcionales.
- **Procesador**: `preprocessor_config.json`.
- **Otros manifiestos**: Diffusers (`model_index.json`), MLC (`mlc-chat-config.json`) y manifiestos de Ollama.
- **Documentación**: README y LICENSE, como opcionales.
- **Lo demás** (`.exe`, `.txt`, un `.gguf` sin magia…) se ignora y aparece en `ignored` con el motivo.
- **Enlaces y junctions** no se siguen.

Cada carpeta con pesos da una variante. Las variantes de una misma carpeta y un mismo nombre forman un modelo con una distribución `local` y una revisión `content:{hash}`. Si el contenido cambia, la revisión nueva sustituye a la anterior, que pasa a `SUPERSEDED`.

### Hash y caché

- El SHA-256 se calcula en streaming.
- La caché por tamaño + mtime (`metadata.hash_cache` de cada ubicación) solo evita releer en una reimportación.
- Una fila creada a partir de la caché queda con `verified_at` nulo, porque la caché no es una verificación. `bundle-verify` siempre relee los bytes.
- Si un fichero cambia mientras se hashea, se rechaza.

## CAS (`root/blobs/sha256/ab/<sha256>`)

- La copia pasa por `staging/<job>/<sha>.partial`, se relee el hash y se coloca sin sobrescribir, **de solo lectura**.
- Si el blob ya existía, no se duplica: las filas de esa ruta pasan a `SHARED`. Un blob compartido no se mueve (P016) ni se borra mientras tenga referencias (P018).
- El CAS es opcional por ubicación. Nunca se aplica a carpetas `HOST_MANAGED`.

## Vista para un runtime

```
larm-daemon runtime-view --location <cas> --variant <id> --out <carpeta>
```

Crea los nombres originales de la variante apuntando a los blobs:

- **Mismo volumen** (NTFS o ReFS): hard links.
- **Sin enlace posible** (otro volumen, recurso de red, FAT): una referencia separada a la ruta del blob en `view.json`. Nunca se copia ni se modifica un blob.

## Duplicados

```
larm-daemon storage-duplicates
```

Lista los ficheros con el mismo sha256 en más de un sitio físico. No borra nada.

| Campo | Qué cuenta |
|---|---|
| `duplicate_bytes` | Todas las copias de más |
| `reclaimable_bytes` | Solo las copias de más **en el mismo volumen**. Entre volúmenes no se asume deduplicación física |
