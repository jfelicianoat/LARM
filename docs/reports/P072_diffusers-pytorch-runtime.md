# P072 — Diffusers/PyTorch runtime

Fecha: 2026-10-23  
Estado: BLOCKED_REAL

P072 prepara un worker privado para `image.generate`. El plan de entorno conserva la identidad runtime/versión/backend/Python/hash de lock de P071, por lo que locks de Diffusers diferentes derivan carpetas distintas. El worker no instala paquetes. La generación exige el intérprete gestionado y un profile lock cualificado por P099.

El inspector acepta únicamente `StableDiffusionPipeline`, `StableDiffusionXLPipeline` y `FluxPipeline`, valida cada componente antes de importar Torch/Diffusers y requiere pesos safetensors. El bundle representa UNet/transformer con el rol contractual `weights` y `facts.diffusers_component`; conserva `vae`, `text_encoder`, `tokenizer`, `config`, `processor` y `diffusers_index` para los demás artefactos. `auto_map`, clases personalizadas y módulos externos fallan con `REMOTE_CODE_REQUIRED`. La carga fija `local_files_only=true`, `use_safetensors=true`, `trust_remote_code=false` y backend explícito sin fallback.

`sidecar.py` aporta hooks internos para `protocol.invoke`, progreso por fase, cancelación y heartbeat a P103. No abre un puerto ni crea cola durable. Una inferencia requiere además la huella P090 y escribe un PNG dentro del root autorizado con hash SHA-256. La metadata local registra pipeline, hash del índice, backend, versiones, dimensiones, steps, seed, tiempo de carga, segundos por imagen y memoria pico; CPU o backends sin medidor quedan como desconocidos. El fixture diminuto usa archivos y pipeline simulados, no pesos reales.

Verificación focal: 5 PASS, 0 FAIL, 2 BLOCKED. Las 12 pruebas cubren validación de bundle, roles, rechazo de código remoto, loader offline/safetensors, salida de imagen contractual, progreso, cancelación y limpieza ante fallos. También pasan formato Rust y Clippy del workspace. La prueba de contrato resuelve las referencias privadas contra el schema público y valida `protocol.invoke` y la salida; el probe Windows rechaza el Python de herramientas sin importar Torch/Diffusers.

La aceptación REAL sigue bloqueada por P071, P036, P103, P099 y P090 sin `REAL_PASS`, y por la falta de runtime/modelo local autorizado. La puerta común no se repitió porque su `cargo_test` de P071 no terminó tras más de diez minutos; no hay resultado oficial de P072. La documentación oficial confirma que `local_files_only` evita descargar desde Hub y que `trust_remote_code=true` puede ejecutar código de pipelines comunitarios [Diffusers loading](https://huggingface.co/docs/diffusers/main/api/diffusion_pipeline), [community pipelines](https://huggingface.co/docs/diffusers/main/using-diffusers/custom_pipeline_overview). P072 permanece `BLOCKED_REAL` y no se anuncia como adaptador disponible.
