"""Materializa una referencia controlada usando el worker Rust del SDK P023."""
import copy
import hashlib
import json
import shutil
from pathlib import Path


def package_hash(root):
    files = sorted((p.relative_to(root).as_posix().lower(), hashlib.sha256(p.read_bytes()).hexdigest()) for p in root.rglob("*") if p.is_file() and p.name != "manifest.json")
    return hashlib.sha256("".join(f"{name}\0{digest}\n" for name, digest in files).encode()).hexdigest()


def create(repo: Path, root: Path, *, version_delay_ms=0, version_flood=False):
    root = root.resolve()
    root.mkdir(parents=True, exist_ok=False)
    (root / "bin").mkdir()
    for source, name in (("larm-reference-runtime.exe", "larm-generic-adapter.exe"), ("larm-reference-echo.exe", "echo.exe")):
        shutil.copyfile(repo / "target/debug" / source, root / "bin" / name)
    (root / "models").mkdir()
    model = root / "models/fixture.bin"
    model.write_bytes(b"P025 self-authored test data; not an inference model\n")
    table = json.loads((repo / "docs/contracts/adapter-methods.json").read_text(encoding="utf-8"))
    methods = table["mandatory_common"] + [item["method"] for item in table["methods"] if item["family"] == "runtime"]
    manifest = {"id": "reference.echo", "version": "1.0.0", "family": "runtime", "protocol_version": "1.0", "publisher": "LARM controlled reference", "executable": {"relative_path": "bin/larm-generic-adapter.exe", "args": []}, "platforms": ["windows"], "architectures": ["x86_64"], "methods": methods, "permissions": {"read_roots": [str(root)], "write_roots": [], "network_origins": [], "process_allowlist": [], "secret_refs": []}, "capabilities": ["vendor.reference.echo"], "package_sha256": "0" * 64, "signature": None, "installation_strategies": []}
    shutil.copyfile(repo / "adapters/runtime/reference/capabilities.json",root / "reference-capabilities.json")
    executable = str(root / "bin/echo.exe")
    document = {"schema_version": "1.0", "manifest": copy.deepcopy(manifest), "version_probe": {"executable": executable, "args": ["--version", "--version-delay-ms", str(version_delay_ms)], "parse_mode": "EXACT_TEXT", "json_field": None}, "start": {"executable": executable, "cwd": str(root), "args": ["--port", "{PORT}"]}, "stop": {"mode": "HTTP", "endpoint": "http://127.0.0.1:{PORT}/shutdown"}, "health": {"url": "http://127.0.0.1:{PORT}/health", "expected_status": 200, "json_pointer": "/status", "expected_value": "ok"}, "protocol_profile_id": "reference.echo", "formats": ["BINARY"], "capabilities": manifest["capabilities"], "environment": {"allowlist": [], "variables": []}}
    if version_flood:
        document["version_probe"]["args"].append("--version-flood")
    (root / "declarative.json").write_text(json.dumps(document, indent=2), encoding="utf-8")
    manifest["package_sha256"] = package_hash(root)
    (root / "manifest.json").write_text(json.dumps(manifest, indent=2), encoding="utf-8")
    return root, manifest, model
