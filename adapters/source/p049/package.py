"""Build one isolated Windows source worker for a P049 provider."""
from __future__ import annotations

import argparse
import hashlib
import importlib.metadata
import importlib.util
import json
import platform
import shutil
import sys
import uuid
from pathlib import Path

REPO = Path(__file__).resolve().parents[3]
PROVIDERS = {
    "KAGGLE": "kaggle",
    "OLLAMA_LIBRARY": "ollama-library",
    "OPENXLAB": "openxlab",
    "NGC": "ngc",
    "CIVITAI": "civitai",
}


def package_hash(root: Path) -> str:
    entries = sorted(
        (path.relative_to(root).as_posix().lower(), hashlib.sha256(path.read_bytes()).hexdigest())
        for path in root.rglob("*")
        if path.is_file() and path.relative_to(root).as_posix() != "manifest.json"
    )
    payload = "".join(f"{name}\0{digest}\n" for name, digest in entries).encode()
    return hashlib.sha256(payload).hexdigest()


def interpreter(repo: Path, destination: Path) -> dict:
    """Copy the repository-pinned Python runtime and schema dependencies."""
    lock = json.loads((repo / "environment.lock.json").read_text(encoding="utf-8"))
    if platform.python_version() != lock["python"]["version"]:
        raise ValueError("Python does not match the repository lock")
    base = Path(sys.base_prefix)
    destination.mkdir()
    for path in [base / "python.exe", *base.glob("*.dll")]:
        shutil.copyfile(path, destination / path.name)
    shutil.copytree(base / "DLLs", destination / "DLLs", ignore=shutil.ignore_patterns("__pycache__"))
    shutil.copytree(
        base / "Lib", destination / "Lib",
        ignore=shutil.ignore_patterns("site-packages", "__pycache__", "test", "tests", "ensurepip", "idlelib", "turtledemo"),
    )
    sites = destination / "Lib/site-packages"
    sites.mkdir()
    modules = ("jsonschema", "referencing", "attrs", "attr", "rpds", "jsonschema_specifications", "typing_extensions", "rfc3339_validator", "rfc8785", "six")
    for name in modules:
        spec = importlib.util.find_spec(name)
        if spec is None or spec.origin is None:
            raise ValueError(f"Missing pinned package module: {name}")
        source = Path(spec.origin)
        if source.name == "__init__.py":
            shutil.copytree(source.parent, sites / source.parent.name, ignore=shutil.ignore_patterns("__pycache__", "tests", "test"))
        else:
            shutil.copyfile(source, sites / source.name)
    pinned = dict(
        line.strip().split("==")
        for line in (repo / "tools/requirements.txt").read_text(encoding="utf-8").splitlines()
        if "==" in line
    )
    distributions = ("jsonschema", "referencing", "attrs", "rpds-py", "jsonschema-specifications", "typing_extensions", "rfc3339-validator", "rfc8785", "six")
    versions = {}
    for name in distributions:
        distribution = importlib.metadata.distribution(name)
        if distribution.version != pinned[name]:
            raise ValueError(f"Package does not match the repository lock: {name}")
        versions[name] = distribution.version
        metadata = next(item for item in distribution.files if item.name == "METADATA" and ".dist-info" in str(item))
        directory = distribution.locate_file(metadata).parent
        shutil.copytree(directory, sites / directory.name)
    tag = f"python{sys.version_info.major}{sys.version_info.minor}"
    (destination / f"{tag}._pth").write_text(".\nDLLs\nLib\nLib/site-packages\n../sdk\n../source\n", encoding="utf-8")
    license_path = base / "LICENSE.txt"
    if not license_path.is_file():
        raise ValueError("The Python license file is missing")
    shutil.copyfile(license_path, destination / "LICENSE.txt")
    return {"python": platform.python_version(), "packages": versions, "scope": "P049 source adapters only"}


def create(repo: Path, root: Path, work: Path, provider: str, *, source_id: str | None = None,
           portable: bool = True) -> dict:
    repo, root, work = repo.resolve(strict=True), root.resolve(), work.resolve()
    provider = provider.upper()
    if provider not in PROVIDERS:
        raise ValueError("unsupported P049 provider")
    if root.exists() or work.exists():
        raise FileExistsError("package/work directories must be new")
    source_id = source_id or str(uuid.uuid7())
    parsed_id = uuid.UUID(source_id)
    if parsed_id.version != 7 or str(parsed_id) != source_id:
        raise ValueError("source_id must be canonical UUIDv7")

    slug = PROVIDERS[provider]
    config_path = repo / "adapters/source" / slug / "provider.json"
    config = json.loads(config_path.read_text(encoding="utf-8"))
    if config.get("provider") != provider or config.get("adapter_id") != f"larm.source.{slug}":
        raise ValueError("provider profile does not match its isolated adapter")

    root.mkdir(parents=True)
    work.mkdir(parents=True)
    (root / "source").mkdir()
    shutil.copyfile(repo / "adapters/source/p049/main.py", root / "source/main.py")
    shutil.copyfile(repo / "adapters/source/p049/adapter.py", root / "source/p049_adapter.py")
    shutil.copyfile(repo / "adapters/source" / slug / "adapter.py", root / "source/provider_adapter.py")
    shutil.copyfile(config_path, root / "source/provider.json")
    shutil.copyfile(repo / "adapters/source" / slug / "capabilities.json", root / "source/capabilities.json")
    shutil.copytree(repo / "adapters/sdk/python/larm_adapter_sdk", root / "sdk/larm_adapter_sdk",
                    ignore=shutil.ignore_patterns("__pycache__"))

    for name in ("larm.schema.json", "domain.schema.json"):
        target = root / "docs/contracts/schemas" / name
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(repo / "docs/contracts/schemas" / name, target)
    target = root / "docs/contracts/adapter-methods.json"
    target.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(repo / "docs/contracts/adapter-methods.json", target)
    for name in ("request", "response", "progress"):
        target = root / "docs/adapters/schemas" / f"frame.{name}.schema.json"
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(repo / "docs/adapters/schemas" / f"frame.{name}.schema.json", target)
    shutil.copytree(repo / "docs/adapters/method-schemas", root / "docs/adapters/method-schemas",
                    ignore=shutil.ignore_patterns("README.md"))

    if portable:
        dependencies = interpreter(repo, root / "bin")
    else:
        (root / "bin").mkdir()
        dependencies = {"python": "system", "packages": "repository-locked", "scope": "test-only package construction"}
    (root / "dependencies.lock.json").write_text(json.dumps(dependencies, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")

    table = json.loads((repo / "docs/contracts/adapter-methods.json").read_text(encoding="utf-8"))
    methods = table["mandatory_common"] + [
        entry["method"] for entry in table["methods"]
        if entry["family"] == "source" and entry["required"]
    ] + ["source.download_chunk"]
    origins = list(config["network_origins"])
    manifest = {
        "id": config["adapter_id"], "version": "1.0.0", "family": "source",
        "protocol_version": "1.0", "publisher": "LARM",
        "executable": {"relative_path": "bin/python.exe", "args": ["-I", "-B", "-m", "main"]},
        "platforms": ["windows"], "architectures": ["x86_64"], "methods": methods,
        "permissions": {
            "read_roots": ["p049-package", "p049-work"], "write_roots": ["p049-work"],
            "network_origins": origins, "process_allowlist": [], "secret_refs": [],
        },
        "capabilities": [], "package_sha256": "0" * 64,
        "signature": None, "installation_strategies": [],
    }
    (root / "manifest.json").write_text(json.dumps(manifest, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    manifest["package_sha256"] = package_hash(root)
    (root / "manifest.json").write_text(json.dumps(manifest, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    (work / "profile.json").write_text(json.dumps({"provider": provider, "source_id": source_id}, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    return manifest


def main(provider: str | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", type=Path, default=REPO)
    parser.add_argument("--package", type=Path, required=True)
    parser.add_argument("--work", type=Path, required=True)
    parser.add_argument("--provider", choices=tuple(PROVIDERS), default=provider)
    args = parser.parse_args()
    if not args.provider:
        parser.error("--provider is required")
    manifest = create(args.repo, args.package, args.work, args.provider)
    profile = json.loads((args.work / "profile.json").read_text(encoding="utf-8"))
    print(json.dumps({"id": manifest["id"], "package_sha256": manifest["package_sha256"],
                      "source_id": profile["source_id"]}))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
