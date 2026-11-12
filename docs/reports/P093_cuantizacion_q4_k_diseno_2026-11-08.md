# P093 — cuantización real Q4_K (super-bloques K) en el conversor GGUF de producto: diseño

Fecha: 2026-11-08. Sexta fase de conversores: el enum pasa a `["Q8_0","Q4_0","Q5_0","Q4_K"]`. Q4_K es el primer super-bloque K (type 12): bloques de QK_K=256 elementos → 144 bytes (`d` f16 + `dmin` f16 + `scales[12]` + `qs[128]`), con 8 sub-bloques de 32 que llevan escala y mínimo propios cuantizados a 6 bits. El contrato se espeja de la referencia oficial de ggml (`ggml-quants.c`, llama.cpp master), aplicando la lección de la fase Q4_0: la interoperabilidad se obtiene del código de referencia externo, no de la auto-consistencia.

## Algoritmo espejo (contrato determinista, idéntico en Python y Rust)

1. **Disciplina f32 por operación**: C acumula en f32 con redondeo en CADA suma. El espejo Python redondea tras cada producto y cada acumulación (`_f32(acc + _f32(x*x))`); Rust acumula en f32 nativo. Ninguna acumulación se computa en f64 y se redondea al final.
2. **Pesos por sub-bloque**: `sum_x2` acumulado f32; `av_x = sqrtf(sum_x2/32)` (raíz f32); `weights[l] = av_x + fabsf(x[l])`.
3. **`nearest_int`**: truco del punto mágico — `val = f32(fval + 12582912.0)`; `i = bits(val)` reinterpretados como int32; `(i & 0x007fffff) - 0x00400000`. Reproducible bit a bit vía struct en Python.
4. **`make_qkx2_quants(32, 15, x, weights, L, &the_min, Laux, rmin=-1.0, rdelta=0.1, nstep=20, use_mad=false)`** (espejo verbatim de la referencia): min/max con primer elemento, `sum_w`/`sum_x` ponderados acumulados f32; `min = min(min, 0)`; si `max==min` → L=0, the_min=-min, retorno 0; `iscale = f32(15/(max-min))`, `scale = f32(1/iscale)`; baseline: `L[i] = clamp(nearest_int(f32(iscale*f32(x[i]-min))), 0..15)`, `best_error` acumulado con `diff*scale+min-x[i]` al cuadrado ponderado; búsqueda `is ∈ [0..20]`: `iscale = f32((f32(-1+0.1*is) + 15)/(max-min))`, acumulaciones `sum_l/sum_l2/sum_xl` ponderadas f32, `D = f32(f32(sum_w*sum_l2) - f32(sum_l*sum_l))`; si `D>0`: `this_scale = f32(f32(sum_w*sum_xl)-f32(sum_x*sum_l))/D`, `this_min = f32(f32(sum_l2*sum_x)-f32(sum_l*sum_xl))/D`; si `this_min>0` → `this_min=0`, `this_scale = sum_xl/sum_l2`; error candidato acumulado; si `cur_error < best_error` (comparación estricta) → adoptar L/escala/min.
5. **Escalas de super-bloque**: `max_scale`/`max_min` como máximos de las 8 sub-escalas; `inv_scale = f32(63/max_scale)` (0 si cero); `ls = min(63, nearest_int(f32(inv_scale*scales[j])))`; `lm` análogo. Empaquetado `scales[12]` exacto: j<4 → `scales[j]=ls`, `scales[j+4]=lm`; j≥4 → `scales[j+4]=(ls&0xF)|((lm&0xF)<<4)`, `scales[j-4] |= (ls>>4)<<6`, `scales[j] |= (lm>>4)<<6`. `d = f16(max_scale/63)`, `dmin = f16(max_min/63)` (RNE).
6. **Re-cuantización final con escala super-bloque**: por sub-bloque `get_scale_min_k4` (6-bit sc/m), `d = f32(f16→f32(y.d))*sc`, `dm` análogo; `L = clamp(nearest_int(f32((x+dm)/d)), 0..15)` (sub-bloque con d==0 → L sin cambios de esa pasada, `continue`).
7. **Empaquetado qs**: grupos de 64: `q[l] = L[j+l] | (L[j+l+32] << 4)` para l en [0..32), j en {0,64,128,192} — empaquetado de mitades dentro de cada grupo de 64.
8. **Guards** heredados: bloque no finito → falla cerrado; escala no representable en f16 → falla cerrado.

## Regla por tensor y fixtures

- Regla: F32/F16 con **total de elementos múltiplo de QK_K=256** se cuantiza a Q4_K; resto verbatim (el super-bloque cubre 256 elementos consecutivos).
- El fixture de cuantización de 64 elementos no es divisible: se añade `build_quantization_k_fixture()` (F32 [256]: 8 sub-bloques con rangos distintos — cero exacto, rangos amplios y estrechos — para ejercitar todas las ramas de `scales[12]`, incluidos j<4 y j≥4).
- El fixture de conformidad (weight.big F32 con 3.145.728 = 256×12.288 elementos) SÍ es divisible → cuantiza en la ruta real; weight.scale/n_positions verbatim.

## Integración

- Suite del host: espejo Rust completo (nearest_int bit-exact, make_qkx2_quants, packing) + golden Q4_K del fixture K + caso positivo de suite.
- Aceptación pública: cuarto ciclo (Q4_K) en el mismo test — counts [4,5,0,0].
- Auditor: re-derivación independiente (98.304 super-bloques Q4_K + los tres formatos simples).
- Wrapper congelado: mismas 34 comprobaciones con asertos actualizados a cuatro formatos.
