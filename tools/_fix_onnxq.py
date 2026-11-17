# -*- coding: utf-8 -*-
"""Corrige onnx_quantize.rs tras la derivación: campos de datos reales,
nombres de helpers y paso de los esperados del mirror al functional."""
import io

p = r'crates/larm-adapter-host/src/conformance/onnx_quantize.rs'
s = io.open(p, encoding='utf-8').read()

# 1) alinear nombres en new()
old = """        let model = parse_quantized_model(&input)?;
        if model.graph_name != "quant-fixture" {
            return Err(invalid("onnxq_fixture_shape_invalid"));
        }
        let weight = initializer_by_name(&model, "w")?;"""
new = """        let model = parse_onnxq_model(&input)?;
        if model.graph_name != "quant-fixture" {
            return Err(invalid("onnxq_fixture_shape_invalid"));
        }
        let weight = onnxq_initializer(&model, "w")?;"""
assert s.count(old) == 1, ('new names', s.count(old))
s = s.replace(old, new)

# 2) ONNXQ_GOLDEN_HEX en el scope de new(): el use del módulo completo
old = 'use super::{canonical, error, file_digest, invalid, NativeIo, NativeSession, NativeSuite};'
new = ('use super::conformance::onnxq_golden::ONNXQ_GOLDEN_HEX;\n'
       'use super::{canonical, error, file_digest, invalid, NativeIo, NativeSession, NativeSuite};')
assert s.count(old) == 1, ('use', s.count(old))
s = s.replace(old, new)

# 3) run pasa los esperados al functional
old = '        self.functional(session, db, report, &original).await?;'
new = '        self.functional(session, db, report, &original, &expected_scale_bytes, &expected_weight_bytes).await?;'
assert s.count(old) == 1, ('run->functional', s.count(old))
s = s.replace(old, new)

# 4) functional recibe los esperados
old = """    async fn functional(
        &self,
        session: &mut NativeSession,
        db: &mut SqliteConnection,
        report: &mut OnnxQReport,
        original: &[u8],
    ) -> Result<(), LarmError> {"""
new = """    async fn functional(
        &self,
        session: &mut NativeSession,
        db: &mut SqliteConnection,
        report: &mut OnnxQReport,
        original: &[u8],
        expected_scale_bytes: &[u8],
        expected_weight_bytes: &[u8],
    ) -> Result<(), LarmError> {"""
assert s.count(old) == 1, ('functional', s.count(old))
s = s.replace(old, new)

# 5) llamadas a output() dentro del functional pasan los esperados
for old_call, new_call in [
    ("""        let manifest = self
            .output(session, db, report, &completed, original)
            .await?;""",
     """        let manifest = self
            .output(session, db, report, &completed, original, &expected_scale_bytes, &expected_weight_bytes)
            .await?;"""),
    ("""        self.output(session, db, report, &recovered_receipt, original)""",
     """        self.output(session, db, report, &recovered_receipt, original, &expected_scale_bytes, &expected_weight_bytes)"""),
    ("""        self.output(session, db, report, &receipt, original).await?;
        let replay = self""",
     """        self.output(session, db, report, &receipt, original, &expected_scale_bytes, &expected_weight_bytes).await?;
        let replay = self"""),
    ("""        self.output(session, db, report, &receipt, original).await?;
        let before = snapshot(db).await?;""",
     """        self.output(session, db, report, &receipt, original, &expected_scale_bytes, &expected_weight_bytes).await?;
        let before = snapshot(db).await?;"""),
]:
    assert s.count(old_call) == 1, ('output call', old_call[:60], s.count(old_call))
    s = s.replace(old_call, new_call)

io.open(p, 'w', encoding='utf-8', newline='\n').write(s)
print('onnx_quantize.rs corregido')
