# llama.cpp en LARM

P060 requiere un adaptador oficial de Windows que inventaríe builds paralelos,
capture su identidad completa y observe los dispositivos que cada build declara.
El helper actual [`probe.py`](../../adapters/runtime/llama-cpp/probe.py) cubre
sólo la fase de descubrimiento y observación.

## Comportamiento actual

`probe.py --root <ruta-configurada>` inspecciona la raíz indicada y sus
subdirectorios directos. Dentro de cada candidato busca las carpetas conocidas
de binarios de Windows, sin recorrer el disco ni seguir directorios hijos que
resuelvan fuera de la raíz configurada. Detecta `llama-server`, `llama-cli`,
`llama-bench`, `llama-quantize`, `llama-gguf-split` y los conversores GGUF
conocidos.

Para cada build, el proceso ejecuta `--version` y `--help` en la herramienta
principal. Sólo ejecuta `--list-devices` cuando `llama-cli --help` anuncia esa
opción (o `llama-server` si no existe CLI). Los procesos tienen límite de
tiempo y salida, no usan shell ni entrada interactiva y reciben un entorno
mínimo sin credenciales heredadas. Cada ejecutable, conversor y salida de probe
se liga por SHA-256. Las versiones no interpretables permanecen `UNKNOWN`, y
todo registro queda `UNQUALIFIED`.

La documentación oficial de llama.cpp describe `--version` y `--list-devices`
en la [guía de CLI](https://github.com/ggml-org/llama.cpp/blob/master/tools/cli/README.md).
El árbol oficial incluye [convert_hf_to_gguf.py](https://github.com/ggml-org/llama.cpp/blob/master/convert_hf_to_gguf.py)
y [convert_lora_to_gguf.py](https://github.com/ggml-org/llama.cpp/blob/master/convert_lora_to_gguf.py).
Estas referencias `master` orientan el inventario; no sustituyen la captura de
fuente y documentación fijada por revisión que P099 exige para un profile lock.

## Hosts de ejecución (P081)

La abstracción de hosts de ejecución (revisión contractual 3.15.0) ya observa read-only los motores virtualizados del equipo — WSL2 (`wsl --status/--list --verbose`) y el motor de contenedores (`docker --version/info --format`) — y los expone por `GET /api/v1/execution-hosts` y en la pantalla Runtimes. Un build de llama.cpp para Linux bajo WSL2 se registrará con estrategia `WSL` y `execution_host_id` del distro observado, nunca como runtime Windows nativo; los comandos que instalen distros, habiliten features de Windows o muten el estado global del motor se rechazan con `execution_host_plan_mutates_host`. La compatibilidad de un runtime concreto exige su lock cualificado (P099).

## Pendiente de P060

El paquete todavía no implementa el protocolo de adaptadores de LARM ni planes
de instalación side-by-side. No se ha seleccionado un artefacto de Windows ni
un backend a partir de evidencia upstream fijada. La máquina no tiene
`llama-server` ni `llama-cli` en el inventario/PATH actual, así que siguen
pendientes la adopción real, la prueba de dos versiones, los probes de backend
con builds reales y los gates completos de P060. Dependencias P025, P031, P032,
P011 y P099 conservan estados distintos de `REAL_PASS`.
