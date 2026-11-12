"""Paquete externo completo con Python fijado; fixture binaria propiedad del caller."""
import base64
import hashlib
import importlib.util
import json
import shutil
import sys
import uuid
from pathlib import Path


def descriptor():
    from larm_adapter_sdk.capabilities import digest
    def schema(maximum):
        return {"type":"object","additionalProperties":False,"required":["sha256","size_bytes"],"properties":{"sha256":{"type":"string","pattern":"^[0-9a-f]{64}$"},"size_bytes":{"type":"string","pattern":"^(0|[1-9][0-9]*)$","maxLength":19,"x-maximum-decimal":str(maximum)}}}
    input_schema,output_schema = schema(64 << 20),schema(4 * (((64 << 20)+2)//3))
    source = b"LARM\n"
    def observed(data): return {"sha256":hashlib.sha256(data).hexdigest(),"size_bytes":str(len(data))}
    fixture = {"input":observed(source),"output":observed(base64.b64encode(source))}
    return {"id":"vendor.reference.base64","version":"1.0","input_schema":input_schema,"output_schema":output_schema,"input_sha256":digest(input_schema),"output_sha256":digest(output_schema),"mapping":"reference.binary.base64.v1","estimator_id":"reference.base64.buffers.v1","fixture":fixture,"fixture_sha256":digest(fixture)}


def create(repo,package,work,data):
    sys.path.insert(0,str(repo / "adapters/sdk/python"))
    if not isinstance(data,bytes) or len(data) > 64 << 20:
        raise ValueError("Fixture propia binaria de hasta 64 MiB requerida")
    spec = importlib.util.spec_from_file_location("reference_python_packaging",repo / "adapters/source/reference/package.py")
    shared = importlib.util.module_from_spec(spec); spec.loader.exec_module(shared)
    package,work = Path(package).resolve(),Path(work).resolve()
    package.mkdir(parents=True,exist_ok=False); work.mkdir(parents=True,exist_ok=False)
    shutil.copytree(repo / "adapters/transformation/reference",package / "transformation",ignore=shutil.ignore_patterns("__pycache__","package.py"))
    shutil.copytree(repo / "adapters/sdk/python/larm_adapter_sdk",package / "sdk/larm_adapter_sdk",ignore=shutil.ignore_patterns("__pycache__"))
    for name in ("contracts/schemas/larm.schema.json","contracts/schemas/domain.schema.json","contracts/adapter-methods.json","contracts/capability-contracts.json","adapters/schemas/frame.request.schema.json","adapters/schemas/frame.response.schema.json","adapters/schemas/frame.progress.schema.json"):
        target = package / "docs" / name
        target.parent.mkdir(parents=True,exist_ok=True); shutil.copyfile(repo / "docs" / name,target)
    for name in ("method-schemas","capability-schemas"):
        shutil.copytree(repo / "docs/adapters" / name,package / "docs/adapters" / name,ignore=shutil.ignore_patterns("README.md"))
    bill = shared.interpreter(repo,package / "bin")
    shared.module_paths(package / "bin", "transformation")
    (package / "dependencies.lock.json").write_text(json.dumps(bill,indent=2),encoding="utf-8")
    entry = descriptor()
    (package / "reference-capabilities.json").write_text(json.dumps({"schema_version":"1.0","entries":[entry]},indent=2),encoding="utf-8")
    for name in ("input","profile","state","output"):
        (work/name).mkdir()
    profile = {"input_variant_id":str(uuid.uuid7()),"storage_location_id":str(uuid.uuid7()),"input":{"relative_path":"fixture.bin","sha256":hashlib.sha256(data).hexdigest(),"size_bytes":str(len(data))}}
    (work / "input/fixture.bin").write_bytes(data)
    (work / "profile/profile.json").write_text(json.dumps(profile,indent=2),encoding="utf-8")
    table = json.loads((repo / "docs/contracts/adapter-methods.json").read_text(encoding="utf-8"))
    roots = ["reference-package"]+["reference-transform-"+name for name in ("input","profile","state","output")]
    manifest = {"id":"reference.transformation.base64","version":"1.0.0","family":"transformation","protocol_version":"1.0","publisher":"LARM controlled reference","executable":{"relative_path":"bin/python.exe","args":["-I","-B","-m","main"]},"platforms":["windows"],"architectures":["x86_64"],"methods":table["mandatory_common"]+[m["method"] for m in table["methods"] if m["family"]=="transformation"],"permissions":{"read_roots":roots,"write_roots":["reference-transform-state","reference-transform-output"],"network_origins":[],"process_allowlist":[],"secret_refs":[]},"capabilities":[entry["id"]],"package_sha256":shared.package_hash(package),"signature":None,"installation_strategies":[]}
    (package / "manifest.json").write_text(json.dumps(manifest,indent=2),encoding="utf-8")
    return manifest,profile,entry
