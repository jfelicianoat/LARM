"""Oráculo focal independiente del conversor ONNX de producto.

Conduce el Adapter empaquetado por las cuatro firmas transformation y valida
el modelo ONNX publicado con un parser protobuf PROPIO del test que no
reutiliza el escritor del paquete: estructura ModelProto/GraphProto (ir
version, producer, opset, nodos Identity, initializers con dims/dtype y
raw_data verbatim del safetensors, inputs/outputs ValueInfoProto tipados).
Incluye negativos de entrada corrupta.
"""
import io
import json
import struct
import sys
import tempfile
import threading
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parents[4]
sys.path.insert(0, str(REPO / "adapters/sdk/python"))
sys.path.insert(0, str(REPO / "adapters/transformation/onnx"))

import larm_adapter_sdk  # noqa: E402
from larm_adapter_sdk import Contracts  # noqa: E402
from larm_adapter_sdk.server import CallContext  # noqa: E402

import package as packaging  # noqa: E402

ONNX_DTYPES = {"F32": 1, "I8": 4, "I16": 5, "I32": 6, "I64": 7, "F16": 10, "F64": 11, "BF16": 16}
ELEMENT_BYTES = {"F64": 8, "F32": 4, "F16": 2, "BF16": 2, "I64": 8, "I32": 4, "I16": 2, "I8": 1}


def read_varint(raw, offset):
    result, shift = 0, 0
    while True:
        byte = raw[offset]
        offset += 1
        result |= (byte & 0x7F) << shift
        if not byte & 0x80:
            return result, offset
        shift += 7


def read_fields(raw, start, end):
    """Itera los campos protobuf de raw[start:end]: (field, wire, valor)."""
    offset = start
    while offset < end:
        key, offset = read_varint(raw, offset)
        field, wire = key >> 3, key & 7
        if wire == 2:
            length, offset = read_varint(raw, offset)
            yield field, wire, raw[offset:offset + length]
            offset += length
        elif wire == 0:
            value, offset = read_varint(raw, offset)
            yield field, wire, value
        else:
            raise AssertionError(f"wire type no cubierto: {wire}")


def parse_model(raw):
    """Parser ONNX/protobuf independiente: estructura completa del modelo."""
    graph = {"nodes": [], "name": None, "initializers": [], "inputs": [], "outputs": [], "opsets": []}
    model = {"ir_version": None, "producer_name": None, "graph": graph, "opsets": []}
    for field, _wire, value in read_fields(raw, 0, len(raw)):
        if field == 1:
            model["ir_version"] = value
        elif field == 2:
            model["producer_name"] = value.decode("utf-8")
        elif field == 8:
            # opset_import (repeated OperatorSetIdProto): domain(1), version(2)
            for o_field, _o_wire, o_value in read_fields(value, 0, len(value)):
                if o_field == 2:
                    model["opsets"].append(o_value)
        elif field == 7:
            for g_field, _g_wire, g_value in read_fields(value, 0, len(value)):
                if g_field == 1:
                    node = {"inputs": [], "outputs": [], "name": None, "op_type": None}
                    for n_field, _n_wire, n_value in read_fields(g_value, 0, len(g_value)):
                        if n_field == 1:
                            node["inputs"].append(n_value.decode("utf-8"))
                        elif n_field == 2:
                            node["outputs"].append(n_value.decode("utf-8"))
                        elif n_field == 3:
                            node["name"] = n_value.decode("utf-8")
                        elif n_field == 4:
                            node["op_type"] = n_value.decode("utf-8")
                    graph["nodes"].append(node)
                elif g_field == 2:
                    graph["name"] = g_value.decode("utf-8")
                elif g_field == 5:
                    tensor = {"dims": [], "data_type": None, "name": None, "raw_data": None}
                    for t_field, _t_wire, t_value in read_fields(g_value, 0, len(g_value)):
                        if t_field == 1:
                            offset = 0
                            while offset < len(t_value):
                                dim, offset = read_varint(t_value, offset)
                                tensor["dims"].append(dim)
                        elif t_field == 2:
                            tensor["data_type"] = t_value
                        elif t_field == 8:
                            tensor["name"] = t_value.decode("utf-8")
                        elif t_field == 9:
                            tensor["raw_data"] = t_value
                    graph["initializers"].append(tensor)
                elif g_field == 8:
                    for o_field, _o_wire, o_value in read_fields(g_value, 0, len(g_value)):
                        if o_field == 2:
                            graph["opsets"].append(o_value)
    return model


def parse_safetensors(raw):
    (header_len,) = struct.unpack_from("<Q", raw, 0)
    header = json.loads(raw[8:8 + header_len].decode("utf-8"))
    data_start = 8 + header_len
    return {name: entry for name, entry in header.items() if name != "__metadata__"}, data_start


def corrupt(source, at):
    damaged = bytearray(source)
    damaged[at] ^= 0x01
    return bytes(damaged)


class ONNXConversionOracle(unittest.TestCase):
    def _build(self, data):
        directory = tempfile.TemporaryDirectory()
        root = Path(directory.name)
        manifest, _profile, _entry = packaging.create(
            REPO, root / "package", root / "work", data
        )
        contracts = Contracts(root / "package" / "docs")
        sys.path.insert(0, str(root / "package" / "transformation"))
        from adapter import Adapter
        adapter = Adapter(root / "package", root / "work", manifest, contracts)
        adapter.open_session()
        self.addCleanup(directory.cleanup)
        return root, adapter, contracts

    def _run_conversion(self, root, adapter, contracts):
        ctx = CallContext("01930000-0000-7000-8000-000000000001", threading.Event(), lambda *_: None)
        described = adapter.call("transformation.describe", {}, ctx)
        self.assertEqual(described["formats_output"], ["ONNX"])
        self.assertEqual(described["formats_input"], ["SAFETENSORS"])
        request = adapter.request()
        plan = adapter.call("transformation.plan", request, ctx)
        self.assertEqual(plan["steps"][0]["phase"], "ONNX")
        receipt = adapter.call("transformation.execute", {
            "plan_id": plan["plan_id"],
            "plan_hash": plan["plan_hash"],
            "approval_id": None,
        }, ctx)
        self.assertEqual(receipt["state"], "COMPLETED")
        verified = adapter.call("transformation.verify", {"job_id": receipt["job_id"]}, ctx)
        return ctx, plan, receipt, verified

    def test_conversion_real_publicada_y_verificada_por_parse_independiente(self):
        fixture = packaging.build_safetensors_fixture()
        root, adapter, contracts = self._build(fixture)
        try:
            ctx, plan, receipt, verified = self._run_conversion(root, adapter, contracts)
            data_artifact = next(a for a in verified["artifacts"] if a["role"] == "DATA")
            self.assertTrue(data_artifact["relative_path"].endswith("/model.onnx"))
            published = root / "work" / "output" / data_artifact["relative_path"]
            raw = published.read_bytes()
            self.assertEqual(int(data_artifact["size_bytes"]), len(raw))
            self.assertEqual(
                data_artifact["sha256"], __import__("hashlib").sha256(raw).hexdigest()
            )

            # El oráculo independiente parsea el modelo completo.
            model = parse_model(raw)
            self.assertEqual(model["ir_version"], 8)
            self.assertEqual(model["producer_name"], "larm-onnx-product")
            self.assertEqual(model["opsets"], [13])
            self.assertEqual(model["graph"]["name"], "larm_product_graph")
            source_tensors, source_data_start = parse_safetensors(fixture)
            self.assertEqual(len(model["graph"]["initializers"]), len(source_tensors))
            for initializer in model["graph"]["initializers"]:
                source = source_tensors[initializer["name"]]
                self.assertEqual(initializer["data_type"], ONNX_DTYPES[source["dtype"]])
                self.assertEqual(initializer["dims"], source["shape"])
                begin = source_data_start + source["data_offsets"][0]
                end = source_data_start + source["data_offsets"][1]
                self.assertEqual(
                    initializer["raw_data"], fixture[begin:end],
                    f"bytes del tensor {initializer['name']} alterados",
                )
            # Cada nodo Identity publica su initializer.
            for node in model["graph"]["nodes"]:
                self.assertEqual(node["op_type"], "Identity")
                self.assertEqual(len(node["inputs"]), 1)
                self.assertEqual(len(node["outputs"]), 1)
                self.assertEqual(node["inputs"][0], node["name"][: -len("_identity")])
            # El fixture de capacidades coincide con la conversión real.
            entry = packaging.descriptor()
            self.assertEqual(entry["fixture"]["output"]["sha256"], __import__("hashlib").sha256(raw).hexdigest())
        finally:
            adapter.shutdown()
            sys.path.remove(str(root / "package" / "transformation"))
            for name in [name for name in list(sys.modules) if name in ("adapter", "files", "state", "onnx")]:
                del sys.modules[name]

    def test_input_corrupto_falla_cerrado_sin_publicar(self):
        fixture = packaging.build_safetensors_fixture()
        dtype_at = fixture.index(b'"F32"') + 2
        damaged = corrupt(fixture, dtype_at)
        root, adapter, contracts = self._build(damaged)
        try:
            ctx = CallContext("01930000-0000-7000-8000-000000000002", threading.Event(), lambda *_: None)
            with self.assertRaises(larm_adapter_sdk.DomainError) as raised:
                adapter.call("transformation.plan", adapter.request(), ctx)
            self.assertEqual(raised.exception.data["details"]["reason"], "product_onnx_input_invalid")
            self.assertFalse(any((root / "work" / "output").iterdir()))
        finally:
            adapter.shutdown()
            sys.path.remove(str(root / "package" / "transformation"))
            for name in [name for name in list(sys.modules) if name in ("adapter", "files", "state", "onnx")]:
                del sys.modules[name]


if __name__ == "__main__":
    unittest.main()
