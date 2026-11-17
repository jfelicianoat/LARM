# P061 — llama.cpp: compatibilidad, serving, smoke tests y llama-bench

Fecha: 2026-11-15.  
Estado: **BLOCKED_REAL** — el núcleo del adaptador está implementado y **verificado con ejecución real en el PC IA** (compatibilidad con GGUF real de Ollama, ciclo de vida completo de llama-server con health/models/embeddings, llama-bench real en los dos builds con salida cruda); el paquete del Runtime Adapter Protocol con admisión/leases y el smoke de generación con modelo de chat siguen bloqueados por dependencias sin REAL_PASS.

## Implementación (`adapters/runtime/llama-cpp/core.py`)

- **Compatibilidad por metadata GGUF + capacidades del build** (checklist 1): lector de cabecera GGUF propio sin dependencias — versiones 1-3, tipos del estándar (con 9=array, clave para los vocabularios reales), arrays de cientos de miles de strings de tokenizer saltados sin decodificar con alineación exacta. `check_compatibility` decide por `general.architecture` + `general.file_type` contra el enumerado de arquitecturas conocidas y las capacidades observadas del build (P060); arch desconocido → incompatible (`unsupported_arch` en reasons); **tool calling y multimodal false salvo evidencia** (checklist 7).
- **ModelBundle con mmproj** (checklist 3): `resolve_model_bundle` exige el mmproj del bundle para arquitecturas multimodales (`bundle_incomplete` si falta) y hash del modelo.
- **Lanzamiento sin shell** (checklist 2): `build_launch_args` sólo acepta los campos contractuales del LaunchProfile (`port/host/context_size/threads/gpu_layers/parallel_slots/timeout_idle_seconds`), enteros validados positivos, campos desconocidos rechazados (`launch_profile_unknown_fields`); el proceso se lanza con argv directo, entorno mínimo, log a fichero y CREATE_NO_WINDOW.
- **Ciclo de vida de llama-server** (checklist 4): `launch_server` + `ServerHandle.health` (espera 200 con plazo y detección de salida prematura del proceso), `/v1/models` (model listing/protocol) y `stop()` determinista (terminate → wait → kill).
- **Smoke de inferencia con token cap** (checklist 5): `chat` con `max_tokens` y timeout propios, rechaza respuestas vacías (`smoke_empty`).
- **llama-bench versionado** (checklist 6): `run_llama_bench` ejecuta con **cinco repeticiones** (`-r 5`), guarda el **raw output** con sha256 y parsea el formato tabular md de forma versionada (`bench_unsupported_output` ante cualquier otro formato); la versión del build se sonsajea del `llama-cli` adyacente (llama-bench no tiene `--version`; su salida de versión va por **stderr** en los builds MSVC).

## Defectos encontrados durante la fase (corregidos con evidencia real)

1. **Tabla de tipos GGUF desplazada**: la especificación fija 9=array (no uint64) — el blob real de Ollama (`general.tags` como array de strings) descarrilaba el lector; corregido y verificado contra el blob real (arch bert, 101 tensores, 34 kv).
2. `launch_server` recibía el ejecutable separado y el argv lo duplicaba (argumento inválido del server); ahora recibe el argv completo.
3. La salida `--version` de llama.cpp va a **stderr** en los builds MSVC vía subprocess — el probe combinaba sólo stdout y reportaba "unknown".
4. El parser contaba la línea separadora `|---|` como fila de datos.

## Evidencia real (PC IA, 2026-11-15)

Oráculo focal `tests/test_p061_core.py`: **12/12 PASS** (7 sintéticos + 5 REAL). Los REAL:

- `REAL compat`: metadata del **GGUF real de Ollama** (granite-embedding 30m, 59 MB, blob sha256-27d24c87…), arch `bert`, capabilities con embeddings true y tool_calling/multimodal false.
- `REAL server`: llama-server real (build b79696f) en puerto loopback → health 200, `/v1/models` con el modelo, `/v1/embeddings` con **vector de 384 dimensiones**, parada determinista verificada.
- `REAL bench`: llama-bench real en **los dos builds** (b79696f y d2d6269) — versiones distintas en la salida, 5 repeticiones, 2 filas parseadas por build, raw output persistido con sha256.

## Pendientes (bloqueos reales)

1. **Paquete del Runtime Adapter Protocol** de llama.cpp con admisión runtime.install y leases: la admisión P031 exige profile lock cualificado (P099 UNQUALIFIED) y las interfaces P034/P036 conservan BLOCKED_REAL (contrato 00).
2. **Smoke de generación de texto** con modelo de chat (tev1 4b / gemma4 12b): requiere autorización explícita del operador para servir esos GGUF fuera de Ollama; el smoke de embeddings ya está probado con el GGUF real más pequeño.
3. **VLM/mmproj**: la ruta está implementada (bundle + capabilities) pero sin mmproj real disponible en el PC IA para evidencia (bloqueo honesto).

## Verificación

- Focal 12/12; runner Python **9 suites OK**; `tools/check.ps1 -Prompt P061 -Real` → **STATUS PASS (11 checks, 0 fallos)**; evidence formal **BLOCKED_REAL (4 PASS, 0 FAIL, 2 BLOCKED)**.
