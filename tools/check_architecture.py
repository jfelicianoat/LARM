#!/usr/bin/env python3
"""P002 - Verificador de arquitectura contra docs/contracts/crate-graph.json (copia del paquete v3).

Reglas del contrato:
- cada crate larm-* solo depende (normal, dev o build) de los crates de `allowed_dependencies`;
- apps/daemon es la raiz de composicion (puede depender de todos); ningun crate depende de apps/*;
- larm-domain no depende de IO, SQLx, Axum, Tauri ni Windows (`forbidden_domain_dependencies`):
  se comprueban sus dependencias externas declaradas y el uso de std::fs/net/process en su codigo;
- la GUI (apps/desktop) no depende de crates internos: solo API HTTP.

Uso:
  check_architecture.py [--root DIR]   verifica el workspace (por defecto el repo) con cargo metadata
  check_architecture.py --selftest     crea workspaces temporales que violan cada regla y exige FAIL
"""
import argparse
import json
import re
import subprocess
import sys
import tempfile
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
GRAPH = REPO / "docs/contracts/crate-graph.json"
# Traduccion de las categorias prohibidas del contrato a crates y APIs concretas.
FORBIDDEN_CRATES = {
    "IO": {"tokio", "async-std", "reqwest", "hyper", "mio", "tempfile", "dirs", "keyring", "tracing-appender"},
    "SQLx": {"sqlx", "sqlx-core", "sqlx-sqlite", "rusqlite"},
    "Axum": {"axum", "axum-core", "tower-http"},
    "Tauri": {"tauri", "tauri-build", "wry", "tao"},
    "Windows": {"windows", "windows-sys", "winapi", "winreg", "windows-core"},
}
FORBIDDEN_SOURCE = re.compile(r"\bstd::(fs|net|process)\b|\buse\s+std::\{[^}]*\b(fs|net|process)\b")


def load_contract() -> dict:
    c = json.loads(GRAPH.read_text(encoding="utf-8"))
    if "allowed_dependencies" not in c or c.get("composition_root") != "apps/daemon":
        raise SystemExit("crate-graph.json no tiene el formato del contrato v3 (allowed_dependencies/composition_root)")
    return c


def metadata(root: Path) -> dict:
    out = subprocess.run(["cargo", "metadata", "--format-version", "1", "--no-deps", "--offline"],
                         cwd=root, capture_output=True, text=True, encoding="utf-8", check=False)
    if out.returncode != 0:
        raise SystemExit(f"cargo metadata fallo en {root}: {out.stderr.strip()[:400]}")
    return json.loads(out.stdout)


def check(meta: dict, contract: dict) -> list[str]:
    allowed = contract["allowed_dependencies"]
    forbidden_cats = contract["forbidden_domain_dependencies"]
    forbidden = set().union(*(FORBIDDEN_CRATES[c] for c in forbidden_cats))
    root = Path(meta["workspace_root"])
    failures = []
    for pkg in meta["packages"]:
        name = pkg["name"]
        manifest = Path(pkg["manifest_path"]).resolve()
        # Ruta relativa al repo cuando el crate esta dentro (p. ej. el shell Tauri, workspace propio).
        base = REPO if manifest.is_relative_to(REPO) else root
        rel = manifest.parent.relative_to(base).as_posix()
        deps = pkg["dependencies"]
        internal = {d["name"] for d in deps if d["name"].startswith("larm-")}
        path_into_apps = [d["name"] for d in deps if d.get("path") and "/apps/" in Path(d["path"]).as_posix() + "/"]
        if rel == contract["composition_root"]:
            continue
        if path_into_apps:
            failures.append(f"{name}: depende de apps/* ({sorted(path_into_apps)})")
        if rel.startswith("apps/"):
            if internal:
                failures.append(f"{name} ({rel}): la GUI solo usa la API HTTP; depende de {sorted(internal)}")
            continue
        if name not in allowed:
            failures.append(f"{name}: crate fuera del grafo contractual")
            continue
        extra = internal - set(allowed[name])
        if extra:
            failures.append(f"{name}: dependencias internas no autorizadas {sorted(extra)}")
        if name == "larm-domain":
            ext = {d["name"] for d in deps if not d["name"].startswith("larm-")}
            bad = sorted(ext & forbidden)
            if bad:
                failures.append(f"larm-domain: dependencias prohibidas ({'/'.join(forbidden_cats)}): {bad}")
            for src in sorted((manifest.parent / "src").rglob("*.rs")):
                for n, line in enumerate(src.read_text(encoding="utf-8").splitlines(), 1):
                    if FORBIDDEN_SOURCE.search(line.split("//")[0]):
                        failures.append(f"larm-domain: IO en {src.relative_to(manifest.parent).as_posix()}:{n}: {line.strip()[:80]}")
    return failures


def run(root: Path) -> int:
    contract = load_contract()
    meta = metadata(root)
    failures = check(meta, contract)
    print(json.dumps({"status": "PASS" if not failures else "FAIL", "workspace": str(root),
                      "crates_checked": len(meta["packages"]), "failures": failures}, indent=2, ensure_ascii=False))
    return 0 if not failures else 1


# --- casos negativos sobre workspaces reales temporales --------------------------------------
def _crate(base: Path, rel: str, name: str, deps: str = "", src: str = "") -> None:
    d = base / rel
    (d / "src").mkdir(parents=True, exist_ok=True)
    (d / "Cargo.toml").write_text(f'[package]\nname = "{name}"\nversion = "0.0.0"\nedition = "2021"\n\n[dependencies]\n{deps}', encoding="utf-8")
    (d / "src/lib.rs").write_text(src or "", encoding="utf-8")


def _workspace(base: Path, members: list[str]) -> None:
    base.mkdir(parents=True, exist_ok=True)
    (base / "Cargo.toml").write_text("[workspace]\nresolver = \"2\"\nmembers = [" + ", ".join(f'"{m}"' for m in members) + "]\n", encoding="utf-8")


def selftest() -> int:
    contract = load_contract()
    cases = {
        "domain_con_sqlx": (
            lambda b: (_workspace(b, ["crates/larm-domain"]),
                       _crate(b, "crates/larm-domain", "larm-domain", 'sqlx = "0.9"\n')), "dependencias prohibidas"),
        "domain_con_windows": (
            lambda b: (_workspace(b, ["crates/larm-domain"]),
                       _crate(b, "crates/larm-domain", "larm-domain", 'windows = "0.62"\n')), "dependencias prohibidas"),
        "domain_con_std_fs": (
            lambda b: (_workspace(b, ["crates/larm-domain"]),
                       _crate(b, "crates/larm-domain", "larm-domain", src="pub fn f() { let _ = std::fs::read(\"x\"); }\n")), "IO en"),
        "runtime_depende_de_core": (
            lambda b: (_workspace(b, ["crates/larm-core", "crates/larm-runtime"]),
                       _crate(b, "crates/larm-core", "larm-core"),
                       _crate(b, "crates/larm-runtime", "larm-runtime", 'larm-core = { path = "../larm-core" }\n')), "no autorizadas"),
        "crate_fuera_del_grafo": (
            lambda b: (_workspace(b, ["crates/larm-extra"]), _crate(b, "crates/larm-extra", "larm-extra")), "fuera del grafo"),
        "gui_depende_de_crate_interno": (
            lambda b: (_workspace(b, ["crates/larm-domain", "apps/desktop"]),
                       _crate(b, "crates/larm-domain", "larm-domain"),
                       _crate(b, "apps/desktop", "larm-desktop", 'larm-domain = { path = "../../crates/larm-domain" }\n')), "la GUI solo usa"),
        "control_grafo_valido": (
            lambda b: (_workspace(b, ["crates/larm-domain", "crates/larm-db", "apps/daemon"]),
                       _crate(b, "crates/larm-domain", "larm-domain", 'serde = "1"\n'),
                       _crate(b, "crates/larm-db", "larm-db", 'larm-domain = { path = "../larm-domain" }\n'),
                       _crate(b, "apps/daemon", "larm-daemon", 'larm-db = { path = "../../crates/larm-db" }\n')), None),
    }
    failed = 0
    with tempfile.TemporaryDirectory(prefix="larm-arch-") as tmp:
        for name, (build, expect) in cases.items():
            base = Path(tmp) / name
            build(base)
            fails = check(metadata(base), contract)
            ok = (not fails) if expect is None else any(expect in f for f in fails)
            failed += not ok
            print(f"[{'PASS' if ok else 'FAIL'}] {name}: esperado={'sin fallos' if expect is None else expect!r} observado={fails[:2]}")
    print(f"selftest arquitectura: {'PASS' if not failed else 'FAIL'} ({len(cases)} workspaces reales)")
    return 1 if failed else 0


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--selftest", action="store_true")
    ap.add_argument("--root", type=Path, default=REPO)
    args = ap.parse_args()
    return selftest() if args.selftest else run(args.root.resolve())


if __name__ == "__main__":
    raise SystemExit(main())
