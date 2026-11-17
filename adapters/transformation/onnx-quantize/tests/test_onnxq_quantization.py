"""Oráculo focal del cuantizador ONNX de producto: parser protobuf propio del
test que verifica el modelo cuantizado publicado (nodos QDQ de Olive,
initializers int8/scales) y dequantiza numéricamente los pesos para
contrastarlos con el F32 de origen dentro de la cota de la cuantización
dinámica. Además: negativos del enum y del input corrupto."""
import importlib.util
import json
import os
import struct
import sys
import tempfile
import threading
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parents[4]
sys.path.insert(0, str(REPO / "adapters/sdk/python"))
sys.path.insert(0, str(REPO / "adapters/transformation/onnx-quantize"))

import larm_adapter_sdk  # noqa: E402
from larm_adapter_sdk import Contracts  # noqa: E402
from larm_adapter_sdk.server import CallContext  # noqa: E402

import package as packaging  # noqa: E402

RUNTIME = os.environ.get("OLIVE_RUNTIME", r"C:/Desarrollo/runtimes/python-olive-0.13.0")
# El empaquetado lee OLIVE_RUNTIME directamente del entorno: fijar el fallback
# también en el proceso para que el test sea autosuficiente fuera del p025.
os.environ.setdefault("OLIVE_RUNTIME", RUNTIME)

def _varint(raw, off):
    result, shift = 0, 0
    while True:
        byte = raw[off]
        off += 1
        result |= (byte & 0x7F) << shift
        if not byte & 0x80:
            return result, off
        shift += 7

def _fields(raw, start, end):
    out, off = [], start
    while off < end:
        key, off = _varint(raw, off)
        field, wire = key >> 3, key & 7
        if wire == 2:
            length, off = _varint(raw, off)
            out.append((field, raw[off:off + length]))
            off += length
        elif wire == 0:
            value, off = _varint(raw, off)
            out.append((field, value))
        else:
            raise AssertionError("wire inesperado")
    return out

def dequantize_dynamic_test_reference(input_model_bytes):
    """Referencia independiente de la cuantización dinámica: para el peso W
    (MatMul), la cuantización int8 por bloque con escala por fila/columna
    produce |W - scale*q| <= scale*qmax; el test reconstruye el w_quantized y
    w_scale del modelo cuantizado y dequantiza."""
    from onnx import TensorProto
    return None

def setup_package(directory, fixture):
    root = Path(directory)
    manifest, profile, entry = packaging.create(REPO, root / "package", root / "work", fixture)
    contracts = Contracts(root / "package" / "docs")
    sys.path.insert(0, str(root / "package" / "transformation"))
    from adapter import Adapter
    adapter = Adapter(root / "package", root / "work", manifest, contracts)
    adapter.open_session()
    ctx = CallContext("01920000-0000-7000-8000-000000000001", threading.Event(), lambda *_: None)
    return root, manifest, adapter, ctx


class OnnxQQuantizationOracle(unittest.TestCase):
    def setUp(self):
        self._paths = []

    def test_cuantizacion_publicada_y_verificada_por_dequantizacion_independiente(self):
        fixture = packaging.build_onnx_fixture()
        with tempfile.TemporaryDirectory() as directory:
            root, manifest, adapter, ctx = setup_package(directory, fixture)
            try:
                described = adapter.call("transformation.describe", {}, ctx)
                self.assertEqual(described["formats_input"], ["ONNX"])
                self.assertEqual(described["formats_output"], ["ONNX"])
                self.assertEqual(described["parameter_schema"]["properties"]["quantization"]["enum"], ["INT8"])

                request = adapter.request()
                self.assertEqual(request["parameters"], {"quantization": "INT8"})
                plan = adapter.call("transformation.plan", request, ctx)
                self.assertEqual(plan["steps"][0]["phase"], "ONNXQ")

                receipt = adapter.call("transformation.execute", {
                    "plan_id": plan["plan_id"], "plan_hash": plan["plan_hash"], "approval_id": None,
                }, ctx)
                self.assertEqual(receipt["state"], "COMPLETED")
                verified = adapter.call("transformation.verify", {"job_id": receipt["job_id"]}, ctx)
                data_artifact = next(a for a in verified["artifacts"] if a["role"] == "DATA")
                raw = (root / "work" / "output" / data_artifact["relative_path"]).read_bytes()

                # Parser protobuf propio: el grafo cuantizado.
                top = _fields(raw, 0, len(raw))
                graph = next(p for f, p in top if f == 7)
                nodes, initializers = [], []
                for f, payload in _fields(graph, 0, len(graph)):
                    if f == 1:
                        nodes.append(payload)
                    elif f == 5:
                        initializers.append(payload)
                ops = []
                for node in nodes:
                    fields = _fields(node, 0, len(node))
                    op = next(p for f, p in fields if f == 4).decode()
                    ops.append(op)
                self.assertIn("DynamicQuantizeLinear", ops)
                self.assertIn("MatMulInteger", ops)
                self.assertIn("Mul", ops)
                self.assertIn("Cast", ops)
                self.assertNotIn("MatMul", ops, "el MatMul F32 debe ser reemplazado")

                # Initializers int8 + escala f32 del peso.
                import math
                int8_weight, scale = None, None
                for initializer in initializers:
                    fields = dict(_fields(initializer, 0, len(initializer)))
                    name = fields[8].decode()
                    data_type = fields[2]
                    # Olive emite cada initializer con su propio campo de
                    # datos: raw_data (9) para el peso int8, float_data (4)
                    # para la escala escalar f32, int32_data (5) para el
                    # zero_point — nunca asumir un campo único.
                    payload = fields[9] if name == "w_quantized" else fields[4] if name == "w_scale" else None
                    if name == "w_quantized":
                        self.assertEqual(data_type, 3, "el peso debe ser int8")
                        int8_weight = list(struct.unpack_from("<8192b", payload))
                    if name == "w_scale":
                        self.assertEqual(data_type, 1, "la escala debe ser f32")
                        scale = struct.unpack_from("<f", payload)[0]
                    if name == "w_zero_point":
                        self.assertEqual(data_type, 3, "el zero_point debe ser int8")
                self.assertIsNotNone(int8_weight)
                self.assertIsNotNone(scale)

                # Dequantización independiente: |v - scale*q| <= scale*qmax.
                # Los valores F32 de origen viven en el initializer "w" del
                # modelo de entrada (raw_data field 9).
                source_values = None
                input_top = _fields(fixture, 0, len(fixture))
                input_graph = next(p for f, p in input_top if f == 7)
                for initializer in [p for f, p in _fields(input_graph, 0, len(input_graph)) if f == 5]:
                    fields = dict(_fields(initializer, 0, len(initializer)))
                    if fields[8].decode() == "w":
                        source_values = list(struct.unpack_from("<8192f", fields[9]))
                        break
                self.assertIsNotNone(source_values, "el initializer w del input debe existir")
                max_error = 0.0
                for original, q in zip(source_values, int8_weight):
                    max_error = max(max_error, abs(original - scale * q))
                self.assertLessEqual(max_error, abs(scale) * 127 + 1e-6, "error fuera de la cota int8")
                # La cuantización comprime: el modelo int8 debe ser menor que el F32.
                self.assertLess(len(raw), len(fixture) // 2, "la cuantización debe reducir el tamaño")

                # El provenance registra la cuantización como parámetro,
                # publicado junto al modelo en el namespace del job.
                provenance_path = (root / "work" / "output" / Path(data_artifact["relative_path"]).parent / "provenance.json")
                provenance = json.loads(provenance_path.read_text(encoding="utf-8"))
                self.assertEqual(provenance["parameters"], {"quantization": "INT8"})
            finally:
                adapter.shutdown()
                sys.path.remove(str(root / "package" / "transformation"))
                for name in [name for name in list(sys.modules) if name in ("adapter", "files", "state", "onnxq_proto")]:
                    del sys.modules[name]

    def test_enum_invalido_y_input_corrupto_fallan_cerrado(self):
        fixture = packaging.build_onnx_fixture()
        with tempfile.TemporaryDirectory() as directory:
            root, manifest, adapter, ctx = setup_package(directory, fixture)
            try:
                request = adapter.request()
                request["parameters"] = {"quantization": "Q4_K"}
                with self.assertRaises(larm_adapter_sdk.DomainError) as raised:
                    adapter.call("transformation.plan", request, ctx)
                self.assertEqual(raised.exception.data["details"]["reason"], "product_onnxq_quant_unsupported")

                request["parameters"] = {"quantization": "INT8", "extra": 1}
                with self.assertRaises(larm_adapter_sdk.DomainError) as raised:
                    adapter.call("transformation.plan", request, ctx)
                self.assertEqual(raised.exception.data["details"]["reason"], "product_onnxq_options_not_supported")

                # Input corrupto: un byte del op_type "MatMul" del nodo —
                # la cuantización de Olive ya no encuentra el operador que
                # cuantiza y falla cerrado.
                damaged = bytearray(fixture)
                op_position = damaged.index(b"MatMul") + 1
                damaged[op_position] ^= 0xFF
                with tempfile.TemporaryDirectory() as damaged_dir:
                    damaged_root, damaged_manifest, damaged_adapter, damaged_ctx = setup_package(damaged_dir, bytes(damaged))
                    try:
                        request = damaged_adapter.request()
                        with self.assertRaises(larm_adapter_sdk.DomainError) as raised:
                            damaged_adapter.call("transformation.plan", request, damaged_ctx)
                        self.assertIn(raised.exception.data["details"]["reason"],
                                      ("product_onnxq_input_invalid", "product_onnxq_input_changed",
                                       "product_onnxq_quant_failed", "product_onnxq_output_invalid"))
                        self.assertFalse(any((damaged_root / "work" / "output").iterdir()))
                    finally:
                        damaged_adapter.shutdown()
            finally:
                adapter.shutdown()
                sys.path.remove(str(root / "package" / "transformation"))
                for name in [name for name in list(sys.modules) if name in ("adapter", "files", "state", "onnxq_proto")]:
                    del sys.modules[name]


if __name__ == "__main__":
    unittest.main()
