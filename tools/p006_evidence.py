#!/usr/bin/env python3
"""P006 - shell desktop Tauri, navegación y cliente API: pruebas y evidencia."""
import shutil
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from evidence import REPO, Evidence, cargo_ok  # noqa: E402

PY = sys.executable
NPM = shutil.which("npm.cmd") or "npm"
NPX = shutil.which("npx.cmd") or "npx"
DESKTOP = "apps/desktop/src-tauri/Cargo.toml"


def vitest(filt: str):
    return [NPX, "vitest", "run", "-t", filt]


def vitest_ok(code: int, out: str) -> bool:
    """La prueba filtrada pasó (las demás figuran como skipped por el filtro)."""
    import re
    m = re.search(r"Tests\s+(\d+) passed", out)
    return code == 0 and m is not None and int(m.group(1)) >= 1 and " failed" not in out


def main() -> int:
    ev = Evidence("P006")
    web = REPO / "apps/desktop"
    cases = [
        ("P006-UNIT-client-envelope", "convierte el envelope", "envelope del contrato -> ApiError sin inventar campos"),
        ("P006-FAULT-client-path", "rechaza rutas fuera", "rutas fuera de /api/v1 rechazadas antes de salir"),
        ("P006-FAULT-client-transport", "un fallo de transporte", "fallo de transporte -> DAEMON_UNREACHABLE reintentable"),
        ("P006-UNIT-status", "ONLINE, DEGRADED y OFFLINE", "estado derivado de live/ready"),
        ("P006-INTEGRATION-shell-online", "muestra las 14 pantallas", "14 pantallas del contrato 08 y daemon en línea"),
        ("P006-INTEGRATION-shell-offline", "sin daemon", "sin daemon: OFFLINE, pantalla offline y diagnóstico con el error"),
        ("P006-FAULT-reconnect", "pasa de offline a online", "reconexión detectada y aviso no intrusivo"),
        ("P006-UNIT-a11y-focus", "navegar mueve el foco", "cambiar de pantalla mueve el foco al título; aria-current"),
        ("P006-UNIT-smoke", "solo actúa con smoke", "humo solo con la variable; informe ping+salud"),
    ]
    for test_id, filt, expected in cases:
        ev.run(test_id, test_id.split("-")[1], vitest(filt), expected, ok=vitest_ok, cwd=web)
    for script in ("lint", "typecheck", "build"):
        ev.run(f"P006-UNIT-npm-{script}", "UNIT", [NPM, "run", script, "--silent"], f"npm run {script} sin errores")
    ev.run("P006-UNIT-desktop-fmt", "UNIT", ["cargo", "fmt", "--manifest-path", DESKTOP, "--check"], "shell Tauri formateado")
    ev.run("P006-UNIT-desktop-clippy", "UNIT", ["cargo", "clippy", "--manifest-path", DESKTOP, "--all-targets", "--", "-D", "warnings"],
           "shell Tauri sin avisos")
    ev.run("P006-UNIT-desktop-transport", "UNIT", ["cargo", "test", "--manifest-path", DESKTOP],
           "transporte: métodos/rutas/cabeceras permitidos, base solo loopback, nombre del pipe", ok=cargo_ok)
    ev.run("P006-REAL-architecture", "REAL", ["powershell", "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", "tools/check_architecture.ps1"],
           "el shell sigue sin depender de crates internos (solo API)")
    ev.run("P006-REAL-tauri-build", "REAL", [NPX, "tauri", "build", "--debug", "--no-bundle"],
           "build Tauri con el frontend embebido", cwd=web, timeout=1800)
    ev.run("P006-REAL-daemon-build", "REAL", ["cargo", "build", "-p", "larm-daemon"], "binario del daemon compilado")
    ev.run("P006-REAL-window-offline", "REAL", [PY, "tools/p006_real.py", "offline"],
           "ventana real sin daemon: live/ready -> DAEMON_UNREACHABLE", timeout=300)
    ev.run("P006-REAL-window-online", "REAL", [PY, "tools/p006_real.py", "online"],
           "ventana real contra daemon real: token por bootstrap en el backend, live LIVE y ready READY", timeout=300)
    changed = [
        "apps/desktop/src/App.tsx", "apps/desktop/src/App.test.tsx", "apps/desktop/src/main.tsx",
        "apps/desktop/src/api/client.ts", "apps/desktop/src/api/status.ts", "apps/desktop/src/components.tsx",
        "apps/desktop/src/hooks.ts", "apps/desktop/src/pages.tsx", "apps/desktop/src/screens.ts",
        "apps/desktop/src/smoke.ts", "apps/desktop/src/styles.css", "apps/desktop/src-tauri/src/api.rs",
        "apps/desktop/src-tauri/src/lib.rs", "apps/desktop/src-tauri/src/commands.rs",
        "apps/desktop/src-tauri/Cargo.toml", "tools/p006_real.py", "tools/p006_evidence.py", "tools/p001_evidence.py",
        "docs/reports/P006_runs/shell_offline_hardware.png",
    ]
    ev.finish(changed, [])
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
