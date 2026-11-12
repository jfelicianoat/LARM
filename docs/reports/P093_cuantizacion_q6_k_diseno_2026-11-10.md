# P093 — cuantización real Q6_K (super-bloques K de 6 bits) en el conversor GGUF de producto: diseño

Fecha: 2026-11-10. Octava fase de conversores: el enum pasa a `["Q8_0","Q4_0","Q5_0","Q4_K","Q5_K","Q6_K"]`. Q6_K (type 14) es el único super-bloque K con cuantización SIMÉTRICA: usa `make_qx_quants` (no `make_qkx2_quants`), sin dmin, con escalas de 8 bits con signo y niveles de 6 bits. Contrato espejo de la referencia oficial de ggml (`ggml-quants.c`, llama.cpp master).

## Algoritmo espejo (contrato determinista, idéntico en Python y Rust)

1. **`make_qx_quants(16, 32, x, L, rmse_type=1, qw=NULL)`** (espejo verbatim; disciplina f32 por operación): `max`/`amax` con comparación estricta (empate: el primero); si `amax < 1e-15f` (GROUP_MAX_EPS) → L=0 y retorno 0; `iscale = f32(-32/max)`; cuantización base `l = clamp(nearest_int(f32(iscale*x[i])), -32, 31)`, `L[i] = l+32`; pesos `w = x[i]*x[i]`; acumulación f32 `sumlx += f32(f32(w*x[i])*l)`, `suml2 += f32(f32(w*l)*l)`; `scale = suml2 ? f32(sumlx/suml2) : 0`; `best = f32(scale*sumlx)`; búsqueda `is ∈ [-9..9] \ {0}`: `iscale = f32(-(f32(32 + 0.1f*is))/max)`, re-acumulación; si `suml2 > 0 && f32(sumlx*sumlx) > f32(best*suml2)` → re-cuantizar L y adoptar `scale/best`. Devuelve `scale`.
2. **Escalas de 8 bits**: `max_scale` = máximo de las 16 sub-escalas (strict, el primero); si `max_abs_scale < 1e-15f` → bloque a cero (d=0, scales=0, L=0, ql/qh=0); `iscale = f32(-128.0f/max_scale)`; `d = f16(1/iscale)` (RNE); `scales[j] = min(127, nearest_int(f32(iscale*scale_j)))` — 8 bits con signo, SIN clamp inferior (byte con módulo).
3. **Re-cuantización final**: por sub-bloque `d_sub = f32(f16→f32(y.d)) * float(int8(scales[j]))`; si `!d_sub` → `continue`; `l = clamp(nearest_int(f32(x/d_sub)), -32, 31); L = l + 32`.
4. **Empaquetado 6 bits** (grupos de 128, l en [0..32)): `ql[l] = (L[j+l]&0xF) | ((L[j+l+64]&0xF)<<4)`, `ql[l+32] = (L[j+l+32]&0xF) | ((L[j+l+96]&0xF)<<4)`; `qh[l] = (L[j+l]>>4) | ((L[j+l+32]>>4)<<2) | ((L[j+l+64]>>4)<<4) | ((L[j+l+96]>>4)<<6)`; avances `ql += 64`, `qh += 32` por grupo.
5. **Bloque**: `d`(2) + `scales[16]`(int8) + `ql[128]` + `qh[64]` = **210 bytes**.
6. Dequant semántica: `v ≈ d·sc·(L−32)`; cota del oráculo focal por sub-bloque como en Q4_K (máximo del sub-bloque + término de redondeo con |d·sc|).
7. Regla por tensor: F32/F16 con total de elementos múltiplo de QK_K=256; guards heredados cerrados (no-finito, escala f16 no representable).

## Integración

- Fixture K reutilizado (sub-bloques de 32 → 2 sub-bloques de 16 cada uno; cubre ramas positivas, negativas y cero).
- Paquete: `_make_qx_quants` + `quantize_q6_k_block` + despacho (210 bytes, type 14).
- Suite host: espejo Rust de `make_qx_quants` (nearest_int ya existe) + bloque + despacho; golden `QUANT6K_GOLDEN_GGUF_HEX`; caso positivo de suite.
- Aceptación pública: SEXTO ciclo (counts [6,7,0,0]).
- Negativo de enum inválido rota de `Q6_K` a `Q2_K` (focal, suite y test público).
- Auditor: re-derivación de los seis formatos (98.304×4 simples + 12.288×3 K).
