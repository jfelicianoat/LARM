# P060 — llama.cpp: descubrimiento, instalación y versiones

Fecha: 2026-11-15  
Estado: **BLOCKED_REAL** (con evidencia real completa de dos builds side-by-side; la publicación MANAGED en runtime store y el paquete del Runtime Adapter Protocol siguen bloqueados por locks sin cualificación)

## Resultado

Dos hitos: el **probe read-only** de instalaciones llama.cpp en rutas configuradas (2026-10-21) y la **adopción real de dos builds side-by-side compilados en el PC IA** (2026-11-15).

El probe enumera las herramientas presentes (`llama-server`, `llama-cli`, `llama-bench`, `llama-quantize`, conversores GGUF), captura `--version` y `--help`, calcula SHA-256 de cada binario y consulta `--list-devices` sólo cuando la ayuda anuncia la opción. No recorre discos, no instala ni inicia runtimes y no declara verificado un backend o versión desconocida.

**Evidencia real de dos versiones** (transcripción completa en `P060_runs/real-probe-both.json`):

| Build | Raíz | version_string | commit | herramientas |
|---|---|---|---|---|
| 1 | `C:/Desarrollo/llama.cpp` | `1.0.0-dev (build 1, commit b79696f)` | `b79696f` | llama-cli/quantize/server + conversores, SHA-256 por binario |
| 2 | `C:/Desarrollo/llama.cpp-b7020` (tag b7020, commit `d2d6269`) | `1` | `d2d6269` | mismas herramientas, hashes distintos |

Ambos builds salen del mismo árbol MSVC 19.29 con commits distintos (fuente adquirida y sellada por P093). El backend probe de ambos reporta lista de dispositivos vacía (builds CPU) con salida hash-eada — observado, no inferido; `qualification` permanece `UNQUALIFIED` y `profile_lock_created=false`: el registro no acredita soporte.

**Mejora del parser** descubierta por la evidencia real: el formato `(build 1, commit b79696f)` dejaba `commit=null` (el patrón original sólo extraía un hash entre paréntesis, no el sufijo `commit <hash>`); ampliado con un tercer patrón `commit[:\s]+<hash>` y verificado contra los dos formatos reales.

## Pruebas y comprobaciones (2026-11-15)

- `tools/test_p060_llama_cpp_probe.py`: **7/7 PASS**.
- `tools/p060_evidence.py` (recibo formal): **BLOCKED_REAL — 3 PASS, 0 FAIL, 2 BLOCKED**. El REAL `P060-REAL-adopt-and-probe` ejecuta el probe contra las dos raíces reales y exige ambos commits observados con hashes, help, backend probe y UNQUALIFIED.
- `tools/check.ps1 -Prompt P060 -Real`: **STATUS PASS (11 checks, 0 fallos)** — verificado en dos corridas (la segunda valida el recibo de la primera: fmt, clippy, fixtures, workspace completo, arquitectura, copias contractuales, suites Python, shell Tauri, frontend y evidencia).

## Pendientes (bloqueos reales)

1. **Publicación MANAGED en runtime store**: la admisión P031 exige un profile lock cualificado (los locks de P099 permanecen UNQUALIFIED) y la selección de artifact por backend exige la evidencia de P011.
2. **Paquete del Runtime Adapter Protocol** para llama.cpp (lanzamiento de llama-server, health, métodos del protocolo): alcance de **P061** (compatibilidad, serving, smoke tests y llama-bench), cuya dependencia P060 queda ahora con evidencia real de dos builds.

Estados permitidos: REAL_PASS exige cualificación real por versión; este incremento se cierra **BLOCKED_REAL** con toda la evidencia ejecutable ejecutada y PASS.
