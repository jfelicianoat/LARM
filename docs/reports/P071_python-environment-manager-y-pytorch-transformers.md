# P071 — Python Environment Manager y PyTorch/Transformers

Fecha: 2026-10-23  
Estado: BLOCKED_REAL

P071 prepara la identidad y el worker privado del runtime Python. `PythonEnvironmentIdentity` incluye runtime, versión, backend, versión exacta de Python y hash SHA256 del lock; el core deriva un path dentro del runtime store sin aceptar destinos del caller. El worker sólo planea entornos, exige que la estrategia solicitada esté anunciada y no lanza instalaciones por su cuenta. Los comandos deben provenir de profile locks cualificados por P099.

El probe carga PyTorch/Transformers sólo si `sys.prefix` coincide con `LARM_PYTHON_ROOT` y `sys.executable` está dentro del mismo prefijo; rechaza intérpretes enlazados o junctions. La inspección se limita a modelos locales dentro de `LARM_TRANSFORMERS_MODEL_ROOT`. La inferencia fija backend, `local_files_only`, `use_safetensors` y `trust_remote_code=false`, y devuelve `REMOTE_CODE_REQUIRED` antes de importar Transformers si hay `auto_map`. El sidecar define un subset OpenAI de texto autenticado, sin streaming/herramientas, y sólo puede enlazar loopback. El worker no lo anuncia ni lo inicia antes de que el host P022/P040 lo cualifique.

Verificación focal: 4 PASS, 0 FAIL, 1 BLOCKED. Las 13 pruebas Python usan módulos simulados para probar controles; las 5 pruebas Rust cubren identidad, hashing compartido con el worker, store path y validación. El probe real del host confirma que el Python de herramientas no se acepta como runtime. No se instaló PyTorch, Transformers ni ningún paquete en Python global.

La puerta `tools/check.ps1 -Prompt P071 -Real` figura como **INTERRUMPIDA (cargo_test workspace no cerró tras más de diez minutos; no hay resultado oficial)**; no se generó `check_P071_real.json`. La inferencia tiny real y las estrategias UV/CONDA/MAMBA siguen bloqueadas porque P031/P022/P040 no tienen `REAL_PASS`, P099 no ha cualificado comandos y locks del entorno, y el host no cuenta con runtime gestionado ni modelo local autorizado. El trabajo no se anuncia como adaptador listo para producción.
