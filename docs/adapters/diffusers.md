# Adaptador Diffusers — P072

P072 prepara un worker privado para `image.generate`, sin registrarlo como adaptador disponible. La generación se limita a bundles locales y a las clases integradas `StableDiffusionPipeline`, `StableDiffusionXLPipeline` y `FluxPipeline`. Los componentes del índice se validan contra pares de biblioteca/clase cualificados antes de importar Torch o Diffusers; el `model_index.json` no puede elegir módulos de código. Un `auto_map`, pipeline desconocido o componente personalizado produce `REMOTE_CODE_REQUIRED`.

El bundle usa los roles publicados por el contrato actual. UNet y transformer se conservan como `weights` y se distinguen mediante `facts.diffusers_component`; los demás componentes usan los roles `vae`, `text_encoder`, `tokenizer`, `config` y `processor`, junto con el kind `diffusers_index` para `model_index.json`. La carga fuerza safetensors, modo local y `trust_remote_code=false`. El backend es explícito y no cambia si falta el proveedor.

El worker responde sólo cuando recibe del host una huella P090 y un root de salida autorizado. Escribe un PNG y metadata local con hash, tamaño, versiones, backend, dimensiones, steps, seed, tiempo de carga, segundos por imagen y memoria pico conocida. Los componentes que el contrato no mide permanecen nulos con causa. `sidecar.py` conecta callbacks de invocación, progreso, cancelación y heartbeat a P103; la cola durable, autorización, cierre de proceso y recuperación siguen siendo responsabilidad de ese framework.

Estado de aceptación: `BLOCKED_REAL`. P071, P036, P103, P099 y P090 todavía no tienen `REAL_PASS`; este host tampoco cuenta con un entorno Diffusers gestionado ni un bundle tiny autorizado con pesos reales. Las pruebas simuladas acreditan controles de código, no inferencia.
