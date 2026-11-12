"""Build a profile-scoped, self-contained Windows P048 source worker package."""
from __future__ import annotations

import argparse
import hashlib
import json
import shutil
import subprocess
import uuid
from pathlib import Path
from urllib.parse import urlsplit
from urllib.parse import parse_qsl


def package_hash(root: Path) -> str:
    rows = sorted(
        (path.relative_to(root).as_posix().lower(), hashlib.sha256(path.read_bytes()).hexdigest())
        for path in root.rglob("*")
        if path.is_file() and path.relative_to(root).as_posix() != "manifest.json"
    )
    return hashlib.sha256("".join(f"{name}\0{digest}\n" for name, digest in rows).encode()).hexdigest()


def origin(value: str) -> str:
    parsed = urlsplit(value)
    if parsed.scheme not in ("https", "http") or not parsed.hostname or parsed.username or parsed.password or parsed.fragment:
        raise ValueError("URL origin is invalid")
    sensitive = ("token", "signature", "secret", "credential", "password", "apikey", "cookie")
    if any(any(part in key.lower().replace("-", "").replace("_", "") for part in sensitive) for key, _ in parse_qsl(parsed.query)):
        raise ValueError("sensitive query parameters are forbidden in source URLs")
    if parsed.scheme == "http" and parsed.hostname not in ("localhost", "127.0.0.1", "::1"):
        raise ValueError("clear-text HTTP is allowed only on loopback")
    host = f"[{parsed.hostname}]" if ":" in parsed.hostname else parsed.hostname.lower()
    port = parsed.port or (443 if parsed.scheme == "https" else 80)
    return f"{parsed.scheme}://{host}:{port}"


def create(repo: Path, package: Path, work: Path, source_type: str, *, source_id: str | None = None,
           root: str | None = None, url: str | None = None, checksum: str | None = None,
           repository: str | None = None, binary: Path | None = None) -> dict:
    repo = repo.resolve(strict=True)
    if package.exists() or work.exists():
        raise FileExistsError("package and work directories must be new")
    source_type = source_type.upper()
    if source_type not in {"GITHUB", "HTTP", "LOCAL", "UNC_PRIVATE"}:
        raise ValueError("unsupported P048 source type; GIT_LFS is deliberately not packaged")
    source_id = source_id or str(uuid.uuid7())
    uuid.UUID(source_id)
    if source_type == "GITHUB":
        if not repository or len(repository.split("/")) != 2 or any(part in ("", ".", "..") for part in repository.split("/")):
            raise ValueError("GitHub requires one configured owner/repository")
        origins = ["https://api.github.com:443", "https://github.com:443", "https://objects.githubusercontent.com:443", "https://release-assets.githubusercontent.com:443"]
        profile = {"source_id": source_id, "source_type": source_type, "repository": repository}
    elif source_type == "HTTP":
        if not url:
            raise ValueError("HTTP source requires an explicit URL")
        if checksum is not None and (len(checksum) != 64 or any(c not in "0123456789abcdef" for c in checksum)):
            raise ValueError("checksum must be lowercase SHA-256")
        origins = [origin(url)]
        profile = {"source_id": source_id, "source_type": source_type, "url": url, "sha256": checksum}
    else:
        if not root:
            raise ValueError("local and UNC sources require an authorized root")
        normalized = root.replace("/", "\\")
        is_unc = normalized.startswith("\\\\")
        if source_type == "UNC_PRIVATE" and not is_unc:
            raise ValueError("UNC_PRIVATE requires a UNC path")
        if source_type == "LOCAL" and (is_unc or len(normalized) < 3 or normalized[1:3] != ":\\"):
            raise ValueError("LOCAL requires an absolute drive path")
        origins = []
        profile = {"source_id": source_id, "source_type": source_type, "root": root}

    if binary is None:
        subprocess.run(["cargo", "build", "--release", "-p", "larm-source-p048"], cwd=repo, check=True)
        binary = repo / "target/release/larm-source-p048.exe"
    if not binary.is_file():
        raise FileNotFoundError(f"P048 worker executable not found: {binary}")
    package.mkdir(parents=True)
    work.mkdir(parents=True)
    (package / "bin").mkdir()
    shutil.copyfile(binary, package / "bin/larm-source-p048.exe")
    (work / "profile.json").write_text(json.dumps(profile, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")

    methods_doc = json.loads((repo / "docs/contracts/adapter-methods.json").read_text(encoding="utf-8"))
    methods = methods_doc["mandatory_common"] + [item["method"] for item in methods_doc["methods"] if item["family"] == "source" and item["required"]]
    if "source.download_chunk" not in methods:
        methods.append("source.download_chunk")
    manifest = {
        "id": f"larm.source.p048.{source_type.lower()}", "version": "1.0.0", "family": "source",
        "protocol_version": "1.0", "publisher": "LARM",
        "executable": {"relative_path": "bin/larm-source-p048.exe", "args": ["--manifest", "manifest.json"]},
        "platforms": ["windows"], "architectures": ["x86_64"], "methods": methods,
        "permissions": {"read_roots": ["p048-work"] + (["p048-source"] if source_type in ("LOCAL", "UNC_PRIVATE") else []),
                        "write_roots": ["p048-work"], "network_origins": origins,
                        "process_allowlist": [],
                        "secret_refs": [f"larm/source/{source_id}"] if source_type == "HTTP" else []},
        "capabilities": ["source.search"] if source_type == "GITHUB" else [],
        "package_sha256": "0" * 64, "signature": None, "installation_strategies": [],
    }
    (package / "manifest.json").write_text(json.dumps(manifest, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    manifest["package_sha256"] = package_hash(package)
    (package / "manifest.json").write_text(json.dumps(manifest, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    return {"manifest": manifest, "profile": profile, "package_sha256": manifest["package_sha256"]}


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", type=Path, default=Path(__file__).resolve().parents[3])
    parser.add_argument("--package", type=Path, required=True)
    parser.add_argument("--work", type=Path, required=True)
    parser.add_argument("--source-type", choices=("GITHUB", "HTTP", "LOCAL", "UNC_PRIVATE"), required=True)
    parser.add_argument("--source-id")
    parser.add_argument("--root")
    parser.add_argument("--url")
    parser.add_argument("--checksum")
    parser.add_argument("--repository")
    parser.add_argument("--binary", type=Path, help="Use an already-built worker executable")
    args = parser.parse_args()
    result = create(args.repo, args.package, args.work, args.source_type, source_id=args.source_id,
                    root=args.root, url=args.url, checksum=args.checksum, repository=args.repository,
                    binary=args.binary)
    print(json.dumps({"id": result["manifest"]["id"], "source_id": result["profile"]["source_id"],
                      "package_sha256": result["package_sha256"]}))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
