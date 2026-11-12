#!/usr/bin/env python3
"""P007 - Convenciones de pruebas y selector (exit 0 = PASS).

  tags [DIR]    ningún test de Rust (#[ignore]) ni de TypeScript (.skip/.todo/xit) se omite sin
                una etiqueta de la convención: real-hardware, network o destructive-disabled.
                Por defecto revisa crates/, apps/ y tests/.
  tags-neg      crea un árbol temporal con omisiones sin etiqueta y exige que `tags` falle
  selector      cada prompt con evidencia tiene tools/<prompt>_evidence.py y check.ps1 conoce
                -Prompt/-Scenario/-Coverage; un escenario sin runner devuelve BLOCKED (exit 2)
"""
import json
import re
import subprocess
import sys
import tempfile
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
TAGS = ("real-hardware", "network", "destructive-disabled")
RUST_IGNORE = re.compile(r"#\[ignore(?:\s*=\s*\"([^\"]*)\")?\]")
TS_SKIP = re.compile(r"\b(?:it|test|describe)\.(?:skip|todo)\s*\(\s*([\"'`])([^\"'`]*)\1|\bx(?:it|describe)\s*\(\s*([\"'`])([^\"'`]*)\3")


def untagged(root: Path) -> list[str]:
    problems = []
    for path in root.rglob("*"):
        if path.suffix not in (".rs", ".ts", ".tsx") or any(p in ("node_modules", "target", "dist") for p in path.parts):
            continue
        text = path.read_text(encoding="utf-8", errors="replace")
        rel = path.relative_to(root).as_posix()
        for m in RUST_IGNORE.finditer(text):
            reason = m.group(1) or ""
            if not reason.startswith(TAGS):
                problems.append(f"{rel}: #[ignore] sin etiqueta ({reason or 'sin motivo'})")
        for m in TS_SKIP.finditer(text):
            name = m.group(2) or m.group(4) or ""
            if not any(f"[{t}]" in name for t in TAGS):
                problems.append(f"{rel}: test omitido sin etiqueta: {name[:60]}")
    return problems


def tags(args: list[str]) -> str:
    roots = [Path(a) for a in args] or [REPO / d for d in ("crates", "apps", "tests")]
    problems = [p for r in roots for p in untagged(r)]
    if problems:
        raise ValueError("; ".join(problems))
    return f"sin tests omitidos sin etiqueta en {len(roots)} árboles ({', '.join(TAGS)})"


def tags_neg() -> str:
    with tempfile.TemporaryDirectory() as tmp:
        t = Path(tmp)
        (t / "a.rs").write_text('#[test]\n#[ignore]\nfn x() {}\n#[test]\n#[ignore = "lento"]\nfn y() {}\n'
                                '#[test]\n#[ignore = "real-hardware: NPU"]\nfn z() {}\n', encoding="utf-8")
        (t / "b.test.ts").write_text('it.skip("algo", () => {});\nit.skip("[network] fuente real", () => {});\n', encoding="utf-8")
        found = untagged(t)
    if len(found) != 3:
        raise ValueError(f"se esperaban 3 omisiones sin etiqueta, encontradas {found}")
    return "3 omisiones sin etiqueta detectadas; las etiquetadas (real-hardware, [network]) se aceptan"


def selector() -> str:
    manifest = json.loads((REPO / "docs/spec/manifest.json").read_text(encoding="utf-8"))
    missing = []
    for p in manifest["prompts"]:
        evidence = REPO / f"{p['report']}.evidence.json"
        runner = REPO / f"tools/{p['code'].lower()}_evidence.py"
        if evidence.is_file() and not runner.is_file() and p["code"] != "P107":
            missing.append(p["code"])
    if missing:
        raise ValueError(f"evidencias sin runner reproducible: {missing}")
    script = (REPO / "tools/check.ps1").read_text(encoding="utf-8")
    for flag in ("$Prompt", "$Scenario", "$Coverage", "_evidence.py"):
        if flag not in script:
            raise ValueError(f"check.ps1 no admite {flag}")
    rc = subprocess.run(["powershell", "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", "tools/check.ps1",
                         "-Real", "-Scenario", "G15"], cwd=REPO, capture_output=True, text=True, check=False).returncode
    if rc != 2:
        raise ValueError(f"un escenario sin runner debe salir con 2 (BLOCKED), salió {rc}")
    with_runner = sorted(p.name for p in (REPO / "tools").glob("p*_evidence.py"))
    return f"{len(with_runner)} prompts con runner ({', '.join(n[:4].upper() for n in with_runner)}); escenario sin runner -> BLOCKED"


if __name__ == "__main__":
    sys.stdout.reconfigure(encoding="utf-8")
    cmd, rest = sys.argv[1], sys.argv[2:]
    try:
        print("PASS " + {"tags": lambda: tags(rest), "tags-neg": tags_neg, "selector": selector}[cmd]())
        raise SystemExit(0)
    except (ValueError, KeyError, OSError) as e:
        print(f"FAIL {e}")
        raise SystemExit(1)
