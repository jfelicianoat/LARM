# Transformers worker privado (P071)

Este worker prepara la integración de PyTorch/Transformers bajo un Python gestionado. Sigue siendo una interfaz privada: P022/P040 deben cualificar el lanzamiento y la contención del proceso antes de anunciar el adaptador como disponible en LARM.

El core identifica cada entorno con `runtime_id`, `runtime_version_id`, `backend_id`, versión exacta de Python y SHA256 del lock de dependencias. El directorio se deriva dentro del runtime store; el worker nunca acepta una ruta de destino ni ejecuta una instalación por su cuenta. Los comandos y argumentos de UV, Conda/Mamba o pip deben provenir del profile lock cualificado por P099. Una estrategia que el adaptador no anuncia falla antes de cualquier proceso o IO.

El protocolo privado v1 admite `probe`, `version`, `capabilities`, `environment_plan`, `inspect_model` y `generate_text`. El plan no crea entornos. El probe sólo carga paquetes si `sys.prefix` coincide con `LARM_PYTHON_ROOT` y `sys.executable` está dentro de ese mismo prefijo; también rechaza intérpretes que sean enlaces o junctions. Reporta backend sólo cuando Torch lo detecta y no busca ni importa paquetes del Python global.

La inferencia acepta una carpeta de `LARM_TRANSFORMERS_MODEL_ROOT`, un backend explícito (`CPU`, `CUDA` o `XPU`) y pesos locales `safetensors`. El loader fija `local_files_only=true`, `trust_remote_code=false` y `use_safetensors=true`; si `config.json` o `tokenizer_config.json` declara `auto_map`, responde `REMOTE_CODE_REQUIRED` antes de importar Transformers. La prueba sólo usa clases y pesos simulados, por lo que no acredita una inferencia real.

`sidecar.py` ofrece las funciones HTTP `/v1/models`, `/v1/completions` y `/v1/chat/completions` para texto, sin streaming, herramientas ni campos no reconocidos. `create_server` sólo acepta una IP loopback y exige un bearer token que entrega el host; no abre LAN ni carga secretos de argumentos o archivos. El worker no anuncia ni inicia el sidecar hasta que P022/P040 cualifiquen su ciclo y permisos.

Pruebas focales:

```powershell
.\.venv-tools\Scripts\python.exe -m unittest discover -s adapters\runtime\transformers\tests -p test_worker.py -v
cargo test -p larm-runtime python_env -- --nocapture
```

La ejecución productiva requiere P031, P022, P040 y profile locks P099 con versiones, comandos, wheels/paquetes, proveedor y pruebas reales fijados. No se instala nada en Python global ni se acepta automáticamente código remoto o licencias.
