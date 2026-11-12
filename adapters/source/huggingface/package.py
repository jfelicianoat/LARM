"""Build a self-contained, offline Hugging Face source worker package."""
from __future__ import annotations

import hashlib
import importlib.metadata
import importlib.util
import json
import platform
import shutil
import sys
import uuid
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[3] / "adapters/sdk/python"))
from adapter import _profile_origin

DEFAULT_ORIGINS = [
    "https://huggingface.co",
    "https://cdn-lfs.huggingface.co",
    "https://cdn-lfs-us-1.huggingface.co",
    "https://cdn-lfs-eu-1.huggingface.co",
    "https://cas-bridge.xethub.hf.co",
    "https://cas-server.xethub.hf.co",
]


def sha(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def package_hash(root: Path) -> str:
    entries = sorted(
        (path.relative_to(root).as_posix().lower(), sha(path))
        for path in root.rglob("*")
        if path.is_file() and path.relative_to(root).as_posix() != "manifest.json"
    )
    payload = "".join(f"{name}\0{digest}\n" for name, digest in entries).encode()
    return hashlib.sha256(payload).hexdigest()


def interpreter(repo: Path, destination: Path) -> dict:
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
    versions = {}
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
    return {"python": platform.python_version(), "packages": versions, "scope": "huggingface source adapter only"}


def create(repo: Path, root: Path, work: Path, base_url: str = "https://huggingface.co", source_id: str | None = None, origins: list[str] | None = None) -> dict:
    """Create a portable worker plus its private profile binding."""
    repo, root, work = repo.resolve(strict=True), root.resolve(), work.resolve()
    if root.exists() or work.exists():
        raise FileExistsError("package/work directories must be new")
    base_origin = _profile_origin(base_url)
    if origins is None:
        origins = DEFAULT_ORIGINS if base_url.startswith("https://huggingface.co") else [base_url]
    normalized_origins = {_profile_origin(origin) for origin in origins}
    if base_origin not in normalized_origins:
        raise ValueError("the profile API origin must be in the package network permissions")
    root.mkdir(parents=True)
    work.mkdir(parents=True)
    source_id = source_id or str(uuid.uuid7())
    origins = list(dict.fromkeys(origins))

    source = repo / "adapters/source/huggingface"
    shutil.copytree(source, root / "source", ignore=shutil.ignore_patterns("__pycache__", "package.py"))
    shutil.copytree(repo / "adapters/sdk/python/larm_adapter_sdk", root / "sdk/larm_adapter_sdk", ignore=shutil.ignore_patterns("__pycache__"))
    for name in ("larm.schema.json", "domain.schema.json"):
        target = root / "docs/contracts/schemas" / name
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(repo / "docs/contracts/schemas" / name, target)
    for name in ("adapter-methods.json",):
        target = root / "docs/contracts" / name
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(repo / "docs/contracts" / name, target)
    for name in ("request", "response", "progress"):
        target = root / "docs/adapters/schemas" / f"frame.{name}.schema.json"
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(repo / "docs/adapters/schemas" / f"frame.{name}.schema.json", target)
    shutil.copytree(repo / "docs/adapters/method-schemas", root / "docs/adapters/method-schemas", ignore=shutil.ignore_patterns("README.md"))
    dependencies = interpreter(repo, root / "bin")
    (root / "dependencies.lock.json").write_text(json.dumps(dependencies, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")

    table = json.loads((repo / "docs/contracts/adapter-methods.json").read_text(encoding="utf-8"))
    methods = table["mandatory_common"] + [
        entry["method"] for entry in table["methods"]
        if entry["family"] == "source" and entry["required"]
    ] + ["source.download_chunk"]
    manifest = {
        "id": "huggingface.source", "version": "1.0.0", "family": "source",
        "protocol_version": "1.0", "publisher": "LARM",
        "executable": {"relative_path": "bin/python.exe", "args": ["-I", "-B", "-m", "main"]},
        "platforms": ["windows"], "architectures": ["x86_64"], "methods": methods,
        "permissions": {
            "read_roots": ["huggingface-package", "huggingface-work"],
            "write_roots": ["huggingface-work"], "network_origins": origins,
            "process_allowlist": [], "secret_refs": [],
        },
        "capabilities": [], "package_sha256": "0" * 64,
        "signature": None, "installation_strategies": [],
    }
    (root / "manifest.json").write_text(json.dumps(manifest, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    manifest["package_sha256"] = package_hash(root)
    (root / "manifest.json").write_text(json.dumps(manifest, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    (work / "profile.json").write_text(json.dumps({"source_id": source_id, "base_url": base_url}, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    return manifest


def main() -> int:
    import argparse

    parser = argparse.ArgumentParser()
    parser.add_argument("--repo", type=Path, default=Path(__file__).resolve().parents[3])
    parser.add_argument("--package", type=Path, required=True)
    parser.add_argument("--work", type=Path, required=True)
    parser.add_argument("--base-url", default="https://huggingface.co")
    args = parser.parse_args()
    manifest = create(args.repo, args.package, args.work, args.base_url)
    print(json.dumps({"id": manifest["id"], "package_sha256": manifest["package_sha256"], "source_id": json.loads((args.work / "profile.json").read_text(encoding="utf-8"))["source_id"]}))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
