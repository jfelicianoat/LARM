#!/usr/bin/env python3
"""Verifica el frontend del ejecutable y que una compilación rechaza assets/fuentes obsoletos."""
import hashlib
import json
import subprocess
import sys
import uuid
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
WEB = REPO / "apps/desktop"
EXE = WEB / "src-tauri/target/debug/larm-desktop.exe"
BUILD = ["cargo", "build", "--manifest-path", "apps/desktop/src-tauri/Cargo.toml", "--features", "tauri/custom-protocol", "--offline"]


def build(reject: bool) -> None:
    result = subprocess.run(BUILD, cwd=REPO, capture_output=True, timeout=300)
    output = (result.stdout + result.stderr).decode("utf-8", errors="replace")
    if reject:
        assert result.returncode != 0 and "frontend_desactualizado:" in output, output[-1500:]
    else:
        assert result.returncode == 0, output[-1500:]


def main() -> int:
    if "--fault" in sys.argv:
        build(False)
        original = (WEB / "dist/index.html").read_bytes()
        try:
            (WEB / "dist/index.html").write_bytes(original + b"\n<!-- stale asset -->\n")
            build(True)
            print("PASS: asset alterado rechazado por la compilación")
        finally:
            (WEB / "dist/index.html").write_bytes(original)
        extra = WEB / "src" / f"qa-stale-{uuid.uuid4().hex}.txt"
        try:
            extra.write_text("fuente nueva sin recompilar", encoding="utf-8")
            build(True)
            print("PASS: fuente nueva sin recompilar rechazada")
        finally:
            extra.unlink(missing_ok=True)
        build(False)
    stamp = WEB / "dist/ui-build.json"
    expected = hashlib.sha256(stamp.read_bytes()).hexdigest()
    result = subprocess.run([str(EXE), "--ui-build-info"], capture_output=True, check=True, timeout=15)
    assert json.loads(result.stdout)["ui_manifest_sha256"] == expected, "el ejecutable no corresponde al frontend actual"
    manifest = json.loads(stamp.read_bytes())
    for group in ("sources", "assets"):
        for name, digest in manifest[group].items():
            assert hashlib.sha256((WEB / name).read_bytes()).hexdigest() == digest, name
    print(f"PASS: ejecutable y frontend coinciden ({len(manifest['sources'])} fuentes y {len(manifest['assets'])} assets)")
    return 0


if __name__ == "__main__":
    sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    raise SystemExit(main())
