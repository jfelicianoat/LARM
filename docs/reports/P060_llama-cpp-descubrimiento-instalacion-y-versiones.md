# P060 — llama.cpp: descubrimiento, instalación y versiones

Fecha: 2026-10-21  
Estado: **BLOCKED**

## Resultado

Se implementó un probe local de solo lectura para inspeccionar instalaciones de llama.cpp en rutas configuradas y directorios binarios inmediatos. El probe enumera las herramientas que existan (`llama-server`, `llama-cli`, `llama-bench`, `llama-quantize`, herramientas GGUF y conversores conocidos), captura `--version` y `--help`, calcula SHA-256 y consulta `--list-devices` únicamente cuando la ayuda declara esa opción. La búsqueda no recorre discos, no instala ni inicia runtimes y no declara como verificado un backend o una versión desconocida.

Esto deja un comienzo verificable para el descubrimiento, pero no satisface P060. En este equipo no se encontraron ejecutables llama.cpp en `PATH` ni una raíz configurada, así que no se pudo ejecutar adopción real ni comparar dos builds. Tampoco están implementados el adaptador oficial empaquetado conforme al Runtime Adapter Protocol, el plan de instalación MANAGED ni el profile lock por versión. P060 queda **BLOCKED**, sin anunciar soporte de backend.

## Pruebas y comprobaciones

- `tools/test_p060_llama_cpp_probe.py`: **7/7 PASS**. Los casos cubren dos builds laterales, límites de búsqueda, versión desconocida, salida ambigua, detección condicional de dispositivos, entorno mínimo sin secretos y raíz inexistente.
- `tools/check.ps1 -Prompt P060 -Real`: **13 PASS, 1 FAIL**, en `package_copy`. Formato, Clippy, fixtures, suite completa del workspace con target aislado, arquitectura, pruebas del escritorio, lint, typecheck, pruebas y build del frontend pasaron.
- El único fallo reproduce tres diferencias ya registradas en el estado del proyecto entre copias contractuales locales y el paquete canónico: `adapter-methods.json`, `api-operations.json` y `schemas/larm.schema.json`. Son ampliaciones locales de operaciones runtime/P099 y estados/campos de esquema. Se preservaron porque forman parte del estado contractual de trabajo y no se deben sobrescribir para hacer verde el control.
- La evidencia P060 se regeneró después del recibo común. Su grupo de puertas comunes refleja el fallo de `package_copy`; por ello la evidencia formal actual registra **1 PASS, 1 FAIL y 2 BLOCKED**, además de los bloqueos de dependencias. La ejecución del runner dentro de `check.ps1` ocurrió antes de que ese script escribiera su recibo final, así que esa corrida marcó la lectura del recibo como pendiente.

Recibo completo: [`check_P060_real.json`](check_P060_real.json). Transcripciones con hashes: [`check_runs/20261021T2115541530574Z`](check_runs/20261021T2115541530574Z). Evidencia focal: [`P060_llama-cpp-descubrimiento-instalacion-y-versiones.evidence.json`](P060_llama-cpp-descubrimiento-instalacion-y-versiones.evidence.json).

## Pendiente para completar P060

1. Resolver contractualmente las diferencias de las tres copias antes de exigir `package_copy` verde.
2. Alcanzar `REAL_PASS` en P025, P031, P032, P011 y P099.
3. Implementar y empaquetar el adaptador llama.cpp bajo el protocolo vigente, el plan MANAGED y el profile lock exacto.
4. Probar en Windows dos builds reales side-by-side, incluyendo versión/build/commit, hashes, herramientas, ayuda y backend observado; dejar cualquier backend sin evidencia como desconocido.

La CLI oficial documenta que `--version` informa versión/build y `--list-devices` enumera dispositivos: [documentación de llama.cpp CLI](https://github.com/ggml-org/llama.cpp/blob/master/tools/cli/README.md). La lista de herramientas observadas se limita a rutas y opciones comprobables de cada build.
