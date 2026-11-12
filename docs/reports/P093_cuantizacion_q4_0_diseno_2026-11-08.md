# P093 — cuantización real Q4_0 en el conversor GGUF de producto: diseño

Fecha: 2026-11-08. Cuarta fase de conversores: el enum de cuantización del conversor GGUF pasa de `["Q8_0"]` a `["Q8_0","Q4_0"]`. Q4_0 es el formato de bloque clásico de ggml (type 2): bloques de QK4_0=32 elementos, 18 bytes por bloque (escala f16 + 16 bytes de nibbles empaquetados 4 bits). Misma arquitectura de la fase Q8_0: espejo Python/Rust con goldens cruzados, oráculo focal independiente y aceptación pública.

## Decisión de alcance

Q4_0 (no Q4_K/Q5_K): es el formato de bloque simple canónico y el siguiente paso natural tras Q8_0; los super-bloques K (escalas de 8 bits por sub-bloque, mínimos de 6 bits) quedan para después. La cuantización ONNX/int8 sigue en Olive/P065.

## Aritmética Q4_0 (contrato determinista, idéntico en Python y Rust)

Referencia clásica de ggml con las convenciones de redondeo ya fijadas en la fase Q8_0:

1. **Escala con signo**: `max` = valor con signo de mayor magnitud (empate: gana el primero, comparación estricta `|v| > |max|`). `d = f32(f64(max) / -8.0)`; `id = d==0 ? 0 : f32(1.0/f64(d))`.
2. **Cuantización por truncado**: `xi = min(15, trunc(f32(f32(v*id) + 8.5)))`. La suma `+8.5` se redondea a f32 ANTES del truncado (misma disciplina de un solo redondeo; el truncado hacia cero es el cast C `(int8_t)`). Por construcción `f32(v*id)+8.5 ∈ [0.5, 16.5]` → `xi ∈ [0..16]` → clamp sólo por arriba a 15.
3. **Empaquetado de nibbles**: 32 cuantizados → 16 bytes; índice par = nibble bajo, impar = nibble alto: `qs[i] = xi0 | (xi1 << 4)`.
4. **Bloque**: `d` (f16 RNE) + `qs[16]` = 18 bytes. Sin cuantizables → contenedor idéntico al verbatim (file_type 0).
5. **Dequantización semántica**: `v ≈ d*(xi - 8)`, error ≤ d/2 + épsilon de la escala f16 (cota verificada en el oráculo focal).
6. No-finitos: mismo guard cerrado de Q8_0 (`gguf_quant_nonfinite`); escala no representable en f16 → `gguf_quant_scale_overflow`.

## Integración (reutiliza la infraestructura de Q8_0)

- **Paquete GGUF v1.1.0**: `gguf.py` con `GGML_TYPE_Q4_0=2`, `QK4_0=32`, `quantize_q4_0_block` y despacho por enum en `stream`/`layout` (el parámetro ya viaja sellado en el plan); `adapter.py` acepta el enum ampliado y `describe` declara `["Q8_0","Q4_0"]`.
- **Suite del host**: `quantize_q4_0_block` en Rust + `expected_gguf_quantized` parametrizado por enum, goldens Q4_0 regenerados desde el escritor Python, caso positivo de suite con Q4_0 (contenedor publicado byte-exacto, weight.big → type 2).
- **Aceptación pública**: el test `transformation_publica_gguf_cuantizacion_q8_0_hasta_publicacion` pasa a ejecutar DOS ciclos completos en la misma sesión de daemon (Q8_0 y Q4_0): dos variantes publicadas (counts [2,3,0,0]), cada contenedor verificado byte-exacto contra su emisión espejo. Sigue siendo UNA comprobación del wrapper (34) — sin coste adicional de validación.
- **Auditor**: re-deriva independientemente los bloques de ambos formatos (98.304 Q8_0 + 98.304 Q4_0) desde el input retenido.
