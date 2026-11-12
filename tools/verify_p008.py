#!/usr/bin/env python3
"""P008 - Pruebas del doctor de prerrequisitos (exit 0 = PASS).

  parser     tools/doctor.ps1 -FromRaw sobre salidas crudas falsas: cada caso fija el estado
             esperado de cada comprobación (incluida la instancia única de vswhere, que
             PowerShell 5.1 trata distinto, y JSON basura que no debe tumbar el doctor)
  readonly   doctor real en el PC: PATH de usuario y de máquina y las claves leídas del
             registro quedan idénticos; informe JSON válido con PASS/WARN/FAIL
"""
import copy
import json
import subprocess
import sys
import tempfile
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
PS = ["powershell", "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", str(REPO / "tools/doctor.ps1")]
LOCK = json.loads((REPO / "environment.lock.json").read_text(encoding="utf-8"))

VS = {"displayName": "Visual Studio Build Tools 2026", "installationVersion": "18.8.1", "isComplete": True, "isPrerelease": False}
GOOD = {
    "vswhere": json.dumps([VS, dict(VS, displayName="VS 2019", installationVersion="16.11")]),
    "webview2": "154.0.4258.62",
    "windows_sdk": "C:\\Program Files (x86)\\Windows Kits\\10\\",
    "tools": {
        "rustc": {"path": "x", "output": f"rustc {LOCK['rust']['rustc']} (b940084d7 2026-09-28)"},
        "cargo": {"path": "x", "output": "cargo 1.99.0"},
        "node": {"path": "x", "output": LOCK["node"]["node"]},
        "npm": {"path": "x", "output": "11.16.0"},
        "python": {"path": "x", "output": "Python 3.14.6"},
        "git": {"path": "x", "output": "git version 2.55.0"},
        "cmake": {"path": "x", "output": "cmake version 4.1.0"},
        "ninja": {"path": "x", "output": "1.13.1"},
        "pwsh": None,
    },
    "powershell": "5.1.26100.9444",
    "developer_mode": 1,
    "long_paths": 1,
    "symlink": True,
}


def case(mutate) -> dict:
    raw = copy.deepcopy(GOOD)
    mutate(raw)
    return raw


def run(raw: dict) -> tuple[int, dict]:
    with tempfile.TemporaryDirectory() as tmp:
        src, out = Path(tmp) / "raw.json", Path(tmp) / "out.json"
        src.write_text(json.dumps(raw), encoding="utf-8")
        p = subprocess.run(PS + ["-FromRaw", str(src), "-OutJson", str(out)], capture_output=True, text=True, check=False)
        report = json.loads(out.read_text(encoding="utf-8-sig"))
    return p.returncode, report


def statuses(report: dict) -> dict:
    return {c["check"]: c["status"] for c in report["checks"]}


def parser() -> str:
    def s(k, v):
        return lambda r: r.__setitem__(k, v)

    def t(k, v):
        return lambda r: r["tools"].__setitem__(k, v)

    cases = [
        ("todo-bien", lambda r: None, 0, {}),
        ("vswhere-una-instancia", s("vswhere", json.dumps([VS])), 0, {"msvc": "PASS"}),
        ("sin-vswhere", s("vswhere", None), 1, {"msvc": "FAIL"}),
        ("solo-prerelease", s("vswhere", json.dumps([dict(VS, isPrerelease=True)])), 1, {"msvc": "FAIL"}),
        ("vswhere-basura", s("vswhere", "no es json"), 1, {"msvc": "FAIL"}),
        ("sin-webview2", s("webview2", None), 1, {"webview2": "FAIL"}),
        ("sin-sdk", s("windows_sdk", None), 1, {"windows_sdk": "FAIL"}),
        ("rust-distinto", t("rustc", {"path": "x", "output": "rustc 1.98.0 (x)"}), 1, {"rustc": "FAIL"}),
        ("rust-prefijo-engañoso", t("rustc", {"path": "x", "output": "rustc 1.99.01 (x)"}), 1, {"rustc": "FAIL"}),
        ("sin-rust", t("rustc", None), 1, {"rustc": "FAIL"}),
        ("node-distinto", t("node", {"path": "x", "output": "v22.0.0"}), 0, {"node": "WARN"}),
        ("python-antiguo", t("python", {"path": "x", "output": "Python 3.10.11"}), 1, {"python": "FAIL"}),
        ("sin-git", t("git", None), 1, {"git": "FAIL"}),
        ("sin-cmake-ninja", lambda r: (r["tools"].__setitem__("cmake", None), r["tools"].__setitem__("ninja", None)), 0,
         {"cmake": "WARN", "ninja": "WARN"}),
        ("sin-devmode-symlink", lambda r: (r.__setitem__("developer_mode", None), r.__setitem__("symlink", False)), 0,
         {"developer_mode": "WARN", "symlink": "WARN"}),
        ("sin-rutas-largas", s("long_paths", 0), 0, {"long_paths": "WARN"}),
    ]
    done = []
    for name, mutate, rc_expected, expected in cases:
        rc, report = run(case(mutate))
        got = statuses(report)
        wrong = {k: (got.get(k), v) for k, v in expected.items() if got.get(k) != v}
        if name == "todo-bien" and set(got.values()) != {"PASS"}:
            wrong["todo"] = got
        if rc != rc_expected or wrong:
            raise AssertionError(f"{name}: rc={rc} (esperado {rc_expected}) diferencias={wrong}")
        missing_fix = [c["check"] for c in report["checks"] if c["status"] != "PASS" and not c["fix"]]
        if missing_fix:
            raise AssertionError(f"{name}: sin paso manual documentado para {missing_fix}")
        done.append(name)
    return f"{len(done)} casos con salidas falsas: {', '.join(done)}"


def snapshot() -> dict:
    ps = ("[ordered]@{ user_path = [Environment]::GetEnvironmentVariable('Path','User'); "
          "machine_path = [Environment]::GetEnvironmentVariable('Path','Machine'); "
          "devmode = (Get-ItemProperty 'HKLM:\\SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\AppModelUnlock' -ErrorAction SilentlyContinue | Out-String); "
          "longpaths = (Get-ItemProperty 'HKLM:\\SYSTEM\\CurrentControlSet\\Control\\FileSystem').LongPathsEnabled } | ConvertTo-Json")
    out = subprocess.run(["powershell", "-NoProfile", "-Command", ps], capture_output=True, text=True, check=True).stdout
    return json.loads(out)


def readonly() -> str:
    before = snapshot()
    out = REPO / "docs/reports/P008_runs/doctor.json"
    out.parent.mkdir(parents=True, exist_ok=True)
    p = subprocess.run(PS + ["-OutJson", str(out), "-RawOut", str(out.with_name("raw.json"))], capture_output=True, text=True, check=False)
    after = snapshot()
    if before != after:
        raise AssertionError(f"el doctor cambió el sistema: {[k for k in before if before[k] != after.get(k)]}")
    report = json.loads(out.read_text(encoding="utf-8-sig"))
    if report["status"] not in ("PASS", "WARN", "FAIL") or not report["checks"]:
        raise AssertionError(f"informe inválido: {report}")
    if (report["status"] == "FAIL") != (p.returncode == 1):
        raise AssertionError(f"exit {p.returncode} no corresponde a {report['status']}")
    summary = ", ".join(f"{c['check']}={c['status']}" for c in report["checks"])
    return f"PATH y registro idénticos tras el doctor; {report['status']}: {summary}"


if __name__ == "__main__":
    sys.stdout.reconfigure(encoding="utf-8")
    try:
        print("PASS " + {"parser": parser, "readonly": readonly}[sys.argv[1]]())
        raise SystemExit(0)
    except (AssertionError, KeyError, IndexError, OSError, json.JSONDecodeError, subprocess.SubprocessError) as e:
        print(f"FAIL {e}")
        raise SystemExit(1)
