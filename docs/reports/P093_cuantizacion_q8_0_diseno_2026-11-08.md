# P093 — cuantización real Q8_0 en el conversor GGUF de producto: diseño

Fecha: 2026-11-08. Tercera fase de conversores: el conversor GGUF pasa de copiar tensores verbatim a transformarlos realmente (F32/F16 → Q8_0, ggml type 8) bajo un parámetro explícito del request. El conversor ONNX no cambia en esta fase (la cuantización ONNX/int8 es territorio Olive/P065). Contrato público sin cambios de superficie: `parameters` ya es un objeto libre en `TransformationRequest`; el schema del adaptador pasa a declararlo.

## Decisiones de diseño

1. **Parámetro**: `parameters.quantization`, enum `["Q8_0"]`, opcional. Absente → comportamiento 1.0.0 intacto (copia verbatim). Claves desconocidas o parámetros no-object → `product_gguf_options_not_supported` (igual que hoy). Enum inválido → `product_gguf_quant_unsupported`.
2. **Regla por tensor** (determinista y total, nunca falla): con Q8_0 pedido, cada tensor F32 o F16 cuyo eje rápido `ne[0]` (primera dim del par invertido, última del shape original) es múltiplo de QK8_0=32 se cuantiza a Q8_0 (ggml_type 8); todo lo demás (enteros, BF16, F64, floats no múltiplo) va verbatim. `general.file_type` u32: 7 (MOSTLY_Q8_0) si quedó ≥1 tensor cuantizado, 0 si no.
3. **Aritmética Q8_0** (idéntica en Python y Rust, byte a byte): valores v en f32 (F32 exacto; F16→f32 exacto); por bloque de 32: `amax=max|v|` en f32; `d_f32=amax/127` división f32; `d=f16(d_f32)` round-to-nearest-even; `id = d_f32==0 ? 0 : 1/d_f32` en f32; `q[i]=clamp(round_half_away(v_f32*id en f32), -127, 127)` int8. Emisión: `<e` (2B scale) + 32×`<b`. En Python el producto f32 se redondea con round-trip `struct '<f'` (el producto de dos f32 exactos en double es exacto; un solo redondeo al empacar). En Rust `f32::round` (half-away) coincide con C `roundf`.
4. **Bloques**: `blocks = elementos_totales/32` exactos (garantizado por ne[0]%32==0); bytes del tensor = 34×blocks; alineación a 32 igual que hoy. El estimado del plan sigue siendo declarado por el worker cualificado (invariante upper=final+control intacto; el predicted ya es el tamaño exacto del layout con cuantización).
5. **Versión**: `TOOL_VERSION` y manifest del paquete → **1.1.0** (cambio semántico del contrato del adaptador). Capability `vendor.product.gguf` conserva id; descriptor/fixture hash se recalculan.
6. **Provenance**: ya copia `parameters` del request → el linaje de cuantización queda en el registro de publicación sin campos nuevos.
7. **Fixtures**: el fixture determinista de 3 tensores NO cambia (evita ondas en hashes del descriptor); se añade `build_quantization_fixture()` (F32 [64] cuantizable) para golden y focal. El fixture de conformidad de 12 MB ya ejercita la regla mixta: `weight.big` F32 [983040] → Q8_0; `weight.scale` F16 [4] y `n_positions` I64 → verbatim.
8. **Suite del host**: oráculo Rust espejo (f32→f16 RNE manual + round_half_away), golden cruzado Python→Rust del fixture de cuantización, caso positivo completo (plan/execute/verify con parámetro y contenedor Q8_0 verificado byte a byte), negativos (enum inválido, clave extra), describe con schema nuevo, tool_version 1.1.0.
9. **Aceptación pública**: nuevo test `transformation_publica_gguf_cuantizacion_q8_0_hasta_publicacion` — ciclo completo con `parameters {"quantization":"Q8_0"}`, publicación CAS verificada por el oráculo Rust (ggml_type 8, dims intactos, bloques exactos, kv file_type 7). Será la 34ª comprobación del wrapper congelado.

## Sub-incrementos

1. Paquete GGUF v1.1.0 (ruta Q8_0 + schema + fixtures) + oráculo focal actualizado.
2. Suite del host: oráculo Q8_0 en Rust + golden cruzado + casos nuevos.
3. Aceptación pública cuantizada (y verificación de que el daemon no necesita cambios: parameters ya pasa como objeto libre).
4. Validación congelada (34 checks) + auditor + informe + STATE + memoria.
