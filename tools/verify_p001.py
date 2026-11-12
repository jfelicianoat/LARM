#!/usr/bin/env python3
"""P001 - Comprobaciones estructurales del bootstrap (exit 0 = PASS).

  structure      estructura obligatoria de SHARED_CONTRACT (incluidas las cinco familias de adaptadores)
  scripts        package.json raíz: lint/typecheck/test/build y daemon:dev/tauri:dev delegados
  policies       unsafe prohibido y clippy estricto en el workspace y en el shell Tauri;
                 rust-toolchain.toml coincide con environment.lock.json
  copy-tamper    negativo: una copia del contrato manipulada en un repo temporal se detecta
"""
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import tomllib
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
STRUCTURE = [
    "Cargo.toml", "rust-toolchain.toml", "package.json", "package-lock.json", ".gitignore", ".editorconfig",
    "apps/daemon", "apps/desktop",
    *(f"crates/larm-{c}" for c in ("domain", "core", "db", "api", "hardware", "storage", "adapter-sdk",
                                   "adapter-host", "runtime", "models", "download", "transform", "benchmark",
                                   "protocol", "events")),
    *(f"adapters/{f}" for f in ("runtime", "source", "protocol", "transformation", "benchmark")),
    "migrations", "docs/spec", "docs/adr", "docs/api", "docs/adapters", "docs/operations", "docs/reports",
    "docs/contracts", "docs/implementation", "tests/fixtures", "tests/integration", "tests/e2e", "tools",
    "docs/spec/LARM_SPEC_V1.md", "docs/implementation/STATE.md", "tools/check.ps1", "README.md",
]


def structure() -> str:
    missing = [p for p in STRUCTURE if not (REPO / p).exists()]
    if missing:
        raise ValueError(f"faltan: {missing}")
    members = tomllib.loads((REPO / "Cargo.toml").read_text(encoding="utf-8"))["workspace"]["members"]
    crates = sorted(p.name for p in (REPO / "crates").iterdir() if p.is_dir())
    missing_members = [c for c in crates if f"crates/{c}" not in members]
    if missing_members or "apps/daemon" not in members:
        raise ValueError(f"crates fuera del workspace: {missing_members}")
    return f"{len(STRUCTURE)} rutas obligatorias presentes; {len(members)} miembros del workspace"


def scripts() -> str:
    pkg = json.loads((REPO / "package.json").read_text(encoding="utf-8"))["scripts"]
    need = {"lint", "typecheck", "test", "build", "daemon:dev", "tauri:dev"}
    missing = need - set(pkg)
    if missing:
        raise ValueError(f"scripts ausentes: {sorted(missing)}")
    if not pkg["daemon:dev"].rstrip().endswith("--"):
        raise ValueError("daemon:dev no reenvía argumentos al daemon (falta '--')")
    for s in ("lint", "typecheck", "test", "build", "tauri:dev"):
        if "--workspace @larm/desktop" not in pkg[s]:
            raise ValueError(f"{s} no delega en @larm/desktop")
    conf = json.loads((REPO / "apps/desktop/src-tauri/tauri.conf.json").read_text(encoding="utf-8"))["build"]
    if "--prefix" in conf["beforeDevCommand"] or "--prefix" in conf["beforeBuildCommand"]:
        raise ValueError("before*Command de Tauri apunta fuera de apps/desktop")
    return "scripts raíz completos y delegados; daemon:dev reenvía argumentos; comandos Tauri en apps/desktop"


def policies() -> str:
    ws = tomllib.loads((REPO / "Cargo.toml").read_text(encoding="utf-8"))["workspace"]
    if ws["lints"]["rust"].get("unsafe_code") not in ("forbid", "deny"):
        raise ValueError("unsafe_code no está prohibido en el workspace")
    for name, cargo in [(p.parent.name, p) for p in REPO.glob("crates/*/Cargo.toml")] + [("daemon", REPO / "apps/daemon/Cargo.toml")]:
        if tomllib.loads(cargo.read_text(encoding="utf-8")).get("lints", {}).get("workspace") is not True:
            raise ValueError(f"{name} no hereda los lints del workspace")
    desk = tomllib.loads((REPO / "apps/desktop/src-tauri/Cargo.toml").read_text(encoding="utf-8"))
    if desk.get("lints", {}).get("rust", {}).get("unsafe_code") != "forbid":
        raise ValueError("el shell Tauri no prohíbe unsafe")
    # Excepciones documentadas: ADR-0009 (DACL de named pipes) y ADR-0010 (sondeo
    # nativo de aceleradores, ejecutado en un proceso hijo).
    allowed = {"apps/daemon/src/winsec.rs", "crates/larm-hardware/src/native.rs"}
    for adr in ("0009-excepcion-unsafe-dacl-pipes.md", "0010-excepcion-unsafe-sondeo-nativo.md"):
        if not (REPO / "docs/adr" / adr).is_file():
            raise ValueError(f"falta docs/adr/{adr}, que documenta una excepción de unsafe")
    unsafe_hits = [p.relative_to(REPO).as_posix() for root in ("crates", "apps/daemon/src", "apps/desktop/src-tauri/src")
                   for p in (REPO / root).rglob("*.rs")
                   if re.search(r"\bunsafe\s*\{|allow\(unsafe_code\)", p.read_text(encoding="utf-8"))]
    outside = sorted(set(unsafe_hits) - allowed)
    if outside:
        raise ValueError(f"unsafe fuera de la excepción documentada: {outside}")
    channel = tomllib.loads((REPO / "rust-toolchain.toml").read_text(encoding="utf-8"))["toolchain"]["channel"]
    locked = json.loads((REPO / "environment.lock.json").read_text(encoding="utf-8"))["rust"]["rustc"]
    if channel != locked:
        raise ValueError(f"rust-toolchain {channel} != environment.lock {locked}")
    rustc = subprocess.run(["rustc", "--version"], capture_output=True, text=True, cwd=REPO, check=True).stdout
    if f"rustc {channel} " not in rustc:
        raise ValueError(f"rustc activo '{rustc.strip()}' no es {channel}")
    return f"unsafe prohibido salvo winsec.rs (ADR-0009) y native.rs (ADR-0010), lints heredados en {len(list(REPO.glob('crates/*/Cargo.toml'))) + 2} crates; sin unsafe fuera de esa excepción; toolchain {channel} activo"


def copy_tamper() -> str:
    with tempfile.TemporaryDirectory(prefix="larm-p001-") as tmp:
        fake = Path(tmp) / "repo"
        for rel in ("docs/spec", "docs/contracts", "tests/fixtures"):
            shutil.copytree(REPO / rel, fake / rel)
        env = {**os.environ, "LARM_REPO_ROOT": str(fake), "PYTHONIOENCODING": "utf-8"}
        clean = subprocess.run([sys.executable, str(REPO / "tools/package_copy.py"), "check"], env=env,
                               capture_output=True, text=True, encoding="utf-8")
        target = fake / "docs/contracts/defaults.json"
        target.write_text(target.read_text(encoding="utf-8").replace("127.0.0.1", "0.0.0.0", 1), encoding="utf-8")
        (fake / "docs/contracts/external/Agora_Runner.md").unlink()
        bad = subprocess.run([sys.executable, str(REPO / "tools/package_copy.py"), "check"], env=env,
                             capture_output=True, text=True, encoding="utf-8")
    if clean.returncode != 0:
        raise ValueError(f"la copia intacta no pasa: {clean.stdout[-300:]}")
    if bad.returncode == 0 or "defaults.json" not in bad.stdout or "Agora_Runner.md" not in bad.stdout:
        raise ValueError(f"manipulación no detectada: rc={bad.returncode} {bad.stdout[-300:]}")
    return "copia intacta PASS; defaults.json alterado y snapshot borrado -> " + " / ".join(
        ln for ln in bad.stdout.splitlines() if ln.startswith("FAIL"))


COMMANDS = {"structure": structure, "scripts": scripts, "policies": policies, "copy-tamper": copy_tamper}

if __name__ == "__main__":
    try:
        print("PASS " + COMMANDS[sys.argv[1]]())
        raise SystemExit(0)
    except (ValueError, KeyError, IndexError, OSError, subprocess.CalledProcessError) as e:
        print(f"FAIL {type(e).__name__}: {e}")
        raise SystemExit(1)
