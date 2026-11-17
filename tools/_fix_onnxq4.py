# -*- coding: utf-8 -*-
"""Ronda 4: warns de clippy — helper muerto, as_chunks, needless borrow,
too_many_arguments en output()."""
import io

p = r'crates/larm-adapter-host/src/conformance/onnx_quantize.rs'
s = io.open(p, encoding='utf-8', newline='').read()

# 1) file_slice_digest quedó sin llamadas tras sustituir los checks QDQ
old = """fn file_slice_digest(bytes: &[u8], start: usize, length: usize) -> Result<Value, LarmError> {
    let slice = bytes
        .get(start..start + length)
        .ok_or_else(|| invalid("onnx_slice_out_of_bounds"))?;
    Ok(json!({"sha256":digest(slice),"size_bytes":length.to_string()}))
}
"""
assert s.count(old) == 1, ('file_slice_digest', s.count(old))
s = s.replace(old, '')

# 2) chunks_exact con tamaño constante + unwrap -> as_chunks (new() y test usan `raw`)
old = """let weight: Vec<f32> = raw
            .chunks_exact(4)
            .map(|chunk| f32::from_le_bytes(chunk.try_into().unwrap()))
            .collect();"""
new = """let weight: Vec<f32> = raw
            .as_chunks::<4>()
            .0
            .iter()
            .map(|chunk| f32::from_le_bytes(*chunk))
            .collect();"""
assert s.count(old) == 2, ('raw chunks', s.count(old))
s = s.replace(old, new)

# 3) el mismo patrón en run() (`weight_raw`)
old = """let weight: Vec<f32> = weight_raw
            .chunks_exact(4)
            .map(|chunk| f32::from_le_bytes(chunk.try_into().unwrap()))
            .collect();"""
new = """let weight: Vec<f32> = weight_raw
            .as_chunks::<4>()
            .0
            .iter()
            .map(|chunk| f32::from_le_bytes(*chunk))
            .collect();"""
assert s.count(old) == 1, ('weight_raw chunks', s.count(old))
s = s.replace(old, new)

# 4) zero_point del output publicado
old = """            json!(zero_int32
                .chunks_exact(4)
                .map(|chunk| u32::from_le_bytes(chunk.try_into().unwrap()))
                .collect::<Vec<_>>()),"""
new = """            json!(zero_int32
                .as_chunks::<4>()
                .0
                .iter()
                .map(|chunk| u32::from_le_bytes(*chunk))
                .collect::<Vec<_>>()),"""
assert s.count(old) == 1, ('zero_point chunks', s.count(old))
s = s.replace(old, new)

# 5) zero_point del test
old = """            zero_bytes.chunks_exact(4).all(|chunk| chunk == [0u8; 4]),"""
new = """            zero_bytes
                .as_chunks::<4>()
                .0
                .iter()
                .all(|chunk| *chunk == [0u8; 4]),"""
assert s.count(old) == 1, ('test zero chunks', s.count(old))
s = s.replace(old, new)

# 6) needless borrow: los parámetros de functional ya son &[u8]
old = """                &completed,
                original,
                &expected_scale_bytes,
                &expected_weight_bytes,"""
new = """                &completed,
                original,
                expected_scale_bytes,
                expected_weight_bytes,"""
assert s.count(old) == 1, ('site 1', s.count(old))
s = s.replace(old, new)

old = """            &recovered_receipt,
            original,
            &expected_scale_bytes,
            &expected_weight_bytes,"""
new = """            &recovered_receipt,
            original,
            expected_scale_bytes,
            expected_weight_bytes,"""
assert s.count(old) == 1, ('site 2', s.count(old))
s = s.replace(old, new)

old = """            &receipt,
            original,
            &expected_scale_bytes,
            &expected_weight_bytes,"""
new = """            &receipt,
            original,
            expected_scale_bytes,
            expected_weight_bytes,"""
assert s.count(old) == 2, ('sites 3+4', s.count(old))
s = s.replace(old, new)

# 7) output() excede el techo de argumentos del linter
old = "    async fn output("
new = "    #[allow(clippy::too_many_arguments)] // el par de esperados del mirror viaja con la llamada\n    async fn output("
assert s.count(old) == 1, ('output sig', s.count(old))
s = s.replace(old, new)

io.open(p, 'w', encoding='utf-8', newline='').write(s)
print('ronda 4 aplicada')
