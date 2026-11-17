# -*- coding: utf-8 -*-
"""Ronda 3: firma de functional con los esperados, llamadas internas a
output() con los parámetros, y mirror muerto fuera de new()."""
import io

p = r'crates/larm-adapter-host/src/conformance/onnx_quantize.rs'
s = io.open(p, encoding='utf-8', newline='').read()

# 1) la firma de functional recibe los esperados del run
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
assert s.count(old) == 1, ('functional sig', s.count(old))
s = s.replace(old, new)

# 2) las llamadas internas usan los parámetros, no los campos del struct
n = s.count('&self.expected_scale_bytes')
assert n == 4, ('scale uses', n)
s = s.replace('&self.expected_scale_bytes', '&expected_scale_bytes')
n = s.count('&self.expected_weight_bytes')
assert n == 4, ('weight uses', n)
s = s.replace('&self.expected_weight_bytes', '&expected_weight_bytes')

# 3) el mirror de new() era trabajo muerto: run() lo computa él mismo
old = """        let (scale, quantized) = mirror_quantize(&weight);
        let suite = Self {"""
new = """        let suite = Self {"""
assert s.count(old) == 1, ('mirror in new', s.count(old))
s = s.replace(old, new)

io.open(p, 'w', encoding='utf-8', newline='').write(s)
print('ronda 3 aplicada')
