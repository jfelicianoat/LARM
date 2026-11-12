# -*- coding: utf-8 -*-
"""Edita gguf.rs: espejo Rust de Q4_K (nearest_int, make_qkx2_quants, bloque,
despacho y regla). Uso único durante la fase Q4_K."""
import io

p = r'crates/larm-adapter-host/src/conformance/gguf.rs'
s = io.open(p, encoding='utf-8').read()

# 1) constantes
old = (
    "const GGML_Q5_0: u32 = 6;\n"
    "const QK5_0: usize = 32;\n"
    "const Q5_0_BLOCK_BYTES: usize = 2 + 4 + 16;"
)
new = (
    "const GGML_Q5_0: u32 = 6;\n"
    "const QK5_0: usize = 32;\n"
    "const Q5_0_BLOCK_BYTES: usize = 2 + 4 + 16;\n"
    "const GGML_Q4_K: u32 = 12;\n"
    "const QK_K: usize = 256;\n"
    "const K_SCALE_SIZE: usize = 12;\n"
    "const Q4_K_BLOCK_BYTES: usize = 2 + 2 + K_SCALE_SIZE + QK_K / 2;"
)
assert s.count(old) == 1
s = s.replace(old, new)

# 2) regla generalizada
old = (
    'fn quantized_tensor(tensor: &SafetensorsTensor, quant: Option<&str>) -> bool {\n'
    '    (quant == Some("Q8_0") || quant == Some("Q4_0") || quant == Some("Q5_0"))\n'
    '        && (tensor.dtype == "F32" || tensor.dtype == "F16")\n'
    "        && !tensor.shape.is_empty()\n"
    "        && tensor.shape[tensor.shape.len() - 1] > 0\n"
    "        && tensor.shape[tensor.shape.len() - 1].is_multiple_of(QK8_0 as u64)\n"
    "}"
)
new = (
    'fn quantized_tensor(tensor: &SafetensorsTensor, quant: Option<&str>) -> bool {\n'
    '    if !(quant == Some("Q8_0")\n'
    '        || quant == Some("Q4_0")\n'
    '        || quant == Some("Q5_0")\n'
    '        || quant == Some("Q4_K"))\n'
    "    {\n"
    "        return false;\n"
    "    }\n"
    '    if tensor.dtype != "F32" && tensor.dtype != "F16" {\n'
    "        return false;\n"
    "    }\n"
    '    if quant == Some("Q4_K") {\n'
    "        // El super-bloque cubre 256 elementos consecutivos: el total debe\n"
    "        // ser múltiplo de QK_K.\n"
    '        let width = if tensor.dtype == "F32" { 4 } else { 2 };\n'
    "        let elements = tensor.length / width;\n"
    "        return elements > 0 && elements % QK_K == 0;\n"
    "    }\n"
    "    !tensor.shape.is_empty()\n"
    "        && tensor.shape[tensor.shape.len() - 1] > 0\n"
    "        && tensor\n"
    "            .shape[tensor.shape.len() - 1]\n"
    "            .is_multiple_of(QK8_0 as u64)\n"
    "}"
)
assert s.count(old) == 1
s = s.replace(old, new)

# 3) helpers + bloque Q4_K antes de la doc de expected_gguf_quantized
anchor = (
    "/// Emisión espejo del contenedor con cuantización opcional (Q8_0/Q4_0/Q5_0).\n"
    "/// El file_type pasa a 7 (MOSTLY_Q8_0) cuando quedó al menos un tensor\n"
    "/// cuantizado; los lengths y el layout siguen la misma disciplina del\n"
    "/// escritor Python (offsets en una sola pasada, alineación a 32).\n"
    "pub fn expected_gguf_quantized("
)
assert s.count(anchor) == 1
insert = "/// `nearest_int` de ggml: punto mágico — f32(fval+12582912) reinterpretado\n/// como int32; (i & 0x007fffff) − 0x00400000. Bit a bit idéntico al C.\nfn nearest_int(fval: f32) -> i32 {\n    let val = fval + 12582912.0f32;\n    let bits = val.to_bits() as i32;\n    (bits & 0x007F_FFFF) - 0x0040_0000\n}\n\n/// Espejo verbatim de make_qkx2_quants de ggml (use_mad=false): cada producto\n/// y cada acumulación se redondea a f32 paso a paso, como el C.\nfn make_qkx2_quants(\n    values: &[f32],\n    weights: &[f32],\n    nmax: f32,\n    rmin: f32,\n    rdelta: f32,\n    nstep: usize,\n) -> (f32, f32, Vec<usize>) {\n    let n = values.len();\n    let mut minimum = values[0];\n    let mut maximum = values[0];\n    let mut sum_w = weights[0];\n    let mut sum_x = sum_w * values[0];\n    for i in 1..n {\n        if values[i] < minimum {\n            minimum = values[i];\n        }\n        if values[i] > maximum {\n            maximum = values[i];\n        }\n        let w = weights[i];\n        sum_w += w;\n        sum_x += w * values[i];\n    }\n    if minimum > 0.0 {\n        minimum = 0.0;\n    }\n    if maximum == minimum {\n        return (0.0, -minimum, vec![0; n]);\n    }\n    let mut iscale = nmax / (maximum - minimum);\n    let mut scale = 1.0f32 / iscale;\n    let mut best_error = 0.0f32;\n    let mut l_levels = Vec::with_capacity(n);\n    for i in 0..n {\n        let l = nearest_int(iscale * (values[i] - minimum)).clamp(0, nmax as i32) as usize;\n        l_levels.push(l);\n        let diff = scale * l as f32 + minimum - values[i];\n        let w = weights[i];\n        best_error += w * (diff * diff);\n    }\n    if nstep < 1 {\n        return (scale, -minimum, l_levels);\n    }\n    let mut laux = vec![0usize; n];\n    for is_step in 0..=nstep {\n        iscale = (rmin + rdelta * is_step as f32 + nmax) / (maximum - minimum);\n        let mut sum_l = 0.0f32;\n        let mut sum_l2 = 0.0f32;\n        let mut sum_xl = 0.0f32;\n        for i in 0..n {\n            let l = nearest_int(iscale * (values[i] - minimum)).clamp(0, nmax as i32) as usize;\n            laux[i] = l;\n            let w = weights[i];\n            sum_l += w * l as f32;\n            sum_l2 += w * (l as f32) * (l as f32);\n            sum_xl += w * (l as f32) * values[i];\n        }\n        let d = sum_w * sum_l2 - sum_l * sum_l;\n        if d > 0.0 {\n            let mut this_scale = (sum_w * sum_xl - sum_x * sum_l) / d;\n            let mut this_min = (sum_l2 * sum_x - sum_l * sum_xl) / d;\n            if this_min > 0.0 {\n                this_min = 0.0;\n                this_scale = sum_xl / sum_l2;\n            }\n            let mut cur_error = 0.0f32;\n            for i in 0..n {\n                let diff = this_scale * laux[i] as f32 + this_min - values[i];\n                let w = weights[i];\n                cur_error += w * (diff * diff);\n            }\n            if cur_error < best_error {\n                l_levels.copy_from_slice(&laux);\n                best_error = cur_error;\n                scale = this_scale;\n                minimum = this_min;\n            }\n        }\n    }\n    (scale, -minimum, l_levels)\n}\n\n/// Un super-bloque Q4_K (type 12) desde 256 elementos F32/F16: 144 bytes con\n/// 8 sub-bloques de escala y mínimo propios (make_qkx2_quants), escalas de 6\n/// bits en scales[12] y re-cuantización final con la escala del super-bloque.\n/// Espejo verbatim del C de la referencia ggml.\n#[allow(clippy::unwrap_used)] // las rebanadas tienen longitud verificada antes de cada try_into\nfn quantize_q4_k_block(source_bytes: &[u8], offset: usize, dtype: &str) -> Vec<u8> {\n    let read = |index: usize| -> f32 {\n        if dtype == \"F32\" {\n            f32::from_le_bytes(\n                source_bytes[offset + index * 4..offset + index * 4 + 4]\n                    .try_into()\n                    .unwrap(),\n            )\n        } else {\n            f16_bytes_to_f32(&source_bytes[offset + index * 2..offset + index * 2 + 2])\n        }\n    };\n    let values: Vec<f32> = (0..QK_K).map(read).collect();\n    let mut mins = [0f32; 8];\n    let mut sub_scales = [0f32; 8];\n    let mut l_all = vec![0usize; QK_K];\n    let mut max_scale = 0f32;\n    let mut max_min = 0f32;\n    for sub in 0..8 {\n        let chunk = &values[32 * sub..32 * sub + 32];\n        let mut sum_x2 = 0.0f32;\n        for value in chunk {\n            sum_x2 += value * value;\n        }\n        let av_x = (sum_x2 / 32.0).sqrt();\n        let weights: Vec<f32> = chunk.iter().map(|v| av_x + v.abs()).collect();\n        let (scale, the_min, levels) = make_qkx2_quants(chunk, &weights, 15.0, -1.0, 0.1, 20);\n        sub_scales[sub] = scale;\n        mins[sub] = the_min;\n        l_all[32 * sub..32 * sub + 32].copy_from_slice(&levels);\n        if scale > max_scale {\n            max_scale = scale;\n        }\n        if the_min > max_min {\n            max_min = the_min;\n        }\n    }\n    let inv_scale = if max_scale > 0.0 { 63.0f32 / max_scale } else { 0.0 };\n    let inv_min = if max_min > 0.0 { 63.0f32 / max_min } else { 0.0 };\n    let mut ls_values = [0usize; 8];\n    let mut lm_values = [0usize; 8];\n    for j in 0..8 {\n        ls_values[j] = nearest_int(inv_scale * sub_scales[j]).clamp(0, 63) as usize;\n        lm_values[j] = nearest_int(inv_min * mins[j]).clamp(0, 63) as usize;\n    }\n    let mut scales = [0u8; K_SCALE_SIZE];\n    for j in 0..8 {\n        let (ls, lm) = (ls_values[j] as u8, lm_values[j] as u8);\n        if j < 4 {\n            scales[j] = ls;\n            scales[j + 4] = lm;\n        } else {\n            scales[j + 4] = (ls & 0xF) | ((lm & 0xF) << 4);\n            scales[j - 4] |= (ls >> 4) << 6;\n            scales[j] |= (lm >> 4) << 6;\n        }\n    }\n    let mut block = Vec::with_capacity(Q4_K_BLOCK_BYTES);\n    block.extend_from_slice(&f32_to_f16_bits(max_scale / 63.0).to_le_bytes());\n    block.extend_from_slice(&f32_to_f16_bits(max_min / 63.0).to_le_bytes());\n    block.extend_from_slice(&scales);\n    let d_block = f16_bytes_to_f32(&block[0..2]);\n    let dmin_block = f16_bytes_to_f32(&block[2..4]);\n    for j in 0..8 {\n        let (sc, m) = if j < 4 {\n            ((scales[j] & 63) as u32, (scales[j + 4] & 63) as u32)\n        } else {\n            (\n                ((scales[j + 4] & 0xF) | ((scales[j - 4] >> 6) << 4)) as u32,\n                ((scales[j + 4] >> 4) | ((scales[j] >> 6) << 4)) as u32,\n            )\n        };\n        let sub_d = d_block * sc as f32;\n        if sub_d == 0.0 {\n            continue;\n        }\n        let sub_dm = dmin_block * m as f32;\n        for ii in 0..32 {\n            let index = 32 * j + ii;\n            let l = nearest_int((values[index] + sub_dm) / sub_d).clamp(0, 15) as usize;\n            l_all[index] = l;\n        }\n    }\n    let mut qs = [0u8; QK_K / 2];\n    let mut cursor = 0usize;\n    for j in (0..QK_K).step_by(64) {\n        for l in 0..32 {\n            qs[cursor + l] = (l_all[j + l] as u8) | ((l_all[j + l + 32] as u8) << 4);\n        }\n        cursor += 32;\n    }\n    block.extend_from_slice(&qs);\n    block\n}\n\n" + anchor
s = s.replace(anchor, insert)

# 4) despacho por formato
old = (
    "    let (block_bytes, emitted_type) = match quant {\n"
    '        Some("Q4_0") => (Q4_0_BLOCK_BYTES, GGML_Q4_0),\n'
    '        Some("Q5_0") => (Q5_0_BLOCK_BYTES, GGML_Q5_0),\n'
    "        _ => (Q8_0_BLOCK_BYTES, GGML_Q8_0),\n"
    "    };\n"
    "    let lengths: Vec<usize> = document\n"
    "        .tensors\n"
    "        .iter()\n"
    "        .zip(&quantized)\n"
    "        .map(|(tensor, flag)| {\n"
    "            if *flag {\n"
    '                let width = if tensor.dtype == "F32" { 4 } else { 2 };\n'
    "                let elements = tensor.length / width;\n"
    "                (elements / QK8_0) * block_bytes\n"
    "            } else {\n"
    "                tensor.length\n"
    "            }\n"
    "        })\n"
    "        .collect();"
)
new = (
    "    let (block_bytes, emitted_type) = match quant {\n"
    '        Some("Q4_0") => (Q4_0_BLOCK_BYTES, GGML_Q4_0),\n'
    '        Some("Q5_0") => (Q5_0_BLOCK_BYTES, GGML_Q5_0),\n'
    '        Some("Q4_K") => (Q4_K_BLOCK_BYTES, GGML_Q4_K),\n'
    "        _ => (Q8_0_BLOCK_BYTES, GGML_Q8_0),\n"
    "    };\n"
    "    let lengths: Vec<usize> = document\n"
    "        .tensors\n"
    "        .iter()\n"
    "        .zip(&quantized)\n"
    "        .map(|(tensor, flag)| {\n"
    "            if *flag {\n"
    '                let width = if tensor.dtype == "F32" { 4 } else { 2 };\n'
    "                let elements = tensor.length / width;\n"
    '                if quant == Some("Q4_K") {\n'
    "                    (elements / QK_K) * block_bytes\n"
    "                } else {\n"
    "                    (elements / QK8_0) * block_bytes\n"
    "                }\n"
    "            } else {\n"
    "                tensor.length\n"
    "            }\n"
    "        })\n"
    "        .collect();"
)
assert s.count(old) == 1
s = s.replace(old, new)

old = (
    "            for offset in (0..raw.len()).step_by(QK8_0 * width) {\n"
    "                let block = match quant {\n"
    '                    Some("Q4_0") => quantize_q4_0_block(raw, offset, &tensor.dtype),\n'
    '                    Some("Q5_0") => quantize_q5_0_block(raw, offset, &tensor.dtype),\n'
    "                    _ => quantize_q8_0_block(raw, offset, &tensor.dtype),\n"
    "                };\n"
    "                out.extend_from_slice(&block);\n"
    "            }"
)
new = (
    "            let step = if quant == Some(\"Q4_K\") { QK_K } else { QK8_0 } * width;\n"
    "            for offset in (0..raw.len()).step_by(step) {\n"
    "                let block = match quant {\n"
    '                    Some("Q4_0") => quantize_q4_0_block(raw, offset, &tensor.dtype),\n'
    '                    Some("Q5_0") => quantize_q5_0_block(raw, offset, &tensor.dtype),\n'
    '                    Some("Q4_K") => quantize_q4_k_block(raw, offset, &tensor.dtype),\n'
    "                    _ => quantize_q8_0_block(raw, offset, &tensor.dtype),\n"
    "                };\n"
    "                out.extend_from_slice(&block);\n"
    "            }"
)
assert s.count(old) == 1
s = s.replace(old, new)

io.open(p, 'w', encoding='utf-8', newline='\n').write(s)
print("espejo Rust Q4_K aplicado")
