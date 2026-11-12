# P093 — cuantización real Q5_K (super-bloques K de 5 bits) en el conversor GGUF de producto: diseño

Fecha: 2026-11-09. Séptima fase de conversores: el enum pasa a `["Q8_0","Q4_0","Q5_0","Q4_K","Q5_K"]`. Q5_K (type 13) comparte TODA la maquinaria ya espejada de Q4_K — `make_qkx2_quants`, `nearest_int`, `get_scale_min_k4`, el empaquetado 6-bit de `scales[12]`, la re-cuantización final y los guards — cambiando sólo los parámetros de la búsqueda y los niveles de 5 bits con su bit alto en `qh`.

## Diferencias exactas respecto de Q4_K (contra la referencia de ggml)

1. **Búsqueda**: `make_qkx2_quants(32, 31, x, weights, L, &min, Laux, -0.5f, 0.1f, 15, false)` — nmax=31 (no 15), rmin=−0.5 (no −1.0), nstep=15 (no 20).
2. **Re-cuantización final**: `L = clamp(nearest_int((x+dm)/d), 0..31)` (no 0..15).
3. **Empaquetado por grupos de 64** (misma geometría de mitades que Q4_K): byte `qs[32·chunk + j] = (L[n+j] & 0xF) | ((L[n+j+32] & 0xF) << 4)`, con `n = 64·chunk`; si el nivel era >15 se resta 16 y su bit alto va a `qh[j]` — elemento `n+j` → bit `2·chunk`, elemento `n+j+32` → bit `2·chunk+1` (`qh` de 32 bytes, cero inicial, máscaras m1=1<<2·chunk y m2=2<<2·chunk).
4. **Bloque**: `d` f16 + `dmin` f16 + `scales[12]` + `qh[32]` + `qs[128]` = **176 bytes**. `d=f16(max_scale/63)`, `dmin=f16(max_min/63)` idénticos a Q4_K.
5. Dequant semántica: `v ≈ d·sc·L − dmin·m` con `L ∈ [0,31]`; cota del oráculo focal como la de Q4_K (máximo del sub-bloque + 0.6·d·sc + épsilon del dmin).
6. Regla por tensor: F32/F16 con total de elementos múltiplo de QK_K=256; guards heredados cerrados.

## Integración (reutilización directa)

- El fixture K (F32 [256], 8 sub-bloques variados) sirve igual para el golden Q5_K — sin fixtures nuevos.
- Paquete: `quantize_q5_k_block` + despacho (176 bytes, type 13); enum ampliado.
- Suite host: espejo Rust directo (make_qkx2_quants ya existe; sólo el bloque Q5_K y su despacho), golden `QUANT5K_GOLDEN_GGUF_HEX`, caso positivo de suite.
- Aceptación pública: QUINTO ciclo (counts [5,6,0,0]).
- Auditor: re-derivación de los cinco formatos (98.304×4 simples + 12.288×2 K).
- El negativo de enum inválido sigue siendo `Q6_K` (entra Q5_K, queda pendiente Q6_K).
