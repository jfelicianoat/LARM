# P142 — Cobertura normativa y manifiesto de evidencia por requisito

Fecha: 2026-11-15.  
Estado: **BLOCKED_REAL** — el verificador, los 15 escenarios preparados y el harness REAL están completos y PASS; el REAL_PASS de la fase exige el conjunto completo de prompts en REAL_PASS y los 15 escenarios ejecutados realmente (exigencia de P138), que hoy no es el estado del árbol (73 evidencias BLOCKED_REAL, 54 NOT_STARTED).

## Implementación

- **Verificador** (`tools/requirements_matrix.py`): `build` genera el manifiesto determinista `docs/reports/requirements-matrix.json` (ordenado, RFC-ish) con una fila por requisito del contrato sellado `acceptance.json` — **127 prompts** (`required_prompt_ids`), **20 runtimes**, **5 familias de plugins**, **34 entidades**, **14 pantallas**, **14 tipos de fuente** y **15 escenarios globales** (215 filas de catálogo + escenario). Cada fila de prompt lleva el estado sellado del DTO `PromptEvidence` (los 4 valores del enumerado; `NOT_STARTED` es estado a nivel matriz cuando no existe evidence.json), el SHA-256 del evidence.json, el host y los contadores por clase de prueba; cada fila de catálogo lleva sus prompts propietarios y el peor estado derivado. `check` recalcula y exige igualdad byte a byte con el manifiesto almacenado más las **seis reglas de fallo** del prompt:
  1. evidencia **borrada** → la fila cambia y el manifiesto deja de corresponder (`matrix_drift`);
  2. **hash alterado** → `matrix_drift` (igualdad byte a byte contra el almacenado);
  3. **estado desconocido** → `unknown_status::<id>::UNKNOWN::<valor>` (sólo los 4 sellados son válidos);
  4. **omitir NCNN** (u otro runtime requerido) → `required_runtime_omitted::NCNN`, con el ancla obligatoria interna del verificador (detecta incluso un acceptance.json tamperado);
  5. **mock etiquetado REAL** → `mock_labeled_real`: una entrada kind=REAL cuyo `observed` es el resumen de una suite en proceso (`Ran N tests…`/`test result:…`) es una UNIT/CONTRACT reetiquetada;
  6. **clausura incompleta** → `closure_incomplete::<id>`: un prompt en REAL_PASS con alguna dependencia del manifiesto fuera de REAL_PASS.
  Además detecta evidencias huérfanas (ficheros .evidence.json cuyo prompt ya no está en el contrato) y evidencias ilegibles.
- **check.ps1 con selección por prompt y escenario**: ya existente (`-Prompt PXXX`, `-Scenario Gxx` con patrón `G\d{2}` que ejecuta `tools/scenarios/<gxx>.py`, BLOCKED exit 2 si el runner no existe); verificado por una prueba de contrato.
- **15 runners de escenario** (`tools/scenarios/g01.py`…`g15.py`), generados determinísticamente desde `acceptance.json`: cada uno reporta id/nombre/procedimiento/oráculo y el estado ACTUAL de sus prompts propietarios leído de los evidence.json; sale 0 sólo con todos en REAL_PASS y 2 (BLOCKED) mientras no — sin duplicar la ejecución real que cada propietario ya probó ni confundir CI o mocks con el entorno del PC IA.

## Pruebas

- **Negativos del verificador** (`tools/test_p142_requirements_matrix.py`, **7/7 PASS**): los seis oráculos negativos del prompt + el caso íntegro sin violaciones estructurales. Cada caso tampera una copia aislada (árbol temporal con el acceptance copiado) y exige el motivo exacto de fallo.
- **Runner Python completo**: 8 suites OK (incluida la nueva).
- **REAL sobre el PC IA**: `requirements_matrix.py check` recalcula el manifiesto contra las 73 evidencias vigentes → PASS con la distribución exacta (73 BLOCKED_REAL, 54 NOT_STARTED — reportada sin maquillar); runner G01 reporta P031/P060 BLOCKED_REAL (con la evidencia real de los dos builds de llama.cpp) y P032 NOT_STARTED, salida 2.
- **Puerta formal**: `tools/check.ps1 -Prompt P142 -Real` → **STATUS PASS (11 checks, 0 fallos)** (fmt, clippy workspace, fixtures, `cargo test --workspace` completo, arquitectura, copias 3.15.0, suites Python, shell Tauri, frontend y `p142_evidence.py` con los 4 CONTRACT + 2 REAL en PASS).

## Pendientes (bloqueos reales)

- El **REAL_PASS de P142** exige que el conjunto completo de los 127 prompts alcance REAL_PASS y que los 15 escenarios se ejecuten realmente en el PC IA (P138 exige ese conjunto); con 54 requisitos NOT_STARTED y la mayoría BLOCKED_REAL, la fase queda honestamente **BLOCKED_REAL**: el verificador reporta esa distribución sin maquillarla y es la herramienta que P138 usará para exigir el cierre.
