# Worker privado Diffusers (P072)

Este worker prepara la capacidad `image.generate` para que P103 la pueda alojar cuando sus dependencias estén cualificadas. No abre puertos ni crea una API propia. El host entrega la raíz local del bundle, el backend ya admitido, el directorio de salida y la huella de ejecución; el worker no consulta fuentes remotas, instala paquetes ni inicia trabajos durables.

El plan de entorno usa la identidad runtime/versión/backend/Python/hash del lock de P071. Un lock Diffusers distinto produce otro hash y otra carpeta; sólo se puede compartir un entorno si coincide la identidad completa. Las estrategias se aceptan únicamente si están anunciadas por el profile lock P099. No se ejecutan comandos de instalación.

La inspección reconoce `StableDiffusionPipeline`, `StableDiffusionXLPipeline` y `FluxPipeline` con componentes integrados cualificados. El Model Bundle conserva los roles contractuales vigentes: `unet` y `transformer` se representan como `weights` con `facts.diffusers_component`; VAE, text encoders, tokenizers, scheduler, processor e índice usan `vae`, `text_encoder`, `tokenizer`, `config`, `processor` y `diffusers_index`. Pesos requieren `safetensors`; `auto_map`, clases desconocidas y componentes externos fallan con `REMOTE_CODE_REQUIRED`. El worker no consume aprobaciones REMOTE_CODE, así que el código personalizado permanece bloqueado.

La carga exige Python dentro del runtime gestionado y fija `local_files_only=true`, `use_safetensors=true` y `trust_remote_code=false`. El backend CPU/CUDA/XPU debe estar disponible exactamente como fue solicitado; no hay fallback. La salida se escribe bajo el root del host, se publica por renombrado atómico y se devuelve conforme a `image_generate_output`. El archivo de metadata local registra versiones, hash del índice, dimensiones, steps, seed, tiempo de carga, segundos por imagen y memoria pico; CPU o backends sin contador quedan como desconocidos.

`sidecar.py` aporta hooks de `protocol.invoke`, `core.cancel`, `core.heartbeat` y `core.progress` a P103; no ofrece HTTP, cola durable, autorización ni recuperación propios. La progresión de inferencia informa fase y mensaje, dejando los bytes nulos porque no representan steps. El host P103 debe persistir el job, aplicar permisos/cancelación del proceso y distribuir la huella P090 antes de anunciar la capacidad.

Las pruebas locales usan un bundle diminuto ficticio y pipelines simulados. No prueban pesos reales, compatibilidad de una versión de Diffusers ni una inferencia en GPU.
