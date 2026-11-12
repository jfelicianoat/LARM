#!/usr/bin/env python3
"""Copia íntegra del paquete LARM_prompts_v3 dentro del repo, con hashes (P001, contrato 00).

  python tools/package_copy.py sync     copia spec, contrato compartido, manifiesto, contratos
                                        (incluidos snapshots external/), schemas y fixtures, y
                                        reescribe docs/spec/PACKAGE_COPY_HASHES.json
  python tools/package_copy.py check    exit 0 solo si cada copia es idéntica al paquete (hash
                                        del manifiesto) y no falta ninguna
  python tools/package_copy.py select PXXX CARPETA
                                        contexto de un prompt con sus entradas y los reports de
                                        sus dependencias (delegado al selector del paquete)

Las copias son de solo lectura conceptual: un contrato no se edita en el repo; un cambio exige
nueva revisión del paquete.
"""
import hashlib
import json
import shutil
import subprocess
import sys
from pathlib import Path

import os

# LARM_REPO_ROOT permite verificar otra copia del repo (pruebas negativas sobre una copia manipulada).
REPO = Path(os.environ.get("LARM_REPO_ROOT") or Path(__file__).resolve().parent.parent)
PACKAGE = Path(__file__).resolve().parents[2] / "LARM_prompts_v3"
HASHES = REPO / "docs/spec/PACKAGE_COPY_HASHES.json"
ROOT_FILES = {
    "LARM_SPEC_V1.md": "docs/spec/LARM_SPEC_V1.md",
    "LARM_SPEC_ORIGINAL.md": "docs/spec/LARM_SPEC_ORIGINAL.md",
    "LARM_SPEC_BASELINE.md": "docs/spec/LARM_SPEC_BASELINE.md",
    "SHARED_CONTRACT.md": "docs/spec/SHARED_CONTRACT.md",
    "manifest.json": "docs/spec/manifest.json",
    "PROMPT_INDEX.md": "docs/spec/PROMPT_INDEX.md",
}
TREES = {"contracts": "docs/contracts", "schemas": "docs/contracts/schemas", "fixtures": "tests/fixtures"}


def sha(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def mapping() -> dict[str, str]:
    """origen relativo al paquete -> destino relativo al repo."""
    out = dict(ROOT_FILES)
    for src_dir, dst_dir in TREES.items():
        for p in sorted((PACKAGE / src_dir).rglob("*")):
            if p.is_file():
                rel = p.relative_to(PACKAGE / src_dir).as_posix()
                out[f"{src_dir}/{rel}"] = f"{dst_dir}/{rel}"
    return out


def manifest_hashes() -> dict[str, str]:
    m = json.loads((PACKAGE / "manifest.json").read_text(encoding="utf-8"))
    return {i["file"]: i["sha256"] for i in m["normative_files"]}


def sync() -> int:
    official = manifest_hashes()
    files = {}
    for src, dst in mapping().items():
        s = PACKAGE / src
        digest = sha(s)
        if src in official and official[src] != digest:
            print(f"FAIL el paquete fuente no coincide con su manifiesto: {src}")
            return 1
        d = REPO / dst
        d.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(s, d)
        files[dst] = {"source": src, "sha256": digest}
    HASHES.write_text(json.dumps({"source_package": "LARM_prompts_v3", "package_version":
                                  json.loads((PACKAGE / "manifest.json").read_text(encoding="utf-8"))["package_version"],
                                  "files": files}, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    print(f"PASS {len(files)} ficheros copiados del paquete con sha256")
    return 0


def check() -> int:
    recorded = json.loads(HASHES.read_text(encoding="utf-8"))["files"]
    problems = []
    for src, dst in mapping().items():
        d = REPO / dst
        if dst not in recorded:
            problems.append(f"sin registrar: {dst}")
        elif not d.is_file():
            problems.append(f"falta copia: {dst}")
        elif sha(d) != sha(PACKAGE / src) or sha(d) != recorded[dst]["sha256"]:
            problems.append(f"copia modificada respecto al paquete: {dst}")
    for p in problems:
        print(f"FAIL {p}")
    if not problems:
        version = json.loads((PACKAGE / "manifest.json").read_text(encoding="utf-8"))["package_version"]
        print(f"PASS {len(recorded)} copias idénticas al paquete {version}")
    return 1 if problems else 0


def select(code: str, out: str) -> int:
    return subprocess.call([sys.executable, str(PACKAGE / "tools/prepare_prompt.py"), "--prompt", code,
                            "--output", out, "--implementation-root", str(REPO)])


if __name__ == "__main__":
    args = sys.argv[1:]
    if args[:1] == ["sync"]:
        raise SystemExit(sync())
    if args[:1] == ["check"]:
        raise SystemExit(check())
    if args[:1] == ["select"] and len(args) == 3:
        raise SystemExit(select(args[1], args[2]))
    print(__doc__)
    raise SystemExit(2)
