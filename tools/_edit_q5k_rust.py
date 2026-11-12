# -*- coding: utf-8 -*-
"""Edita gguf.rs: espejo Rust de Q5_K. Uso único durante la fase Q5_K."""
import io

p = r'crates/larm-adapter-host/src/conformance/gguf.rs'
s = io.open(p, encoding='utf-8').read()

# 1) constantes
old = (
    "const GGML_Q4_K: u32 = 12;\n"
    "const QK_K: usize = 256;\n"
    "const K_SCALE_SIZE: usize = 12;\n"
    "const Q4_K_BLOCK_BYTES: usize = 2 + 2 + K_SCALE_SIZE + QK_K / 2;"
)
new = (
    "const GGML_Q4_K: u32 = 12;\n"
    "const QK_K: usize = 256;\n"
    "const K_SCALE_SIZE: usize = 12;\n"
    "const Q4_K_BLOCK_BYTES: usize = 2 + 2 + K_SCALE_SIZE + QK_K / 2;\n"
    "const GGML_Q5_K: u32 = 13;\n"
    "const Q5_K_BLOCK_BYTES: usize = 2 + 2 + K_SCALE_SIZE + QK_K / 8 + QK_K / 2;"
)
assert s.count(old) == 1
s = s.replace(old, new)

# 2) regla por super-bloques compartida
old = (
    '    if quant == Some("Q4_K") {\n'
    "        // El super-bloque cubre 256 elementos consecutivos: el total debe\n"
    "        // ser múltiplo de QK_K.\n"
    '        let width = if tensor.dtype == "F32" { 4 } else { 2 };\n'
    "        let elements = tensor.length / width;\n"
    "        return elements > 0 && elements.is_multiple_of(QK_K);\n"
    "    }"
)
new = (
    '    if quant == Some("Q4_K") || quant == Some("Q5_K") {\n'
    "        // El super-bloque cubre 256 elementos consecutivos: el total debe\n"
    "        // ser múltiplo de QK_K.\n"
    '        let width = if tensor.dtype == "F32" { 4 } else { 2 };\n'
    "        let elements = tensor.length / width;\n"
    "        return elements > 0 && elements.is_multiple_of(QK_K);\n"
    "    }"
)
assert s.count(old) == 1
s = s.replace(old, new)

# 3) quantize_q5_k_block antes de la doc de expected_gguf_quantized
anchor = (
    "/// Emisión espejo del contenedor con cuantización opcional (Q8_0/Q4_0/Q5_0).\n"
    "/// El file_type pasa a 7 (MOSTLY_Q8_0) cuando quedó al menos un tensor\n"
    "/// cuantizado; los lengths y el layout siguen la misma disciplina del\n"
    "/// escritor Python (offsets en una sola pasada, alineación a 32).\n"
    "pub fn expected_gguf_quantized(\n"
)
assert s.count(anchor) == 1
insert = "/// Un super-bloque Q5_K (type 13) desde 256 elementos F32/F16: 176 bytes\n/// (d f16 + dmin f16 + scales[12] + qh[32] + qs[128]). Misma maquinaria de\n/// Q4_K con niveles de 5 bits: make_qkx2_quants(32, 31, ..., -0.5, 0.1, 15),\n/// re-cuantización final con clamp 0..31, nibbles por mitades y el 5º bit en\n/// los bits 2·chunk/2·chunk+1 de qh[j]. Espejo verbatim de la referencia ggml.\n#[allow(clippy::unwrap_used)] // las rebanadas tienen longitud verificada antes de cada try_into\nfn quantize_q5_k_block(source_bytes: &[u8], offset: usize, dtype: &str) -> Vec<u8> {\n    let read = |index: usize| -> f32 {\n        if dtype == \"F32\" {\n            f32::from_le_bytes(\n                source_bytes[offset + index * 4..offset + index * 4 + 4]\n                    .try_into()\n                    .unwrap(),\n            )\n        } else {\n            f16_bytes_to_f32(&source_bytes[offset + index * 2..offset + index * 2 + 2])\n        }\n    };\n    let values: Vec<f32> = (0..QK_K).map(read).collect();\n    let mut mins = [0f32; 8];\n    let mut sub_scales = [0f32; 8];\n    let mut l_all = vec![0usize; QK_K];\n    let mut max_scale = 0f32;\n    let mut max_min = 0f32;\n    for sub in 0..8 {\n        let chunk = &values[32 * sub..32 * sub + 32];\n        let mut sum_x2 = 0.0f32;\n        for value in chunk {\n            sum_x2 += value * value;\n        }\n        let av_x = (sum_x2 / 32.0).sqrt();\n        let weights: Vec<f32> = chunk.iter().map(|v| av_x + v.abs()).collect();\n        let (scale, the_min, levels) = make_qkx2_quants(chunk, &weights, 31.0, -0.5, 0.1, 15);\n        sub_scales[sub] = scale;\n        mins[sub] = the_min;\n        l_all[32 * sub..32 * sub + 32].copy_from_slice(&levels);\n        if scale > max_scale {\n            max_scale = scale;\n        }\n        if the_min > max_min {\n            max_min = the_min;\n        }\n    }\n    let inv_scale = if max_scale > 0.0 { 63.0f32 / max_scale } else { 0.0 };\n    let inv_min = if max_min > 0.0 { 63.0f32 / max_min } else { 0.0 };\n    let mut ls_values = [0usize; 8];\n    let mut lm_values = [0usize; 8];\n    for j in 0..8 {\n        ls_values[j] = nearest_int(inv_scale * sub_scales[j]).clamp(0, 63) as usize;\n        lm_values[j] = nearest_int(inv_min * mins[j]).clamp(0, 63) as usize;\n    }\n    let mut scales = [0u8; K_SCALE_SIZE];\n    for j in 0..8 {\n        let (ls, lm) = (ls_values[j] as u8, lm_values[j] as u8);\n        if j < 4 {\n            scales[j] = ls;\n            scales[j + 4] = lm;\n        } else {\n            scales[j + 4] = (ls & 0xF) | ((lm & 0xF) << 4);\n            scales[j - 4] |= (ls >> 4) << 6;\n            scales[j] |= (lm >> 4) << 6;\n        }\n    }\n    let mut block = Vec::with_capacity(Q5_K_BLOCK_BYTES);\n    block.extend_from_slice(&f32_to_f16_bits(max_scale / 63.0).to_le_bytes());\n    block.extend_from_slice(&f32_to_f16_bits(max_min / 63.0).to_le_bytes());\n    block.extend_from_slice(&scales);\n    let d_block = f16_bytes_to_f32(&block[0..2]);\n    let dmin_block = f16_bytes_to_f32(&block[2..4]);\n    for j in 0..8 {\n        let (sc, m) = if j < 4 {\n            ((scales[j] & 63) as u32, (scales[j + 4] & 63) as u32)\n        } else {\n            (\n                ((scales[j + 4] & 0xF) | ((scales[j - 4] >> 6) << 4)) as u32,\n                ((scales[j + 4] >> 4) | ((scales[j] >> 6) << 4)) as u32,\n            )\n        };\n        let sub_d = d_block * sc as f32;\n        if sub_d == 0.0 {\n            continue;\n        }\n        let sub_dm = dmin_block * m as f32;\n        for ii in 0..32 {\n            let index = 32 * j + ii;\n            let l = nearest_int((values[index] + sub_dm) / sub_d).clamp(0, 31) as usize;\n            l_all[index] = l;\n        }\n    }\n    let mut qh = [0u8; QK_K / 8];\n    let mut qs = [0u8; QK_K / 2];\n    let mut m1 = 1u8;\n    let mut m2 = 2u8;\n    let mut cursor = 0usize;\n    for n in (0..QK_K).step_by(64) {\n        for j in 0..32 {\n            let mut l1 = l_all[n + j];\n            if l1 > 15 {\n                l1 -= 16;\n                qh[j] |= m1;\n            }\n            let mut l2 = l_all[n + j + 32];\n            if l2 > 15 {\n                l2 -= 16;\n                qh[j] |= m2;\n            }\n            qs[cursor + j] = (l1 as u8) | ((l2 as u8) << 4);\n        }\n        cursor += 32;\n        m1 <<= 2;\n        m2 <<= 2;\n    }\n    block.extend_from_slice(&qh);\n    block.extend_from_slice(&qs);\n    block\n}\n\n" + anchor
s = s.replace(anchor, insert)

# 4) despacho por formato
old = (
    "    let (block_bytes, emitted_type) = match quant {\n"
    '        Some("Q4_0") => (Q4_0_BLOCK_BYTES, GGML_Q4_0),\n'
    '        Some("Q5_0") => (Q5_0_BLOCK_BYTES, GGML_Q5_0),\n'
    '        Some("Q4_K") => (Q4_K_BLOCK_BYTES, GGML_Q4_K),\n'
    "        _ => (Q8_0_BLOCK_BYTES, GGML_Q8_0),\n"
    "    };"
)
new = (
    "    let (block_bytes, emitted_type) = match quant {\n"
    '        Some("Q4_0") => (Q4_0_BLOCK_BYTES, GGML_Q4_0),\n'
    '        Some("Q5_0") => (Q5_0_BLOCK_BYTES, GGML_Q5_0),\n'
    '        Some("Q4_K") => (Q4_K_BLOCK_BYTES, GGML_Q4_K),\n'
    '        Some("Q5_K") => (Q5_K_BLOCK_BYTES, GGML_Q5_K),\n'
    "        _ => (Q8_0_BLOCK_BYTES, GGML_Q8_0),\n"
    "    };"
)
assert s.count(old) == 1
s = s.replace(old, new)

old = (
    "                let elements = tensor.length / width;\n"
    '                if quant == Some("Q4_K") {\n'
    "                    (elements / QK_K) * block_bytes\n"
    "                } else {\n"
    "                    (elements / QK8_0) * block_bytes\n"
    "                }"
)
new = (
    "                let elements = tensor.length / width;\n"
    '                if quant == Some("Q4_K") || quant == Some("Q5_K") {\n'
    "                    (elements / QK_K) * block_bytes\n"
    "                } else {\n"
    "                    (elements / QK8_0) * block_bytes\n"
    "                }"
)
assert s.count(old) == 1
s = s.replace(old, new)

old = (
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
new = (
    "            let step =\n"
    '                if quant == Some("Q4_K") || quant == Some("Q5_K") { QK_K } else { QK8_0 } * width;\n'
    "            for offset in (0..raw.len()).step_by(step) {\n"
    "                let block = match quant {\n"
    '                    Some("Q4_0") => quantize_q4_0_block(raw, offset, &tensor.dtype),\n'
    '                    Some("Q5_0") => quantize_q5_0_block(raw, offset, &tensor.dtype),\n'
    '                    Some("Q4_K") => quantize_q4_k_block(raw, offset, &tensor.dtype),\n'
    '                    Some("Q5_K") => quantize_q5_k_block(raw, offset, &tensor.dtype),\n'
    "                    _ => quantize_q8_0_block(raw, offset, &tensor.dtype),\n"
    "                };\n"
    "                out.extend_from_slice(&block);\n"
    "            }"
)
assert s.count(old) == 1
s = s.replace(old, new)

io.open(p, 'w', encoding='utf-8', newline='\n').write(s)
print("espejo Rust Q5_K aplicado")
