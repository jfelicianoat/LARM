# P071 — Entornos Python y Transformers

El worker privado de `adapters/runtime/transformers` prepara la integración de PyTorch/Transformers en un entorno gestionado. El core usa una identidad determinista compuesta por runtime, versión runtime, backend, versión exacta de Python y hash del lock de dependencias. El directorio se deriva bajo el runtime store.

La instalación queda delegada a un profile lock cualificado: el worker no construye comandos UV, Conda/Mamba o pip a partir de entrada libre y no instala paquetes globales. Estrategias no anunciadas se rechazan antes de iniciar procesos. P031 y P099 conservan la propiedad del plan/instalación durable y del comando externo cualificado.

El probe sólo considera paquetes cuando `sys.prefix` coincide con `LARM_PYTHON_ROOT` y `sys.executable` está dentro del mismo prefijo; también rechaza intérpretes que sean enlaces o junctions. El modelo debe ser una carpeta local bajo `LARM_TRANSFORMERS_MODEL_ROOT`. Para generación se requiere una arquitectura conocida por Transformers y pesos `safetensors`; backend explícito CPU/CUDA/XPU; `local_files_only=true`; `use_safetensors=true`; `trust_remote_code=false`. Si `config.json` o `tokenizer_config.json` declara `auto_map`, la llamada falla con `REMOTE_CODE_REQUIRED` antes de cargar el runtime.

El módulo `sidecar.py` implementa el subset HTTP `/v1/models`, `/v1/completions` y `/v1/chat/completions` sólo para texto, con bearer token entregado por el host, cuerpo acotado, validación cerrada y bind exclusivo a loopback. No admite streaming, herramientas ni extensiones de campos. El host no expone ni lanza ese sidecar hasta que P022/P040 cualifiquen contención, permisos y cancelación.

La implementación y sus límites se registran en [informe P071](../reports/P071_python-environment-manager-y-pytorch-transformers.md) y [evidencia](../reports/P071_python-environment-manager-y-pytorch-transformers.evidence.json).
