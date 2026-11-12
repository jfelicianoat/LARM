# -*- coding: utf-8 -*-
"""Sustituye el oráculo GGUF del onnx.rs por el oráculo ONNX en Rust."""
from pathlib import Path

p = Path(r"crates/larm-adapter-host/src/conformance/onnx.rs")
s = p.read_text(encoding="utf-8")

start = s.index("/// Oráculo GGUF en Rust:")
end = s.index("fn file_slice_digest")

oracle = Path(r"crates/larm-adapter-host/src/conformance/onnx_oracle.inc").read_text(encoding="utf-8")
s = s[:start] + oracle + s[end:]

# restos de nombres GGUF en el cuerpo de la suite
s = s.replace("let container = parse_gguf(&actual)?;", "let model = parse_onnx(&actual)?;")
s = s.replace("expected: expected_gguf(&document, &input),", "expected: expected_onnx(&document, &input),")
s = s.replace("let document = parse_safetensors(&original)?;\n        let expected = expected_gguf(&document, &original);",
              "let document = parse_safetensors(&original)?;\n        let expected = expected_onnx(&document, &original);")
s = s.replace("assert_eq!(expected_gguf(&document, &fixture), decode(GOLDEN_GGUF_HEX));",
              "assert_eq!(expected_onnx(&document, &fixture), decode(GOLDEN_ONNX_HEX));")
s = s.replace("let container = parse_gguf(&golden).unwrap();", "let model = parse_onnx(&golden).unwrap();")
s = s.replace('use crate::conformance::gguf_golden::{FIXTURE_HEX, GOLDEN_GGUF_HEX};',
              'use crate::conformance::onnx_golden::{FIXTURE_HEX, GOLDEN_ONNX_HEX};')

p.write_text(s, encoding="utf-8", newline="\n")
print("oráculo sustituido")
