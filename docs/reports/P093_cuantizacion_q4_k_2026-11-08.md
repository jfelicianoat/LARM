# P093 — cuantización real Q4_K (super-bloques K) en el conversor GGUF de producto

Fecha: 2026-11-08. Sexta fase de conversores según el [diseño fijado](P093_cuantizacion_q4_k_diseno_2026-11-08.md): el enum pasa a `["Q8_0","Q4_0","Q5_0","Q4_K"]`. Q4_K es el primer super-bloque K (type 12): bloques de QK_K=256 elementos → 144 bytes, con 8 sub-bloques de 32 que llevan escala y mínimo propios cuantizados a 6 bits, empaquetados en `scales[12]`, y una re-cuantización final con la escala del super-bloque.

## Implementación

- **Espejo de la referencia oficial de ggml** (`ggml-quants.c`, llama.cpp master): `make_qkx2_quants` (búsqueda iterativa de escala/mínimo con ajuste por mínimos cuadrados ponderados, 21 pasos, `use_mad=false`), `nearest_int` (truco del punto mágico 12582912), `get_scale_min_k4`, el empaquetado exacto de `scales[12]` (6 bits por sub-bloque con partes altas en bits 6-7) y el empaquetado de nibbles por grupos de 64. **Disciplina f32 por operación**: C acumula en f32 con redondeo en cada suma — el espejo Python redondea tras cada producto y cada acumulación (`_f32(acc + _f32(x*x))`); Rust acumula en f32 nativo.
- **Paquete GGUF v1.1.0** (`gguf.py`): `quantize_q4_k_block` + regla por tensor para super-bloques (total de elementos múltiplo de 256), guards heredados (no-finito y escala f16 no representable → falla cerrado).
- **Fixture K** (`build_quantization_k_fixture`): F32 [256] con 8 sub-bloques de rangos distintos (cero exacto, amplios, estrechos, positivos, negativos, desplazados) para ejercitar todas las ramas de `make_qkx2_quants` y del empaquetado de `scales[12]` (j<4 y j≥4).
- **Oráculo focal** (8 pruebas): referencia Q4_K independiente completa + contenedor publicado byte-exacto + cota semántica por sub-bloque (rigurosa para la ruta `sc==0` que conserva el nivel base de qkx2).
- **Suite del host**: espejo Rust completo — `nearest_int` bit-exact, `make_qkx2_quants` con f32 nativo, bloque Q4_K, regla por super-bloques, despacho por formato — validado byte a byte contra el golden del escritor Python (`QUANTK_GOLDEN_GGUF_HEX`, fixture K replicado byte a byte en Rust). 62 pruebas del crate, caso positivo de suite Q4_K.
- **Aceptación pública CUÁDRUPLE**: cuatro ciclos completos (Q8_0 → Q4_0 → Q5_0 → Q4_K) en la misma sesión — counts [4,5,0,0], variantes de 3.34 / 1.77 / 2.16 / 1.77 MB publicadas por CAS, cada contenedor byte-exacto contra su emisión espejo. Negativos: 404 / 422 (Q6_K, el nuevo valor no soportado tras entrar Q4_K al enum) / 201.

## Defectos y calibraciones de la fase

1. **Timeouts del sistema calibrados a los conversores reales** (la cuantización K del fixture de 12 MB tarda ~140 s por pasada en Python stdlib — el algoritmo K es intrínsecamente costoso y la ruta de emisión única lo ejecuta en preflight y en conversión): plan preview RPC del daemon 60→300 s, perfil local de cualificación 120→600 s (techo del executor 600), deadline del test para la sonda 360→900 s, deadline del ciclo Q4_K público 120→600 s, HTTP del test 180→300 s, y TTL de la aprobación de prueba 15→60 min (la sesión cuádruple completa supera el cuarto de hora). Todos son detectores de cuelgue, no presupuestos de rendimiento; el execute RPC del daemon ya estaba en 600 s.
2. **Negativo de enum rotado**: al entrar Q4_K al enum, el valor no-soportado de los negativos pasa a ser `Q6_K` (test focal, suite host y test público).
3. **Longitud de bloque K**: la regla genérica dividía por 32 — el super-bloque divide por 256 (destapado por la conformidad cruzada del golden).

## Validación congelada

[attempt9](P093_runs/public-gguf-quant-20261108/attempt9/verification-synopsis.json) — **516 pruebas PASS / 0 FAIL / 0 ignoradas, 34/34 comprobaciones**, auditor independiente PASS que re-deriva en Python **307.200 bloques** (98.304 por cada formato simple + 12.288 super-bloques Q4_K, formatos verificados {2,6,8,12}) sobre **9.045.728 bytes** de contenedores cuantizados retenidos, bloque a bloque contra referencias independientes de los cuatro formatos. Attempts 5–8 conservados como FAIL: dos por `larm-daemon.exe` bloqueado por un proceso zombi de corridas exploratorias (el `taskkill /F` fallaba silenciosamente por la conversión de rutas de Git Bash — se necesita `//F`), uno por un bug del propio auditor (el bucle avanzaba en pasos de 32 elementos también para los super-bloques de 256 — detectado y validado su arreglo contra los datos reales de attempt7 mediante una simulación en `attempt7-sim/` antes de relanzar) y uno por la carrera conocida `backend_busy` bajo carga (pasó en el intento siguiente sin cambios).

## Límites conservados

Q5_K/Q6_K (los otros super-bloques K), cuantización ONNX/int8 (Olive/P065), aceptación de runtimes reales (llama.cpp/Ollama — bloqueados por dependencias externas P060/P099) y UI siguen pendientes. P093 sigue `BLOCKED_REAL`/preparación sin `CODE_PASS` integral ni `REAL_PASS`; contrato 3.13.0 intacto.
