#!/usr/bin/env python3
"""P001 - ejecuta las puertas y pruebas reales del bootstrap y escribe la evidencia."""
import shutil
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from evidence import REPO, Evidence, cargo_ok  # noqa: E402

PY = sys.executable
DESKTOP = "apps/desktop/src-tauri/Cargo.toml"
NPM = shutil.which("npm.cmd") or "npm"
NPX = shutil.which("npx.cmd") or "npx"


def smoke_ok(code: int, out: str) -> bool:
    return code == 0 and 'LARM_DESKTOP_SMOKE {"ping":"pong"' in out


def main() -> int:
    ev = Evidence("P001")
    for sub, exp in [("structure", "estructura completa de SHARED_CONTRACT, incluidas adapters/<5 familias>"),
                     ("scripts", "scripts raíz lint/typecheck/test/build/daemon:dev/tauri:dev delegados y funcionales"),
                     ("policies", "unsafe prohibido, lints heredados, toolchain fijado y activo")]:
        ev.run(f"P001-CONTRACT-{sub}", "CONTRACT", [PY, "tools/verify_p001.py", sub], exp)
    ev.run("P001-CONTRACT-package-copy", "CONTRACT", [PY, "tools/package_copy.py", "check"],
           "spec, contratos (con external/), schemas y fixtures copiados íntegros con sha256 del paquete v3",
           fixtures=[REPO / "docs/spec/PACKAGE_COPY_HASHES.json"])
    ev.run("P001-FAULT-package-copy-tampered", "FAULT", [PY, "tools/verify_p001.py", "copy-tamper"],
           "una copia alterada y un snapshot borrado en un repo temporal se detectan como FAIL")

    ev.run("P001-UNIT-cargo-fmt", "UNIT", ["cargo", "fmt", "--all", "--check"], "sin diferencias de formato")
    ev.run("P001-UNIT-cargo-clippy", "UNIT", ["cargo", "clippy", "--workspace", "--all-targets", "--", "-D", "warnings"],
           "clippy sin avisos en todo el workspace")
    ev.run("P001-UNIT-cargo-test", "UNIT", ["cargo", "test", "--workspace"],
           "todas las pruebas del workspace pasan, ninguna ignorada", ok=cargo_ok)
    ev.run("P001-INTEGRATION-daemon-binary", "INTEGRATION", ["cargo", "test", "-p", "larm-daemon", "--test", "lifecycle"],
           "el binario real: --help sale 0; config inválida o inexistente sale 2 sin inicializar logs", ok=cargo_ok)
    ev.run("P001-UNIT-desktop-fmt", "UNIT", ["cargo", "fmt", "--manifest-path", DESKTOP, "--check"], "shell Tauri formateado")
    ev.run("P001-UNIT-desktop-clippy", "UNIT", ["cargo", "clippy", "--manifest-path", DESKTOP, "--all-targets", "--", "-D", "warnings"],
           "shell Tauri sin avisos de clippy")
    ev.run("P001-UNIT-desktop-test", "UNIT", ["cargo", "test", "--manifest-path", DESKTOP],
           "ping devuelve pong; smoke desactivado sin variable", ok=cargo_ok)
    for script in ("lint", "typecheck", "test", "build"):
        ev.run(f"P001-UNIT-npm-{script}", "UNIT", [NPM, "run", script, "--silent"], f"npm run {script} sin errores")

    ev.run("P001-REAL-daemon-build", "REAL", ["cargo", "build", "-p", "larm-daemon"], "binario del daemon compilado")
    ev.run("P001-REAL-daemon-lifecycle", "REAL", [PY, "tools/daemon_lifecycle.py"],
           "larm-daemon.exe arranca con raíz gestionada, escribe su log, CTRL_BREAK -> exit 0")
    ev.run("P001-REAL-daemon-dev-script", "REAL", [PY, "tools/daemon_lifecycle.py", "--npm"],
           "npm run daemon:dev arranca el daemon con argumentos y éste registra el cierre limpio", timeout=600)
    ev.run("P001-REAL-desktop-build", "REAL", [NPX, "tauri", "build", "--debug", "--no-bundle"],
           "shell Tauri compilado con el frontend embebido", cwd=REPO / "apps/desktop", timeout=1800)
    ev.run("P001-REAL-desktop-ping", "REAL", [str(REPO / "apps/desktop/src-tauri/target/debug/larm-desktop.exe")],
           "la ventana LARM real hace invoke('ping') por WebView2 y recibe 'pong'", ok=smoke_ok,
           env={"LARM_DESKTOP_SMOKE": "1"}, timeout=120)
    ev.run("P001-REAL-check-ps1", "REAL",
           ["powershell", "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", "tools/check.ps1", "-SkipFrontend", "-SkipDesktop"],
           "tools/check.ps1 se ejecuta en Windows PowerShell 5.1 y devuelve PASS", timeout=1800)

    changed = [
        ".gitignore", "package.json", "apps/daemon/Cargo.toml", "apps/daemon/tests/lifecycle.rs",
        "apps/desktop/src/App.tsx", "apps/desktop/src/App.test.tsx", "apps/desktop/src/tauri.ts",
        "apps/desktop/vite.config.ts", "apps/desktop/src-tauri/tauri.conf.json",
        "apps/desktop/src-tauri/src/commands.rs", "apps/desktop/src-tauri/src/lib.rs",
        "adapters/runtime/README.md", "adapters/source/README.md", "adapters/protocol/README.md",
        "adapters/transformation/README.md", "adapters/benchmark/README.md",
        "docs/spec/PACKAGE_COPY_HASHES.json", "docs/spec/LARM_SPEC_ORIGINAL.md", "docs/spec/LARM_SPEC_BASELINE.md",
        "docs/spec/manifest.json", "docs/spec/PROMPT_INDEX.md", "docs/contracts/crate-graph.json",
        "docs/contracts/external/", "tools/check.ps1", "tools/package_copy.py", "tools/verify_p001.py",
        "tools/daemon_lifecycle.py", "tools/p001_evidence.py",
    ]
    ev.finish(changed, [])
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
