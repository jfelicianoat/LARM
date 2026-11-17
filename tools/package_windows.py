#!/usr/bin/env python3
"""Empaqueta LARM para Windows (P137): paquete portátil del daemon.

Sólo COPIA ficheros y calcula hashes: no modifica el PATH global, drivers,
WSL, BIOS ni el registro (spec P137.4). El instalador de la GUI (NSIS) lo
genera `npm run tauri build`; este paquete portátil lleva el binario del
daemon, la documentación de operación, VERSION y SHA256SUMS.

Uso: tools/package_windows.py [--out dist] [--exe ruta-al-daemon]
"""

import argparse
import hashlib
import io
import json
import os
import subprocess
import sys
import zipfile
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with open(path, "rb") as handle:
        for chunk in iter(lambda: handle.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--out", default=str(REPO / "dist"))
    parser.add_argument("--exe", default=None)
    args = parser.parse_args()

    exe = Path(args.exe) if args.exe else REPO / "target" / "release" / "larm-daemon.exe"
    if not exe.is_file():
        print("construyendo el daemon (release)...", flush=True)
        subprocess.run(
            ["cargo", "build", "--offline", "-j", "1", "--release", "-p", "larm-daemon", "--bin", "larm-daemon"],
            cwd=REPO,
            check=True,
        )
        exe = REPO / "target" / "release" / "larm-daemon.exe"
    assert exe.is_file(), f"no existe {exe}"

    version = "0.1.0"
    contract = "3.16.0"
    out_dir = Path(args.out)
    out_dir.mkdir(parents=True, exist_ok=True)
    name = f"LARM-{version}-windows-x64"
    entries = [
        ("larm-daemon.exe", exe.read_bytes()),
        ("VERSION", f"larm {version} / contrato {contract}\nhost: windows-x64\n".encode()),
    ]
    docs = REPO / "docs" / "operations"
    for md in sorted(docs.glob("*.md")):
        entries.append((f"docs/{md.name}", md.read_bytes()))
    manifest = {
        "name": name,
        "larm_contract_version": contract,
        "files": {name_: {"sha256": hashlib.sha256(data).hexdigest(), "bytes": len(data)} for name_, data in entries},
    }
    manifest_bytes = json.dumps(manifest, indent=2, sort_keys=True).encode() + b"\n"

    zip_path = out_dir / f"{name}.zip"
    with zipfile.ZipFile(zip_path, "w", zipfile.ZIP_DEFLATED) as archive:
        for name_, data in entries:
            archive.writestr(f"{name}/{name_}", data)
        archive.writestr(f"{name}/manifest.json", manifest_bytes)
    # SHA256SUMS del propio zip (artefacto instalable).
    sums = out_dir / f"{name}.zip.sha256"
    sums.write_text(f"{sha256(zip_path)}  {zip_path.name}\n", encoding="utf-8")
    print(f"PASS: {zip_path} ({zip_path.stat().st_size} bytes) sha256={sha256(zip_path)}")
    return 0


if __name__ == "__main__":
    sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    raise SystemExit(main())
