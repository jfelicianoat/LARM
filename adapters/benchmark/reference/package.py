"""Paquete benchmark autónomo con suite/fixture/settings fijados por hashes JCS."""
import importlib.util
import json
import shutil
import sys
import uuid
from pathlib import Path


def load(name,path):
    spec=importlib.util.spec_from_file_location(name,path)
    module=importlib.util.module_from_spec(spec); spec.loader.exec_module(module)
    return module


def create(repo,package,work,base_url):
    sys.path.insert(0,str(repo/"adapters/sdk/python"))
    shared=load("reference_python_packaging",repo/"adapters/source/reference/package.py")
    protocol=load("reference_echo_descriptor",repo/"adapters/protocol/reference/package.py")
    config=load("reference_benchmark_config",repo/"adapters/benchmark/reference/config.py")
    from larm_adapter_sdk.capabilities import digest
    package,work=Path(package).resolve(),Path(work).resolve()
    package.mkdir(parents=True,exist_ok=False); work.mkdir(parents=True,exist_ok=False)
    shutil.copytree(repo/"adapters/benchmark/reference",package/"benchmark",ignore=shutil.ignore_patterns("__pycache__","package.py"))
    shutil.copytree(repo/"adapters/sdk/python/larm_adapter_sdk",package/"sdk/larm_adapter_sdk",ignore=shutil.ignore_patterns("__pycache__"))
    for name in ("contracts/schemas/larm.schema.json","contracts/schemas/domain.schema.json","contracts/adapter-methods.json","contracts/capability-contracts.json","adapters/schemas/frame.request.schema.json","adapters/schemas/frame.response.schema.json","adapters/schemas/frame.progress.schema.json"):
        target=package/"docs"/name; target.parent.mkdir(parents=True,exist_ok=True)
        shutil.copyfile(repo/"docs"/name,target)
    for name in ("method-schemas","capability-schemas"):
        shutil.copytree(repo/"docs/adapters"/name,package/"docs/adapters"/name,ignore=shutil.ignore_patterns("README.md"))
    bill=shared.interpreter(repo,package/"bin")
    shared.module_paths(package / "bin", "benchmark")
    (package/"dependencies.lock.json").write_text(json.dumps(bill,indent=2),encoding="utf-8")
    entry=protocol.descriptor()
    (package/"reference-capabilities.json").write_text(json.dumps({"schema_version":"1.0","entries":[entry]},indent=2),encoding="utf-8")
    policy={"base_url":base_url,"suite":config.SUITE,"fixture":entry["fixture"],"settings":config.SETTINGS}
    (package/"reference-benchmark.json").write_text(json.dumps(policy,ensure_ascii=False,indent=2),encoding="utf-8")
    for name in ("profile","state","output"): (work/name).mkdir()
    profile={name:str(uuid.uuid7()) for name in ("variant_id","runtime_version_id","backend_id","launch_profile_id")}
    (work/"profile/profile.json").write_text(json.dumps(profile,indent=2),encoding="utf-8")
    table=json.loads((repo/"docs/contracts/adapter-methods.json").read_text(encoding="utf-8"))
    roots=["reference-package"]+["reference-benchmark-"+name for name in ("profile","state","output")]
    manifest={"id":"reference.benchmark.echo","version":"1.0.0","family":"benchmark","protocol_version":"1.0","publisher":"LARM controlled reference","executable":{"relative_path":"bin/python.exe","args":["-I","-B","-m","main"]},"platforms":["windows"],"architectures":["x86_64"],"methods":table["mandatory_common"]+[m["method"] for m in table["methods"] if m["family"]=="benchmark"],"permissions":{"read_roots":roots,"write_roots":["reference-benchmark-state","reference-benchmark-output"],"network_origins":[base_url],"process_allowlist":[],"secret_refs":[]},"capabilities":[entry["id"]],"package_sha256":shared.package_hash(package),"signature":None,"installation_strategies":[]}
    (package/"manifest.json").write_text(json.dumps(manifest,indent=2),encoding="utf-8")
    request=profile|{"suite_id":config.SUITE["id"],"fixture_hash":digest(policy["fixture"]),"settings_hash":digest(config.SETTINGS)}
    return manifest,profile,policy,request
