"""Paquete autónomo protocol: SDK/Python fijados; ningún cambio en Core."""
import importlib.util
import json
import shutil
import sys
import uuid
from pathlib import Path


def descriptor():
    from larm_adapter_sdk.capabilities import digest
    input_schema = {"type":"object","additionalProperties":False,"required":["text"],"properties":{"text":{"type":"string","maxLength":2048}}}
    output_schema = {"type":"object","additionalProperties":False,"required":["echo"],"properties":{"echo":input_schema}}
    fixture = {"input":{"text":"LARM ✓"},"output":{"echo":{"text":"LARM ✓"}}}
    return {"id":"vendor.reference.echo","version":"1.0","input_schema":input_schema,"output_schema":output_schema,"input_sha256":digest(input_schema),"output_sha256":digest(output_schema),"mapping":"reference.http.echo.v1","estimator_id":"reference.echo.buffers.v1","fixture":fixture,"fixture_sha256":digest(fixture)}


def create(repo, package, work, base_url):
    sys.path.insert(0, str(repo / "adapters/sdk/python"))
    from larm_adapter_sdk.capabilities import digest
    spec = importlib.util.spec_from_file_location("reference_python_packaging", repo / "adapters/source/reference/package.py")
    shared = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(shared)
    package, work = Path(package).resolve(), Path(work).resolve()
    package.mkdir(parents=True, exist_ok=False)
    work.mkdir(parents=True, exist_ok=False)
    shutil.copytree(repo / "adapters/protocol/reference", package / "protocol", ignore=shutil.ignore_patterns("__pycache__", "package.py"))
    shutil.copytree(repo / "adapters/sdk/python/larm_adapter_sdk", package / "sdk/larm_adapter_sdk", ignore=shutil.ignore_patterns("__pycache__"))
    for relative in ("contracts/schemas/larm.schema.json","contracts/schemas/domain.schema.json","contracts/adapter-methods.json","contracts/capability-contracts.json","adapters/schemas/frame.request.schema.json","adapters/schemas/frame.response.schema.json","adapters/schemas/frame.progress.schema.json"):
        target = package / "docs" / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(repo / "docs" / relative, target)
    for name in ("method-schemas", "capability-schemas"):
        shutil.copytree(repo / "docs/adapters" / name, package / "docs/adapters" / name, ignore=shutil.ignore_patterns("README.md"))
    bill = shared.interpreter(repo, package / "bin")
    shared.module_paths(package / "bin", "protocol")
    (package / "dependencies.lock.json").write_text(json.dumps(bill, indent=2), encoding="utf-8")
    entry = descriptor()
    (package / "reference-capabilities.json").write_text(json.dumps({"schema_version":"1.0","entries":[entry]},ensure_ascii=False,indent=2), encoding="utf-8")
    policy = {"base_url":base_url,"mapping":"reference.http.echo.v1","max_request_bytes":16 << 10,"max_response_bytes":32 << 10,"timeout_seconds":2}
    (package / "reference-protocol.json").write_text(json.dumps(policy,indent=2), encoding="utf-8")
    profile = {name:str(uuid.uuid7()) for name in ("endpoint_id","variant_id","lease_id")}
    (work / "profile.json").write_text(json.dumps(profile,indent=2), encoding="utf-8")
    table = json.loads((repo / "docs/contracts/adapter-methods.json").read_text(encoding="utf-8"))
    manifest = {"id":"reference.protocol.echo","version":"1.0.0","family":"protocol","protocol_version":"1.0","publisher":"LARM controlled reference","executable":{"relative_path":"bin/python.exe","args":["-I","-B","-m","main"]},"platforms":["windows"],"architectures":["x86_64"],"methods":table["mandatory_common"]+[m["method"] for m in table["methods"] if m["family"]=="protocol"],"permissions":{"read_roots":["reference-package","reference-protocol-work"],"write_roots":[],"network_origins":[base_url],"process_allowlist":[],"secret_refs":[]},"capabilities":[entry["id"]],"package_sha256":shared.package_hash(package),"signature":None,"installation_strategies":[]}
    (package / "manifest.json").write_text(json.dumps(manifest,indent=2), encoding="utf-8")
    return manifest, profile, entry
