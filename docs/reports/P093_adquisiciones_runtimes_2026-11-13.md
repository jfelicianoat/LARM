# P093 — adquisición de dependencias externas: Olive, llama.cpp y Ollama

Fecha: 2026-11-13. Ejecución del objetivo del operador: instalar y sellar el runtime Olive, compilar llama.cpp, y registrar la instalación de Ollama. Las tres adquisiciones quedan registradas en `environment.lock.json` (`extensions`) con su versión, fuente, ruta y sello.

## 1. Runtime gestionado python-olive (P065)

- **Venv dedicado** `.venv-olive` (Python 3.14.6, compatible con el lock): `olive-ai==0.13.0` + `torch 2.15.0+cpu` + `onnx 1.23.2` + `onnxruntime 1.32.0` + `requests` y 52 dependencias más (**57 paquetes**, PyPI + índice CPU de PyTorch).
- **Función verificada**: import de los cuatro módulos OK; cuantización dinámica int8 de un modelo ONNX de prueba (MatMul F32 64×128) — 32.866 bytes → 8.776 bytes (‑73%), error máximo vs F32 0.0258, initializers int8+scales; y **el flujo Olive real** (`workflows.run` con el pass `OnnxDynamicQuantization`, `LocalSystem` CPU) produce el modelo cuantizado con su footprint.
- **Sellado**: `pip freeze` de 67 versiones exactas en `adapters/transformation/olive-runtime.lock`; manifiesto de sellado con **sha256 por archivo (26.872 archivos)** en el runtime: `runtime-manifest.json`.
- **Ruta gestionada** (decisión del operador, modelo por ruta): `C:\Desarrollo\runtimes\python-olive-0.13.0\` — copiada del venv y sellada en el lugar definitivo.

## 2. llama.cpp compilado (P060)

- Fuente: `https://github.com/ggml-org/llama.cpp` clon shallow (commit `b79696f`) en `C:\Desarrollo\llama.cpp`.
- Toolchain: **MSVC 19.29.30159** (VS2019 BuildTools, `cl.exe` x64) + Windows SDK 10.0.26100.0 + **Ninja 1.13.2** y **CMake 4.4.4** instalados por pip en `.venv-tools`.
- Compilación: 190/190 targets (`GGML_CUDA=OFF`, `LLAMA_CURL=OFF`), flags Release.
- **Binarios verificados**: `llama-cli.exe` (`version: 1.0.0-dev (build 1, commit b79696f), built with MSVC 19.29.30159.0 for Windows AMD64`), `llama-quantize.exe`, `llama-server.exe` en `C:\Desarrollo\llama.cpp\build\bin`.

## 3. Ollama del operador (P099)

- **Ollama 0.40.16** detectado y verificado en `C:\Users\pcesp\AppData\Local\Programs\Ollama\ollama.exe` (el lock histórico registraba 0.40.2).

## Registro

`environment.lock.json` `detected_on_host.ollama` actualizado a 0.40.16 y tres entradas nuevas en `extensions` (P065 python-olive, P060 llama.cpp, P060 ollama) con versión, fuente, ruta y sello. Las rutas de runtimes se expresan con `/` (sin escapes de Windows).

## Siguiente fase (P065)

Con el runtime sellado, la fase P065 continúa con el paquete del cuantizador: worker de cuantización ONNX→ONNX-INT8 vía Olive (pass `OnnxDynamicQuantization`), ledger privado derivado del GGUF/ONNX, oráculo focal, suite del host con oráculo en Rust, aceptación pública y validación congelada. El modelo de empaquetado es **runtime gestionado por ruta** (decisión del operador): el paquete declara la dependencia por lock y el worker consume la ruta sellada.
