# P093 — cuantización real Q6_K (super-bloques K de 6 bits, simétrica) en el conversor GGUF de producto

Fecha: 2026-11-10. Octava fase de conversores según el [diseño fijado](P093_cuantizacion_q6_k_diseno_2026-11-10.md): el enum pasa a `["Q8_0","Q4_0","Q5_0","Q4_K","Q5_K","Q6_K"]` — la familia completa de super-bloques K. Q6_K (type 14, 210 bytes: d f16 + scales int8[16] + ql[128] + qh[64]) es el único K con cuantización SIMÉTRICA: usa `make_qx_quants` (no `make_qkx2_quants`), 16 sub-bloques de 16 elementos con niveles [-32,31] desplazados a [0,63], escalas de 8 bits con signo (`iscale=-128/max_scale`, MIN(127) sin clamp inferior), sin dmin, y empaquetado de 6 bits (ql nibbles por mitades cruzadas +64; qh 2 bits altos de 4 elementos por byte).

## Implementación

- **Paquete GGUF v1.1.0** (`gguf.py`): `_make_qx_quants` (espejo verbatim con f32 por operación — búsqueda is∈[-9..9]\{0} maximizando sumlx²/suml2) + `quantize_q6_k_block` con re-cuantización final `L=clamp(x/d,-32,31)+32` y bloque a cero si `max_scale<1e-15`; regla de super-bloques compartida (total múltiplo de 256) y guards heredados cerrados.
- **Oráculo focal** (10 pruebas): referencia Q6_K independiente + contenedor publicado byte-exacto + cota semántica con reconstrucción completa del nivel de 6 bits desde ql/qh.
- **Suite del host** (14 pruebas del módulo): `make_qx_quants` en Rust + bloque Q6_K + despacho; golden `QUANT6K_GOLDEN_GGUF_HEX` del fixture K reutilizado reproducido byte a byte a la PRIMERA (sin iteración de calibración — la lección de contraste externo de las fases previas aplicada desde el diseño).
- **Aceptación pública SEXTUPLE**: seis ciclos completos — counts [6,7,0,0], variantes de 3.34/1.77/2.16/1.77/2.16/2.58 MB publicadas por CAS, cada contenedor byte-exacto contra su emisión espejo. Negativos: 404 / 422 (Q2_K, el nuevo valor no soportado tras entrar Q6_K) / 201.

## Defectos y calibraciones de la fase

1. **Negativo de enum rotado** de `Q6_K` a `Q2_K` (focal, suite y test público) al entrar Q6_K al enum — misma rotación periódica de las fases previas.
2. **TTL de la aprobación de prueba 60→120 min**: la sesión sxtuple completa (sonda de cualificación con tres conversiones K + seis ciclos públicos) superó la hora bajo la degradación acumulada de la máquina (el intento 1 de la aceptación falló con `grant_expired` al llegar al ciclo Q6_K tras 61 min).
3. El bucle de escalas de 8 bits usa `float(scales[j])` con interpretación con signo en la re-cuantización (el byte guarda el módulo; el valor puede ser negativo).

## Validación congelada

[attempt16](P093_runs/public-gguf-quant-20261108/attempt16/verification-synopsis.json) — **516 pruebas PASS / 0 FAIL / 0 ignoradas, 34/34 comprobaciones**, auditor independiente PASS que re-deriva en Python **331.776 bloques** (98.304 por cada formato simple + 12.288 super-bloques por cada uno de los tres K, formatos verificados {2,6,8,12,13,14}) sobre **13.789.776 bytes** de contenedores cuantizados retenidos. Attempt15 conservado como FAIL: verify 34/34 PASS pero el auditor re-derivó los super-bloques Q6_K con el stride viejo (el `qk` del bucle de desempaquetado no incluía el type 14 — mismo defecto de doble variable del fix anterior; su arreglo se validó con simulación `attempt15-sim/` PASS antes de relanzar). El intento 1 de la aceptación sxtuple también se conservó: `grant_expired` al llegar al ciclo Q6_K tras 61 min (TTL de 60 min agotado — subido a 120).

## Límites conservados

Con la familia K completa (Q4_K/Q5_K/Q6_K), los pendientes de cuantización quedan en el lado ONNX (Olive/P065, int8), la aceptación de runtimes reales (bloqueada P060/P099) y la UI. P093 sigue `BLOCKED_REAL`/preparación sin `CODE_PASS` integral ni `REAL_PASS`; contrato 3.13.0 intacto.
