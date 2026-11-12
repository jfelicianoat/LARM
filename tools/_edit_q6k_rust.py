# -*- coding: utf-8 -*-
"""Edita gguf.rs: espejo Rust de Q6_K. Uso único durante la fase Q6_K."""
import io

p = r'crates/larm-adapter-host/src/conformance/gguf.rs'
s = io.open(p, encoding='utf-8').read()

# 1) constantes
old = "const GGML_Q5_K: u32 = 13;\nconst Q5_K_BLOCK_BYTES: usize = 2 + 2 + K_SCALE_SIZE + QK_K / 8 + QK_K / 2;"
new = "const GGML_Q5_K: u32 = 13;\nconst Q5_K_BLOCK_BYTES: usize = 2 + 2 + K_SCALE_SIZE + QK_K / 8 + QK_K / 2;\nconst GGML_Q6_K: u32 = 14;\nconst Q6_K_BLOCK_BYTES: usize = 2 + QK_K / 16 + QK_K / 2 + QK_K / 4;"
assert s.count(old) == 1
s = s.replace(old, new)

# 2) allowlist
old = '    if !(quant == Some("Q8_0")\n        || quant == Some("Q4_0")\n        || quant == Some("Q5_0")\n        || quant == Some("Q4_K")\n        || quant == Some("Q5_K"))\n    {'
new = '    if !(quant == Some("Q8_0")\n        || quant == Some("Q4_0")\n        || quant == Some("Q5_0")\n        || quant == Some("Q4_K")\n        || quant == Some("Q5_K")\n        || quant == Some("Q6_K"))\n    {'
assert s.count(old) == 1
s = s.replace(old, new)

# 3) regla super-bloques (ancla con el comentario siguiente: única)
old = '    if quant == Some("Q4_K") || quant == Some("Q5_K") {\n        // El super-bloque cubre'
new = '    if quant == Some("Q4_K") || quant == Some("Q5_K") || quant == Some("Q6_K") {\n        // El super-bloque cubre'
assert s.count(old) == 1
s = s.replace(old, new)

# 4) make_qx_quants + quantize_q6_k_block antes de la doc de expected
anchor = "/// Emisión espejo del contenedor con cuantización opcional (Q8_0/Q4_0/Q5_0).\n"
assert s.count(anchor) == 1
insert = "/// Espejo verbatim de make_qx_quants de ggml (rmse_type=1, qw=NULL):\n/// cuantización simétrica con ajuste por mínimos cuadrados ponderados por x².\n/// Cada producto y cada acumulación se redondea a f32 paso a paso, como el C.\nfn make_qx_quants(values: &[f32], nmax: i32) -> (f32, Vec<i32>) {\n    let n = values.len();\n    let mut max_value = 0f32;\n    let mut amax = 0f32;\n    for value in values {\n        let ax = value.abs();\n        if ax > amax {\n            amax = ax;\n            max_value = *value;\n        }\n    }\n    if amax < 1e-15f32 {\n        return (0.0, vec![0; n]);\n    }\n    let mut iscale = -nmax as f32 / max_value;\n    let mut levels: Vec<i32> = Vec::with_capacity(n);\n    let mut sumlx = 0f32;\n    let mut suml2 = 0f32;\n    for value in values {\n        let mut l = nearest_int(iscale * value).clamp(-nmax, nmax - 1);\n        levels.push(l + nmax);\n        let w = value * value;\n        sumlx += w * value * l as f32;\n        suml2 += w * (l as f32) * (l as f32);\n    }\n    let mut scale = if suml2 != 0.0 { sumlx / suml2 } else { 0.0 };\n    let mut best = scale * sumlx;\n    for is_step in -9..=9 {\n        if is_step == 0 {\n            continue;\n        }\n        iscale = -(nmax as f32 + 0.1f32 * is_step as f32) / max_value;\n        sumlx = 0.0;\n        suml2 = 0.0;\n        for value in values {\n            let l = nearest_int(iscale * value).clamp(-nmax, nmax - 1);\n            let w = value * value;\n            sumlx += w * value * l as f32;\n            suml2 += w * (l as f32) * (l as f32);\n        }\n        if suml2 > 0.0 && sumlx * sumlx > best * suml2 {\n            levels.clear();\n            for value in values {\n                let l = nearest_int(iscale * value).clamp(-nmax, nmax - 1);\n                levels.push(l + nmax);\n            }\n            scale = sumlx / suml2;\n            best = scale * sumlx;\n        }\n    }\n    (scale, levels)\n}\n\n/// Un super-bloque Q6_K (type 14) desde 256 elementos F32/F16: 210 bytes\n/// (d f16 + scales int8[16] + ql[128] + qh[64]). 16 sub-bloques de 16\n/// elementos con cuantización SIMÉTRICA (make_qx_quants, niveles [-32,31]\n/// desplazados a [0,63]), escalas de 8 bits con signo y empaquetado de 6\n/// bits. Espejo verbatim de la referencia ggml.\n#[allow(clippy::unwrap_used)] // las rebanadas tienen longitud verificada antes de cada try_into\nfn quantize_q6_k_block(source_bytes: &[u8], offset: usize, dtype: &str) -> Vec<u8> {\n    let read = |index: usize| -> f32 {\n        if dtype == \"F32\" {\n            f32::from_le_bytes(\n                source_bytes[offset + index * 4..offset + index * 4 + 4]\n                    .try_into()\n                    .unwrap(),\n            )\n        } else {\n            f16_bytes_to_f32(&source_bytes[offset + index * 2..offset + index * 2 + 2])\n        }\n    };\n    let values: Vec<f32> = (0..QK_K).map(read).collect();\n    let mut sub_scales = [0f32; 16];\n    let mut l_all = vec![0i32; QK_K];\n    let mut max_scale = 0f32;\n    for sub in 0..16 {\n        let chunk = &values[16 * sub..16 * sub + 16];\n        let (scale, levels) = make_qx_quants(chunk, 32);\n        sub_scales[sub] = scale;\n        l_all[16 * sub..16 * sub + 16].copy_from_slice(&levels);\n        if scale > max_scale {\n            max_scale = scale;\n        }\n    }\n    let mut block = Vec::with_capacity(Q6_K_BLOCK_BYTES);\n    if max_scale < 1e-15f32 {\n        block.extend_from_slice(&0u16.to_le_bytes());\n        block.extend_from_slice(&[0u8; 16]);\n        block.extend_from_slice(&[0u8; QK_K / 2]);\n        block.extend_from_slice(&[0u8; QK_K / 4]);\n        return block;\n    }\n    let iscale = -128.0f32 / max_scale;\n    block.extend_from_slice(&f32_to_f16_bits(1.0f32 / iscale).to_le_bytes());\n    let mut scales = [0i32; 16];\n    for j in 0..16 {\n        scales[j] = nearest_int(iscale * sub_scales[j]).min(127);\n    }\n    for scale in &scales {\n        block.push(*scale as u8);\n    }\n    let d_block = f16_bytes_to_f32(&block[0..2]);\n    for j in 0..16 {\n        let d_sub = d_block * scales[j] as f32;\n        if d_sub == 0.0 {\n            continue;\n        }\n        for ii in 0..16 {\n            let index = 16 * j + ii;\n            let l = nearest_int(values[index] / d_sub).clamp(-32, 31);\n            l_all[index] = l + 32;\n        }\n    }\n    let mut ql = [0u8; QK_K / 2];\n    let mut qh = [0u8; QK_K / 4];\n    for j in (0..QK_K).step_by(128) {\n        let base = j / 128;\n        for l in 0..32 {\n            let q1 = (l_all[j + l] & 0xF) as u8;\n            let q2 = (l_all[j + l + 32] & 0xF) as u8;\n            let q3 = (l_all[j + l + 64] & 0xF) as u8;\n            let q4 = (l_all[j + l + 96] & 0xF) as u8;\n            ql[base * 64 + l] = q1 | (q3 << 4);\n            ql[base * 64 + l + 32] = q2 | (q4 << 4);\n            qh[base * 32 + l] = (((l_all[j + l] >> 4) & 3) as u8)\n                | ((((l_all[j + l + 32] >> 4) & 3) as u8) << 2)\n                | ((((l_all[j + l + 64] >> 4) & 3) as u8) << 4)\n                | ((((l_all[j + l + 96] >> 4) & 3) as u8) << 6);\n        }\n    }\n    block.extend_from_slice(&ql);\n    block.extend_from_slice(&qh);\n    block\n}\n\n" + anchor
s = s.replace(anchor, insert)

# 5) despacho por formato
old = '    let (block_bytes, emitted_type) = match quant {\n        Some("Q4_0") => (Q4_0_BLOCK_BYTES, GGML_Q4_0),\n        Some("Q5_0") => (Q5_0_BLOCK_BYTES, GGML_Q5_0),\n        Some("Q4_K") => (Q4_K_BLOCK_BYTES, GGML_Q4_K),\n        Some("Q5_K") => (Q5_K_BLOCK_BYTES, GGML_Q5_K),\n        _ => (Q8_0_BLOCK_BYTES, GGML_Q8_0),\n    };'
new = '    let (block_bytes, emitted_type) = match quant {\n        Some("Q4_0") => (Q4_0_BLOCK_BYTES, GGML_Q4_0),\n        Some("Q5_0") => (Q5_0_BLOCK_BYTES, GGML_Q5_0),\n        Some("Q4_K") => (Q4_K_BLOCK_BYTES, GGML_Q4_K),\n        Some("Q5_K") => (Q5_K_BLOCK_BYTES, GGML_Q5_K),\n        Some("Q6_K") => (Q6_K_BLOCK_BYTES, GGML_Q6_K),\n        _ => (Q8_0_BLOCK_BYTES, GGML_Q8_0),\n    };'
assert s.count(old) == 1
s = s.replace(old, new)

old = '                if quant == Some("Q4_K") || quant == Some("Q5_K") {'
new = '                if quant == Some("Q4_K") || quant == Some("Q5_K") || quant == Some("Q6_K") {'
assert s.count(old) == 1
s = s.replace(old, new)

old = '            let step = if quant == Some("Q4_K") || quant == Some("Q5_K") {\n                QK_K\n            } else {\n                QK8_0\n            } * width;'
new = '            let step = if quant == Some("Q4_K")\n                || quant == Some("Q5_K")\n                || quant == Some("Q6_K")\n            {\n                QK_K\n            } else {\n                QK8_0\n            } * width;'
assert s.count(old) == 1
s = s.replace(old, new)

old = '                let block = match quant {\n                    Some("Q4_0") => quantize_q4_0_block(raw, offset, &tensor.dtype),\n                    Some("Q5_0") => quantize_q5_0_block(raw, offset, &tensor.dtype),\n                    Some("Q4_K") => quantize_q4_k_block(raw, offset, &tensor.dtype),\n                    Some("Q5_K") => quantize_q5_k_block(raw, offset, &tensor.dtype),\n                    _ => quantize_q8_0_block(raw, offset, &tensor.dtype),\n                };'
new = '                let block = match quant {\n                    Some("Q4_0") => quantize_q4_0_block(raw, offset, &tensor.dtype),\n                    Some("Q5_0") => quantize_q5_0_block(raw, offset, &tensor.dtype),\n                    Some("Q4_K") => quantize_q4_k_block(raw, offset, &tensor.dtype),\n                    Some("Q5_K") => quantize_q5_k_block(raw, offset, &tensor.dtype),\n                    Some("Q6_K") => quantize_q6_k_block(raw, offset, &tensor.dtype),\n                    _ => quantize_q8_0_block(raw, offset, &tensor.dtype),\n                };'
assert s.count(old) == 1
s = s.replace(old, new)

io.open(p, 'w', encoding='utf-8', newline='\n').write(s)
print("espejo Rust Q6_K aplicado")
