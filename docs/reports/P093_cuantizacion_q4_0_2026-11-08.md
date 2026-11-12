# P093 — cuantización real Q4_0 en el conversor GGUF de producto

Fecha: 2026-11-08. Cuarta fase de conversores según el [diseño fijado](P093_cuantizacion_q4_0_diseno_2026-11-08.md): el enum de cuantización del conversor GGUF de producto pasa de `["Q8_0"]` a `["Q8_0","Q4_0"]`. Q4_0 es el formato de bloque clásico de ggml (type 2): bloques de 32 elementos, 18 bytes (escala f16 **con signo** + 16 bytes de nibbles 4 bits). La aceptación pública pasa a ejecutar DOS ciclos completos en la misma sesión de daemon — dos variantes cuantizadas publicadas y verificadas byte a byte.

## Implementación

- **Paquete GGUF v1.1.0** (`gguf.py`): `quantize_q4_0_block` con la aritmética del diseño — escala CON signo `d=f32(max/-8)` (empate: el primero, comparación estricta), `id=f32(1/d)`, cuantización por TRUNCADO del cast C sobre `f32(f32(v·id)+8.5)` (la suma se redondea a f32 antes del truncado), clamp 15, nibbles par-bajo/impar-alto. Misma regla por tensor y guard de no-finitos de Q8_0. `adapter.py` acepta el enum ampliado y `describe` declara `["Q8_0","Q4_0"]` dinámicamente desde `QUANTIZATIONS`.
- **Oráculo focal** (6 pruebas): referencia Q4_0 independiente en el test + contenedor publicado byte-exacto contra ella + cota semántica |v−d·(xi−8)|≤|d| (la escala tiene signo) + negativos.
- **Suite del host**: `quantize_q4_0_block` en Rust (regla `_quantizable` generalizada a ambos formatos), `expected_gguf_quantized` parametrizado (type 2 / 18 bytes por bloque), golden Q4_0 (`QUANT4_GOLDEN_GGUF_HEX`) generado por el escritor Python y reproducido byte a byte por el oráculo Rust, caso positivo completo de suite (plan Q4_0 → execute → contenedor byte-exacto + types + file_type 7). 59 pruebas del crate.
- **Aceptación pública dual**: `transformation_publica_gguf_cuantizacion_q8_0_hasta_publicacion` ejecuta dos ciclos (Q8_0 y luego Q4_0) en la misma sesión: counts [2,3,0,0], variante Q8_0 (3.34 MB) y variante Q4_0 (1.77 MB, 98.304 bloques × 18 bytes) publicadas por CAS, cada contenedor byte-exacto contra su emisión espejo + parseo estructural (weight.big type 8 / type 2 según formato, file_type 7). Negativos: 404 / 422 (Q4_K) / 201.

## Defectos encontrados y corregidos durante la fase

1. **Regla de cuantización no generalizada en el oráculo Rust**: `quantized_tensor` seguía checando `quant == Q8_0` — el contenedor Q4_0 salía verbatim con file_type 0; destapado por la conformidad cruzada del golden.
2. **Check `description-real-tool` con enum viejo**: la suite esperaba `["Q8_0"]` mientras el worker ya declaraba `["Q8_0","Q4_0"]` — primer fallo de la aceptación dual.
3. **Cota semántica con escala con signo**: la referencia del test comparaba contra `d` (negativo cuando max>0) — la cota correcta es `|d|`.

## Validación congelada

[attempt3](P093_runs/public-gguf-quant-20261108/attempt3/verification-synopsis.json) — **516 pruebas PASS / 0 FAIL / 0 ignoradas, 34/34 comprobaciones**, auditor independiente PASS que re-deriva en Python **196.608 bloques** (98.304 Q8_0 + 98.304 Q4_0, formatos verificados {8,2}) sobre 5.112.688 bytes de contenedores cuantizados retenidos, bloque a bloque contra referencias independientes de ambos formatos. [Attempt2](P093_runs/public-gguf-quant-20261108/attempt2/terminal-wrapper.json) conservado como FAIL: el aserto de retención exigía `quant_format` en ambos artefactos y el ciclo Q8_0 aún no etiquetaba el suyo — corregido en la fuente y relanzado.

## Límites conservados

Q8_0/Q4_0 sólo para F32/F16 con eje rápido múltiplo de 32; Q4_K/Q5_K (super-bloques con escalas/mins de 8/6 bits) y cuantización ONNX/int8 (Olive, P065) pendientes, así como aceptación de runtimes y UI. P093 sigue `BLOCKED_REAL`/preparación sin `CODE_PASS` integral ni `REAL_PASS`; contrato 3.13.0 intacto.
