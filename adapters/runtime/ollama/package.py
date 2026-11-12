"""Build a portable SDK package from locally pinned Python; no installers."""
import importlib.util
import json
from pathlib import Path
import shutil


def create(repo, root, work, model_store):
    repo, root, work = Path(repo).resolve(strict=True), Path(root).resolve(), Path(work).resolve()
    model_store = Path(model_store).resolve(strict=True)
    if root.exists() or work.exists() or root == work or root.is_relative_to(work) or work.is_relative_to(root):
        raise ValueError("package/work must be distinct new directories")
    root.mkdir(parents=True)
    work.mkdir(parents=True)
    spec = importlib.util.spec_from_file_location("ollama_reference_packaging", repo / "adapters/source/reference/package.py")
    shared = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(shared)
    bill = shared.interpreter(repo, root / "bin")
    shared.module_paths(root / "bin", "runtime")
    bill["scope"] = "Pinned Python SDK for Ollama read-only store inspection; not an AI runtime profile"
    (root / "dependencies.lock.json").write_text(json.dumps(bill,indent=2)+"\n",encoding="utf-8")
    (root / "runtime").mkdir()
    for name in ("store.py", "adapter.py", "main.py"):
        shutil.copyfile(repo / "adapters/runtime/ollama" / name, root / "runtime" / name)
    shutil.copytree(repo / "adapters/sdk/python/larm_adapter_sdk", root / "sdk/larm_adapter_sdk",ignore=shutil.ignore_patterns("__pycache__"))
    for name in ("contracts/schemas/larm.schema.json", "contracts/schemas/domain.schema.json", "contracts/adapter-methods.json",
                 "adapters/schemas/frame.request.schema.json", "adapters/schemas/frame.response.schema.json", "adapters/schemas/frame.progress.schema.json"):
        target = root / "docs" / name
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(repo / "docs" / name,target)
    shutil.copytree(repo / "docs/adapters/method-schemas", root / "docs/adapters/method-schemas",ignore=shutil.ignore_patterns("README.md"))
    table = json.loads((repo / "docs/contracts/adapter-methods.json").read_bytes())
    manifest = {"id":"larm.runtime.ollama", "version":"0.1.0", "family":"runtime", "protocol_version":"1.0",
                "publisher":"LARM development", "executable":{"relative_path":"bin/python.exe", "args":["-I","-B","-m","main"]},
                "platforms":["windows"], "architectures":["x86_64"],
                "methods":table["mandatory_common"]+[entry["method"] for entry in table["methods"] if entry["family"]=="runtime" and entry["required"]],
                "permissions":{"read_roots":["ollama-package","ollama-work","ollama-model-store"],
                               "write_roots":[],"network_origins":[],"process_allowlist":[],"secret_refs":[]},
                "capabilities":[],"installation_strategies":[],"package_sha256":shared.package_hash(root),"signature":None}
    (root / "manifest.json").write_text(json.dumps(manifest,indent=2)+"\n",encoding="utf-8")
    (work / "profile.json").write_text(json.dumps({"model_store_root":str(model_store)},indent=2)+"\n",encoding="utf-8")
    return manifest
