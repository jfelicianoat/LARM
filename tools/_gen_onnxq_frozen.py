# -*- coding: utf-8 -*-
"""Genera el congelado public-onnxq-20261114: run.py/public_audit.py copiados,
verify.py derivado del de gguf-quant con el paso public-onnxq (35 pasos) y
audit.py con el walk independiente ONNXQ (protobuf + espejo ORT dynamic)."""
import io
import shutil
from pathlib import Path

ROOT = Path(r"C:\Procesos\Local AI Runtime Manager — LARM\larm")
SRC = ROOT / "docs/reports/P093_runs/public-gguf-quant-20261108"
DST = ROOT / "docs/reports/P093_runs/public-onnxq-20261114"
assert DST.exists() is False
DST.mkdir(parents=True)

shutil.copyfile(SRC / "run.py", DST / "run.py")
shutil.copyfile(SRC / "public_audit.py", DST / "public_audit.py")

# ---- verify.py ----
s = io.open(SRC / "verify.py", encoding="utf-8", newline="").read()

a = '''    ("public-gguf-quant", cargo_test("larm-daemon", "--test", "reference_adapters", "transformation_publica_gguf_cuantizacion_q8_0_hasta_publicacion") + ["--ignored"]),'''
b = a + '''
    ("public-onnxq", cargo_test("larm-daemon", "--test", "reference_adapters", "transformation_publica_onnxq_cuantizador_olive_hasta_publicacion") + ["--ignored"]),'''
assert s.count(a) == 1
s = s.replace(a, b)

a = '"public-gguf","public-onnx","public-gguf-quant","qualified-guardian"'
b = '"public-gguf","public-onnx","public-gguf-quant","public-onnxq","qualified-guardian"'
assert s.count(a) == 1
s = s.replace(a, b)

a = 'and the real Q8_0/Q4_0/Q5_0/Q4_K/Q5_K/Q6_K six-format quantization acceptance:'
b = 'and the real Q8_0/Q4_0/Q5_0/Q4_K/Q5_K/Q6_K six-format quantization acceptance and the ONNX-INT8 Olive quantizer acceptance:'
assert s.count(a) == 1
s = s.replace(a, b)

a = "and file_type 7). Prior registered guardian/native/physical/Core regression"
b = ("and file_type 7; the ONNX-INT8 model published by product.transformation.onnxq with the sealed managed python-olive "
     "runtime injected via OLIVE_RUNTIME, byte-exact against the runtime golden with the QDQ structure "
     "DynamicQuantizeLinear/Mul/MatMulInteger/Cast/Mul and w_quantized int8/w_scale f32 verified against the independent "
     "ORT-dynamic mirror derived from the fixture weights). Prior registered guardian/native/physical/Core regression")
assert s.count(a) == 1
s = s.replace(a, b)

a = '''        assert report["request"]["adapter_id"]=="product.transformation.gguf"
        assert report["request"]["parameters"]=={"quantization":"Q8_0"}'''
b = a + '''
    if name == "public-onnxq" and result.returncode == 0:
        latest=ROOT/"docs/reports/P093_runs/native-public-onnxq.json"
        native=json.loads(latest.read_bytes())
        assert native["status"]=="PASS"
        assert datetime.fromisoformat(native["generated_at"])>=datetime.fromisoformat(summary["started_at"])
        retained=OUT/latest.name;retained.write_bytes(latest.read_bytes())
        evidence=ROOT/native["evidence"]
        assert native["report"]==json.loads((evidence/"public-onnxq/report.json").read_bytes())
        report=native["report"]
        assert report["original_intact"] is True and report["principals_revoked"] is True
        assert report["counts_final"]==[1,2,0,0]
        assert report["request"]["adapter_id"]=="product.transformation.onnxq"
        assert report["request"]["parameters"]=={"quantization":"INT8"}
        assert len(report["artifacts"])==1
        assert report["artifacts"][0]["quantization"]=="INT8"
        paths=[retained,ROOT/"target/debug/larm-daemon.exe"]
        paths.extend(p for p in evidence.rglob("*") if p.is_file())
        value["native_artifacts"]={p.relative_to(ROOT).as_posix():hashlib.sha256(p.read_bytes()).hexdigest() for p in paths}'''
assert s.count(a) == 1
s = s.replace(a, b)

a = 'for name in ["tools/check_architecture.py", "tools/package_copy.py", "docs/spec/PACKAGE_COPY_HASHES.json",\n             "docs/contracts/REVISION_3_13_0_P093.md", "docs/spec/manifest.json",'
b = 'for name in ["tools/check_architecture.py", "tools/package_copy.py", "docs/spec/PACKAGE_COPY_HASHES.json",\n             "docs/contracts/REVISION_3_13_0_P093.md", "docs/contracts/REVISION_3_14_0_P120.md", "docs/spec/manifest.json",'
assert s.count(a) == 1
s = s.replace(a, b)

io.open(DST / "verify.py", "w", encoding="utf-8", newline="\n").write(s)

# ---- audit.py ----
s = io.open(SRC / "audit.py", encoding="utf-8", newline="").read()

a = 'assert summary["status"] == "PASS" and len(summary["steps"]) == 34'
b = 'assert summary["status"] == "PASS" and len(summary["steps"]) == 35'
assert s.count(a) == 1
s = s.replace(a, b)

a = '"passed":449+unit[0][0]+workers[0][0]'
b = '"passed":450+unit[0][0]+workers[0][0]'
assert s.count(a) == 1
s = s.replace(a, b)

a = 'assert groups["public-gguf-quant"]["test_summaries"][0][:4] == [1,0,0,0]'
b = a + '''
assert groups["public-onnxq"]["test_summaries"][0][:4] == [1,0,0,0]'''
assert s.count(a) == 1
s = s.replace(a, b)

onnxq_walk = '''

# The ONNX-INT8 Olive quantizer public case: independently re-verify the
# retained report AND an independent Python protobuf/QDQ walk of the published
# model bytes: parse the container, check the QDQ structure, then re-derive
# the quantized weight and scale from the retained input with an independent
# ORT-dynamic reference implementation.
onnxq_native = json.loads((BASE / "native-public-onnxq.json").read_bytes())
assert onnxq_native["status"] == "PASS"
assert datetime.fromisoformat(onnxq_native["generated_at"]) >= datetime.fromisoformat(summary["started_at"])
onnxq_evidence = (ROOT / onnxq_native["evidence"]).resolve()
assert onnxq_evidence.is_relative_to(ROOT.resolve())
onnxq_dir = onnxq_evidence / "public-onnxq"
onnxq_report = onnxq_native["report"]
assert onnxq_native["report"] == json.loads((onnxq_dir / "report.json").read_bytes())
assert onnxq_report["status"] == "PASS"
assert onnxq_report["original_intact"] is True and onnxq_report["principals_revoked"] is True
assert onnxq_report["counts_final"] == [1, 2, 0, 0]
assert onnxq_report["request"]["adapter_id"] == "product.transformation.onnxq"
assert onnxq_report["request"]["parameters"] == {"quantization": "INT8"}
assert len(onnxq_report["artifacts"]) == 1
assert onnxq_report["artifacts"][0]["quantization"] == "INT8"
assert onnxq_report["artifacts"][0]["ops"] == ["DynamicQuantizeLinear", "Mul", "MatMulInteger", "Cast", "Mul"]
assert onnxq_report["artifacts"][0]["weight_dims"] == [64, 128]
onnxq_cases = {n["case"]: n["status"] for n in onnxq_report["negatives"]}
assert onnxq_cases == {"unknown_adapter": 404, "quant_unsupported": 422, "plan_replay": 201}
onnxq_input = (onnxq_dir / "input.bin").read_bytes()
onnxq_published = (onnxq_dir / "published-0.bin").read_bytes()
assert hashlib.sha256(onnxq_published).hexdigest() == onnxq_report["artifacts"][0]["sha256"]
assert hashlib.sha256(onnxq_input).hexdigest() == onnxq_report["input_sha256"]
assert len(onnxq_published) < len(onnxq_input) // 2, "la cuantizacion debe comprimir al menos a la mitad"

def onnxq_fields(raw, start, end):
    out, off = [], start
    while off < end:
        key, off = onnxq_varint(raw, off)
        field, wire = key >> 3, key & 7
        if wire == 0:
            value, off = onnxq_varint(raw, off)
            out.append((field, wire, b"", value))
        elif wire == 2:
            length, off = onnxq_varint(raw, off)
            out.append((field, wire, raw[off:off + length], 0))
            off += length
        elif wire == 5:
            out.append((field, wire, raw[off:off + 4], 0)); off += 4
        elif wire == 1:
            out.append((field, wire, raw[off:off + 8], 0)); off += 8
        else:
            raise AssertionError(f"wire {wire} inesperado en el walk ONNXQ")
    return out

def onnxq_graph(model_bytes):
    graph = [payload for f, w, payload, v in onnxq_fields(model_bytes, 0, len(model_bytes)) if f == 7]
    assert len(graph) == 1
    return graph[0]

def onnxq_initializers(graph_bytes):
    tensors = []
    for f, w, payload, v in onnxq_fields(graph_bytes, 0, len(graph_bytes)):
        if f != 5:
            continue
        tensor = {"dims": [], "dtype": None, "name": None, "raw_data": None,
                  "float_data": None, "int32_data": None, "int64_data": None}
        for tf, tw, tp, tv in onnxq_fields(payload, 0, len(payload)):
            if tf == 1:
                tensor["dims"].append(tv)
            elif tf == 2:
                tensor["dtype"] = tv
            elif tf == 4:
                tensor["float_data"] = tp
            elif tf == 5:
                tensor["int32_data"] = tp
            elif tf == 7:
                tensor["int64_data"] = tp
            elif tf == 8:
                tensor["name"] = tp.decode("utf-8")
            elif tf == 9:
                tensor["raw_data"] = tp
        tensors.append(tensor)
    return {t["name"]: t for t in tensors if t["name"]}

def onnxq_nodes(graph_bytes):
    ops = []
    for f, w, payload, v in onnxq_fields(graph_bytes, 0, len(graph_bytes)):
        if f != 1:
            continue
        op = None
        for nf, nw, np, nv in onnxq_fields(payload, 0, len(payload)):
            if nf == 4:
                op = np.decode("utf-8")
        ops.append(op)
    return ops

published_graph = onnxq_graph(onnxq_published)
assert onnxq_nodes(published_graph) == ["DynamicQuantizeLinear", "Mul", "MatMulInteger", "Cast", "Mul"]
published_init = onnxq_initializers(published_graph)
weight = published_init["w_quantized"]
assert weight["dtype"] == 3 and weight["dims"] == [64, 128]
assert weight["raw_data"] is not None and len(weight["raw_data"]) == 8192
scale = published_init["w_scale"]
assert scale["dtype"] == 1
scale_bytes = scale["float_data"] or scale["raw_data"]
assert scale_bytes is not None and len(scale_bytes) == 4
zero = published_init["w_zero_point"]
assert zero["dtype"] == 3
zero_bytes = zero["int32_data"] or zero["int64_data"] or zero["raw_data"]
assert zero_bytes is not None and len(zero_bytes) >= 1
assert all(b == 0 for b in zero_bytes), "el zero_point debe ser 0"
assert published_init.get("w", {"dtype": None})["dtype"] != 1, "el peso F32 fuente no debe sobrevivir"

input_graph = onnxq_graph(onnxq_input)
input_init = onnxq_initializers(input_graph)
source_weight = input_init["w"]
assert source_weight["dtype"] == 1 and source_weight["dims"] == [64, 128]
source_raw = source_weight["raw_data"]
assert source_raw is not None and len(source_raw) == 8192 * 4

values = [struct.unpack_from("<f", source_raw, off)[0] for off in range(0, len(source_raw), 4)]
max_abs = max(abs(v) for v in values)
expected_scale = struct.unpack("<f", struct.pack("<f", max_abs / 127.0))[0]
observed_scale = struct.unpack("<f", scale_bytes)[0]
assert observed_scale == expected_scale, "la escala publicada difiere de la formula ORT dynamic"
expected_int8 = bytearray()
for v in values:
    quotient = v / expected_scale
    rounded = math.floor(quotient + 0.5) if quotient >= 0 else math.ceil(quotient - 0.5)
    expected_int8 += struct.pack("<b", max(-127, min(127, int(rounded))))
assert bytes(expected_int8) == weight["raw_data"], "el peso int8 publicado difiere del espejo independiente"
'''

a = '''assert quant_published > 0
assert quant_formats_verified == {8, 2, 6, 12, 13, 14}, quant_formats_verified
assert quant_blocks_verified == 98304 * 3 + 12288 * 3'''
b = a + onnxq_walk
assert s.count(a) == 1
s = s.replace(a, b)

io.open(DST / "audit.py", "w", encoding="utf-8", newline="\n").write(s)
print("congelado generado en", DST.name)
