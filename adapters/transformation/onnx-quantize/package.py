"""Empaquetado del cuantizador ONNX de producto (P065): runtime gestionado
por ruta — el paquete lleva el worker y el lock de dependencias sellado, no
el runtime de 1,2 GB. El fixture de entrada es un modelo ONNX determinista
emitido con protobuf a mano (MatMul F32 [64x128])."""
import hashlib
import json
import os
import shutil
import tempfile
import struct
import sys
import uuid
from pathlib import Path
import importlib.util

REPO = Path(__file__).resolve().parent.parent.parent.parent

def _varint(value):
    out = b""
    while True:
        byte = value & 0x7F
        value >>= 7
        if value:
            out += bytes((byte | 0x80,))
        else:
            out += bytes((byte,))
            return out

def _tag(field, wire):
    return _varint((field << 3) | wire)

def _string_field(field, text):
    raw = text.encode("utf-8")
    return _tag(field, 2) + _varint(len(raw)) + raw

def _message_field(field, raw):
    return _tag(field, 2) + _varint(len(raw)) + raw

def value_info(name, dim, elem=1):
    """ValueInfoProto completo: name(1) → type(2){ tensor_type(1){
    elem_type(1), shape(2){ dim(1){ dim_value(1) } } } } — la jerarquía
    completa del ValueInfoProto de ONNX, sin el doc_string."""
    tensor = (_tag(1, 0) + _varint(elem)
              + _message_field(2, _message_field(1, _tag(1, 0) + _varint(dim))))
    type_proto = _message_field(1, tensor)
    return _string_field(1, name) + _message_field(2, type_proto)


def build_onnx_fixture():
    """Modelo ONNX determinista de entrada: y = x · W (MatMul F32 [1x64 ·
    64x128]) — el patrón de peso que el cuantizador int8 transforma."""
    W = struct.pack("<8192f", *(1.0 + (index % 97) * 0.031 for index in range(64 * 128)))
    weight = (_tag(1, 0) + _varint(64)   # dims[0] = 64 (varint, no int64 fijo)
              + _tag(1, 0) + _varint(128)  # dims[1] = 128
              + _tag(2, 0) + _varint(1)   # data_type = F32 (1)
              + _tag(8, 2) + _varint(1) + b"w"
              + _tag(9, 2) + _varint(len(W)) + W)
    graph = (_string_field(2, "quant-fixture")
             + _message_field(5, weight)
             + _message_field(11, value_info("x", 64))
             + _message_field(12, value_info("y", 128))
             + _message_field(1, (_string_field(4, "MatMul")
                                  + _string_field(1, "x") + _string_field(1, "w")
                                  + _string_field(2, "y")
                                  + _string_field(3, "quant-node")))
             + _string_field(2, "larm-onnxq-fixture"))
    model = (_tag(1, 0) + _varint(8)  # ir_version = 8
             + _message_field(7, graph)
             + _message_field(8, (_string_field(1, "") + _tag(2, 0) + _varint(17)))
             + _message_field(14, (_string_field(1, "")
                                   + _tag(2, 0) + _varint(17))))
    return model


def _conversion_fixture(fixture_input):
    """El ONNX cuantizado del fixture, calculado con el runtime real
    gestionado por ruta (OLIVE_RUNTIME)."""
    import subprocess
    runtime = os.environ.get("OLIVE_RUNTIME", "")
    if not runtime:
        raise ValueError("OLIVE_RUNTIME no está definido para el fixture de conversión")
    with tempfile.TemporaryDirectory(prefix="onnxq-fixture-") as directory:
        input_path = Path(directory) / "input.onnx"
        input_path.write_bytes(fixture_input)
        output_dir = Path(directory) / "out"
        output_dir.mkdir()
        # Mismo motivo que files.py: los ficheros temporales del runtime van a
        # un scratch corto y ASCII (los lanzadores y ORT fallan con rutas
        # extendidas \\?\ bajo un TEMP de trabajo con unicode/espacios).
        scratch = os.environ.get("USERPROFILE")
        scratch = Path(scratch, "AppData", "Local", "Temp") if scratch else None
        if scratch is not None:
            scratch.mkdir(parents=True, exist_ok=True)
        env = {**os.environ, "PYTHONIOENCODING": "utf-8"}
        if scratch is not None:
            env["TEMP"] = env["TMP"] = env["TMPDIR"] = str(scratch)
        process = subprocess.run(
            [str(Path(runtime) / "Scripts/python.exe"), "-X", "utf8",
             str(Path(__file__).resolve().parent / "quantize.py"),
             str(input_path), str(output_dir / "model.onnx"), "INT8"],
            check=False, capture_output=True, timeout=600, env=env,
        )
        if process.returncode != 0:
            detail = (process.stdout.decode("utf-8", "replace").strip().splitlines() or [""])[-1]
            raise RuntimeError(f"cuantización del fixture falló (rc={process.returncode}): {detail[:400]}")
        result = (output_dir / "model.onnx").read_bytes()
    return result


class _NullCtx:
    def check_cancelled(self):
        return None

    def progress(self, *_args):
        return None


def descriptor():
    from larm_adapter_sdk.capabilities import digest
    def schema(maximum):
        return {"type": "object", "additionalProperties": False, "required": ["sha256", "size_bytes"],
                "properties": {"sha256": {"type": "string", "pattern": "^[0-9a-f]{64}$"},
                               "size_bytes": {"type": "string", "pattern": "^(0|[1-9][0-9]*)$", "maxLength": 19,
                                              "x-maximum-decimal": str(maximum)}}}
    input_schema, output_schema = schema(64 << 20), schema(64 << 20)
    fixture_input = build_onnx_fixture()
    fixture_output = _conversion_fixture(fixture_input)
    fixture = {"input": {"sha256": hashlib.sha256(fixture_input).hexdigest(), "size_bytes": str(len(fixture_input))},
               "output": {"sha256": hashlib.sha256(fixture_output).hexdigest(), "size_bytes": str(len(fixture_output))}}
    return {"id": "vendor.product.onnxq", "version": "1.0", "input_schema": input_schema, "output_schema": output_schema,
            "input_sha256": digest(input_schema), "output_sha256": digest(output_schema),
            "mapping": "product.onnx.onnxq.v1", "estimator_id": "product.onnxq.buffers.v1",
            "fixture": fixture, "fixture_sha256": digest(fixture)}


def create(repo, package, work, data):
    sys.path.insert(0, str(repo / "adapters/sdk/python"))
    if not isinstance(data, bytes) or len(data) > 64 << 20:
        raise ValueError("Fixture propia binaria de hasta 64 MiB requerida")
    spec = importlib.util.spec_from_file_location("product_onnxq_packaging", repo / "adapters/source/reference/package.py")
    shared = importlib.util.module_from_spec(spec); spec.loader.exec_module(shared)
    package, work = Path(package).resolve(), Path(work).resolve()
    package.mkdir(parents=True, exist_ok=False); work.mkdir(parents=True, exist_ok=False)
    shutil.copytree(repo / "adapters/transformation/onnx-quantize", package / "transformation",
                    ignore=shutil.ignore_patterns("__pycache__", "package.py", "tests"))
    shutil.copytree(repo / "adapters/sdk/python/larm_adapter_sdk", package / "sdk/larm_adapter_sdk",
                    ignore=shutil.ignore_patterns("__pycache__"))
    for name in ("contracts/schemas/larm.schema.json", "contracts/schemas/domain.schema.json",
                 "contracts/adapter-methods.json", "contracts/capability-contracts.json",
                 "adapters/schemas/frame.request.schema.json", "adapters/schemas/frame.response.schema.json",
                 "adapters/schemas/frame.progress.schema.json"):
        target = package / "docs" / name
        target.parent.mkdir(parents=True, exist_ok=True); shutil.copyfile(repo / "docs" / name, target)
    for name in ("method-schemas", "capability-schemas"):
        shutil.copytree(repo / "docs/adapters" / name, package / "docs/adapters" / name,
                        ignore=shutil.ignore_patterns("README.md"))
    bill = shared.interpreter(repo, package / "bin")
    shared.module_paths(package / "bin", "transformation")
    (package / "dependencies.lock.json").write_text(json.dumps(bill, indent=2), encoding="utf-8")
    for name in ("input", "profile", "state", "output"):
        (work / name).mkdir()
    (work / "input/model.onnx").write_bytes(data)
    entry = descriptor()
    (package / "product-capabilities.json").write_text(json.dumps({"schema_version": "1.0", "entries": [entry]}, indent=2), encoding="utf-8")
    profile = {"input_variant_id": str(uuid.uuid7()), "storage_location_id": str(uuid.uuid7()),
               "input": {"relative_path": "model.onnx", "sha256": hashlib.sha256(data).hexdigest(), "size_bytes": str(len(data))}}
    (work / "profile/profile.json").write_text(json.dumps(profile, indent=2), encoding="utf-8")
    table = json.loads((repo / "docs/contracts/adapter-methods.json").read_text(encoding="utf-8"))
    roots = ["product-package"] + ["product-onnxq-" + name for name in ("input", "profile", "state", "output")]
    manifest = {"id": "product.transformation.onnxq", "version": "1.0.0", "family": "transformation",
                "protocol_version": "1.0", "publisher": "LARM product onnxq",
                "executable": {"relative_path": "bin/python.exe", "args": ["-I", "-B", "-m", "main"]},
                "platforms": ["windows"], "architectures": ["x86_64"],
                "methods": table["mandatory_common"] + [m["method"] for m in table["methods"] if m["family"] == "transformation"],
                "permissions": {"read_roots": roots, "write_roots": ["product-onnxq-state", "product-onnxq-output"],
                                "network_origins": [], "process_allowlist": [], "secret_refs": []},
                "capabilities": [entry["id"]], "package_sha256": shared.package_hash(package),
                "signature": None, "installation_strategies": []}
    (package / "manifest.json").write_text(json.dumps(manifest, indent=2), encoding="utf-8")
    return manifest, profile, entry
