# -*- coding: utf-8 -*-
"""Corrige los unused del onnx_quantize.rs derivado."""
import io

p = r'crates/larm-adapter-host/src/conformance/onnx_quantize.rs'
s = io.open(p, encoding='utf-8', newline='').read()

# 1) el run pasa los esperados al functional (paso 3 pendiente)
old = '        self.functional(session, db, report, &original).await?;'
new = ('        self.functional(session, db, report, &original, &expected_scale_bytes, &expected_weight_bytes)\n'
       '            .await?;')
assert s.count(old) == 1, s.count(old)
s = s.replace(old, new)

# 2) eliminar el campo weight del struct (el mirror usa el local del run)
old = """pub struct OnnxQuantizeSuite {
    config: OnnxQuantizeConfig,
    bundle: Value,
    expected: Vec<u8>,
    weight: Vec<f32>,
    expected_scale_bytes: Vec<u8>,
    expected_weight_bytes: Vec<u8>,
}"""
new = """pub struct OnnxQuantizeSuite {
    config: OnnxQuantizeConfig,
    bundle: Value,
    expected: Vec<u8>,
}"""
assert s.count(old) == 1
s = s.replace(old, new)

old = """        let suite = Self {
            config,
            bundle: bundle()?,
            expected: hex::decode(ONNXQ_GOLDEN_HEX)
                .map_err(|_| invalid("onnxq_golden_output_invalid"))?,
            weight,
            expected_scale_bytes: scale.to_le_bytes().to_vec(),
            expected_weight_bytes: quantized
                .iter()
                .flat_map(|value| (*value as i8).to_le_bytes())
                .collect(),
        };"""
new = """        let suite = Self {
            config,
            bundle: bundle()?,
            expected: hex::decode(ONNXQ_GOLDEN_HEX)
                .map_err(|_| invalid("onnxq_golden_output_invalid"))?,
        };"""
assert s.count(old) == 1
s = s.replace(old, new)

# 3) el p unused en el find del op_type (el payload no se usa ahí)
old = """                let op_type = inner
                    .iter()
                    .find(|(f, p, _)| *f == 4)
                    .map(|(_, p, _)| String::from_utf8_lossy(p).to_string())
                    .ok_or_else(|| invalid("onnxq_node_op_missing"))?;"""
new = """                let op_type = inner
                    .iter()
                    .find(|(f, _, _)| *f == 4)
                    .map(|(_, p, _)| String::from_utf8_lossy(p).to_string())
                    .ok_or_else(|| invalid("onnxq_node_op_missing"))?;"""
assert s.count(old) == 1
s = s.replace(old, new)

io.open(p, 'w', encoding='utf-8', newline='').write(s)
print('unused corregidos')
