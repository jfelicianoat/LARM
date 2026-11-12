"""Paquete externo completo con Python fijado; fixture safetensors del caller."""
import hashlib
import importlib.util
import io
import json
import shutil
import struct
import sys
import uuid
from pathlib import Path


def build_conformance_safetensors():
    """Fixture grande para la cualificación: los bloques duraderos y el
    progreso por tensor necesitan una conversión de varios segundos. Los
    valores F32 son FINITOS y deterministas (un bloque variado de 1024
    elementos repetido): la ruta Q8_0 cuantiza pesos reales, no patrones de
    bytes arbitrarios con infinitos/NaN que fallan cerrado por diseño."""
    import math
    values = [
        math.ldexp(1 + (index % 255), -(3 + (index % 24))) * (1 if index % 2 == 0 else -1)
        for index in range(1024)
    ]
    block = struct.pack("<1024f", *values)
    big_raw = block * ((12 << 20) // len(block))
    header = {
        "weight.big": {"dtype": "F32", "shape": [(12 << 20) // 4], "data_offsets": [0, 12 << 20]},
    }
    offset = 12 << 20
    header["weight.scale"] = {"dtype": "F16", "shape": [4], "data_offsets": [offset, offset + 8]}
    header["n_positions"] = {"dtype": "I64", "shape": [3], "data_offsets": [offset + 8, offset + 32]}
    document = json.dumps(header, separators=(",", ":")).encode("utf-8")
    document += b" " * ((8 - (len(document) % 8)) % 8)
    return struct.pack("<Q", len(document)) + document + big_raw + struct.pack("<4H", 15360, 16384, 17408, 18432) + struct.pack("<3q", 7, 8, 9)


def build_safetensors_fixture():
    """Fixture safetensors determinista: tres tensores (F32 2-D, F16 1-D,
    I64 1-D) que cubren los dtypes copiables y el orden de ejes ggml."""
    f16_raw = struct.pack("<4H", 15360, 16384, 17408, 18432)  # 1.0,2.0,3.0,4.0 en f16
    tensors = [
        ("weight.embedding", "F32", [2, 3], struct.pack("<6f", 1.0, 2.0, 3.0, 4.0, 5.0, 6.0)),
        ("weight.scale", "F16", [4], f16_raw),
        ("n_positions", "I64", [3], struct.pack("<3q", 7, 8, 9)),
    ]
    header = {}
    blobs = []
    offset = 0
    for name, dtype, shape, raw in tensors:
        header[name] = {"dtype": dtype, "shape": shape, "data_offsets": [offset, offset + len(raw)]}
        offset += len(raw)
        blobs.append(raw)
    document = json.dumps(header, separators=(",", ":")).encode("utf-8")
    document += b" " * ((8 - (len(document) % 8)) % 8)
    return struct.pack("<Q", len(document)) + document + b"".join(blobs)


def build_quantization_fixture():
    """Fixture safetensors determinista para la ruta Q8_0: un tensor F32
    [64] cuantizable (eje rápido múltiplo de QK8_0) con valores que cubren
    signo, escala f16, redondeo half-away y bloque cero exacto."""
    import math
    values = [0.0] * 32 + [math.ldexp(1.0 + index, -8 - (index % 7)) * (1 if index % 2 == 0 else -1) for index in range(32)]
    raw = struct.pack("<64f", *values)
    header = {"q.weight": {"dtype": "F32", "shape": [64], "data_offsets": [0, len(raw)]}}
    document = json.dumps(header, separators=(",", ":")).encode("utf-8")
    document += b" " * ((8 - (len(document) % 8)) % 8)
    return struct.pack("<Q", len(document)) + document + raw


def build_quantization_k_fixture():
    """Fixture safetensors determinista para Q4_K: F32 [256] (QK_K exacto)
    con 8 sub-bloques de rango distinto — cero exacto, rangos amplios,
    estrechos, desplazados en positivo y con el eje rápido exacto — para
    ejercitar todas las ramas del empaquetado de escalas[12]."""
    import math
    values = []
    for sub in range(8):
        for index in range(32):
            if sub == 0:
                values.append(0.0)
            elif sub == 1:
                values.append(math.ldexp(1 + index, -4) * (1 if index % 2 == 0 else -1))
            elif sub == 2:
                values.append(math.ldexp(1 + index % 8, -14))
            elif sub == 3:
                values.append(-math.ldexp(1 + index % 8, -14))
            elif sub == 4:
                values.append(math.ldexp(1 + index, -3))
            elif sub == 5:
                values.append(0.5 + index * math.ldexp(1, -6))
            elif sub == 6:
                values.append(-0.25 - index * math.ldexp(1, -8))
            else:
                values.append(math.ldexp(33 - index, -5))
    raw = struct.pack("<256f", *values)
    header = {"q.kweight": {"dtype": "F32", "shape": [256], "data_offsets": [0, len(raw)]}}
    document = json.dumps(header, separators=(",", ":")).encode("utf-8")
    document += b" " * ((8 - (len(document) % 8)) % 8)
    return struct.pack("<Q", len(document)) + document + raw


def _conversion_fixture(fixture_input):
    """El GGUF del fixture, calculado con el núcleo real del paquete."""
    sys.path.insert(0, str(Path(__file__).resolve().parent))
    import gguf as converter

    opener = lambda: io.BytesIO(fixture_input)
    parsed = converter.read_safetensors(opener, len(fixture_input))
    converted = bytearray()
    converter.stream(parsed, opener, _NullCtx(), converted.extend)
    return bytes(converted)


class _NullCtx:
    def check_cancelled(self):
        return None

    def progress(self, *_args):
        return None


def descriptor():
    from larm_adapter_sdk.capabilities import digest
    def schema(maximum):
        return {"type":"object","additionalProperties":False,"required":["sha256","size_bytes"],"properties":{"sha256":{"type":"string","pattern":"^[0-9a-f]{64}$"},"size_bytes":{"type":"string","pattern":"^(0|[1-9][0-9]*)$","maxLength":19,"x-maximum-decimal":str(maximum)}}}
    input_schema,output_schema = schema(64 << 20),schema(80 << 20)
    fixture_input = build_safetensors_fixture()
    fixture_output = _conversion_fixture(fixture_input)
    fixture = {"input":{"sha256":hashlib.sha256(fixture_input).hexdigest(),"size_bytes":str(len(fixture_input))},"output":{"sha256":hashlib.sha256(fixture_output).hexdigest(),"size_bytes":str(len(fixture_output))}}
    return {"id":"vendor.product.gguf","version":"1.0","input_schema":input_schema,"output_schema":output_schema,"input_sha256":digest(input_schema),"output_sha256":digest(output_schema),"mapping":"product.safetensors.gguf.v1","estimator_id":"product.gguf.buffers.v1","fixture":fixture,"fixture_sha256":digest(fixture)}


def create(repo,package,work,data):
    sys.path.insert(0,str(repo / "adapters/sdk/python"))
    if not isinstance(data,bytes) or len(data) > 64 << 20:
        raise ValueError("Fixture propia binaria de hasta 64 MiB requerida")
    spec = importlib.util.spec_from_file_location("product_gguf_packaging",repo / "adapters/source/reference/package.py")
    shared = importlib.util.module_from_spec(spec); spec.loader.exec_module(shared)
    package,work = Path(package).resolve(),Path(work).resolve()
    package.mkdir(parents=True,exist_ok=False); work.mkdir(parents=True,exist_ok=False)
    shutil.copytree(repo / "adapters/transformation/gguf",package / "transformation",ignore=shutil.ignore_patterns("__pycache__","package.py","tests"))
    shutil.copytree(repo / "adapters/sdk/python/larm_adapter_sdk",package / "sdk/larm_adapter_sdk",ignore=shutil.ignore_patterns("__pycache__"))
    for name in ("contracts/schemas/larm.schema.json","contracts/schemas/domain.schema.json","contracts/adapter-methods.json","contracts/capability-contracts.json","adapters/schemas/frame.request.schema.json","adapters/schemas/frame.response.schema.json","adapters/schemas/frame.progress.schema.json"):
        target = package / "docs" / name
        target.parent.mkdir(parents=True,exist_ok=True); shutil.copyfile(repo / "docs" / name,target)
    for name in ("method-schemas","capability-schemas"):
        shutil.copytree(repo / "docs/adapters" / name,package / "docs/adapters" / name,ignore=shutil.ignore_patterns("README.md"))
    bill = shared.interpreter(repo,package / "bin")
    shared.module_paths(package / "bin", "transformation")
    (package / "dependencies.lock.json").write_text(json.dumps(bill,indent=2),encoding="utf-8")
    for name in ("input","profile","state","output"):
        (work/name).mkdir()
    (work / "input/fixture.safetensors").write_bytes(data)
    entry = descriptor()
    (package / "product-capabilities.json").write_text(json.dumps({"schema_version":"1.0","entries":[entry]},indent=2),encoding="utf-8")
    profile = {"input_variant_id":str(uuid.uuid7()),"storage_location_id":str(uuid.uuid7()),"input":{"relative_path":"fixture.safetensors","sha256":hashlib.sha256(data).hexdigest(),"size_bytes":str(len(data))}}
    (work / "profile/profile.json").write_text(json.dumps(profile,indent=2),encoding="utf-8")
    table = json.loads((repo / "docs/contracts/adapter-methods.json").read_text(encoding="utf-8"))
    roots = ["product-package"]+["product-gguf-"+name for name in ("input","profile","state","output")]
    manifest = {"id":"product.transformation.gguf","version":"1.1.0","family":"transformation","protocol_version":"1.0","publisher":"LARM product gguf","executable":{"relative_path":"bin/python.exe","args":["-I","-B","-m","main"]},"platforms":["windows"],"architectures":["x86_64"],"methods":table["mandatory_common"]+[m["method"] for m in table["methods"] if m["family"]=="transformation"],"permissions":{"read_roots":roots,"write_roots":["product-gguf-state","product-gguf-output"],"network_origins":[],"process_allowlist":[],"secret_refs":[]},"capabilities":[entry["id"]],"package_sha256":shared.package_hash(package),"signature":None,"installation_strategies":[]}
    (package / "manifest.json").write_text(json.dumps(manifest,indent=2),encoding="utf-8")
    return manifest,profile,entry
