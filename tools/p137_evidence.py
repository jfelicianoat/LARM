#!/usr/bin/env python3
"""P137: puerta de packaging Windows, User Background Agent y Windows
Service opcional — paquete portátil determinista, autarranque del agente en
la carpeta Startup del usuario (sin admin), servicio opcional con elevación
(NO VERIFICADO sin ella, conducta esperada) y desinstalación que nunca borra
datos sin purga separada y confirmada. REAL sobre el PC IA."""

import sys

from evidence import Evidence, REPO

TOOL_PYTHON = REPO / ".venv-tools" / "Scripts" / "python.exe"
PYTHON = str(TOOL_PYTHON) if TOOL_PYTHON.is_file() else sys.executable


def main() -> int:
    ev = Evidence("P137")

    ev.run(
        "P137-CONTRACT-modos-windows",
        "CONTRACT",
        ["cargo", "test", "-p", "larm-daemon", "--bin", "larm-daemon", "windows::"],
        "decisiones puras de los dos modos (spec §102): .cmd de autarranque determinista y minimizado sin tocar el PATH global, sc create con rutas separadas (ProgramData) y arranque a demanda, service_data_root rechaza unidades de sesión, uninstall_plan exige confirmación para la purga de datos",
    )
    ev.run(
        "P137-REAL-agente-y-servicio-pc-ia",
        "REAL",
        ["cargo", "test", "-p", "larm-daemon", "--test", "windows_agent", "--", "--nocapture"],
        "REAL sobre el PC IA: el daemon arranca como User Background Agent del usuario sin privilegios y sirve health/live; el autarranque se registra y elimina en la carpeta Startup real del usuario (sin admin, .cmd con --serve-api); el servicio opcional sin elevación queda NO VERIFICADO (service_requires_elevation); purge-data sin confirmación se rechaza (uninstall_purge_requires_confirmation) y con --confirm borra la raíz de datos",
        ok=lambda code, output: (
            code == 0
            and "test result: ok. 1 passed" in output
            and "REAL P137: user background agent arranca y sirve la API" in output
            and "REAL P137: autarranque del agente en la carpeta Startup del usuario" in output
            and "REAL P137: servicio opcional sin elevación → NO VERIFICADO" in output
            and "REAL P137: purge sin confirmación rechazada" in output
            and "REAL P137: purge confirmada borra modelos y base" in output
        ),
        timeout=600,
    )
    ev.run(
        "P137-REAL-package-build",
        "REAL",
        [PYTHON, "-X", "utf8", "-I", "tools/package_windows.py"],
        "oráculo Install package build: paquete portátil determinista con el binario del daemon, la documentación de operación, VERSION y SHA256SUMS — sólo copia ficheros (no PATH global, drivers, WSL ni BIOS)",
        ok=lambda code, output: (
            code == 0 and "PASS: " in output and "sha256=" in output
        ),
        timeout=900,
    )
    ev.blocked(
        "P137-REAL-service-instalado",
        "REAL",
        "servicio Windows instalado, arrancado y verificado unattended",
        "sc create LARM con consola elevada, arranque del servicio y verificación de health/live del daemon servido por el SCM con rutas separadas en ProgramData",
        "la instalación del servicio exige una consola elevada; la consola de pruebas no está elevada y el comando real queda NO VERIFICADO (conducta esperada documentada): el guard service_requires_elevation está probado REAL y el flujo sc es el documentado en install-uninstall.md",
    )

    changed = [
        "apps/daemon/src/windows.rs",
        "apps/daemon/src/main.rs",
        "apps/desktop/src-tauri/src/commands.rs",
        "apps/desktop/src-tauri/src/lib.rs",
        "apps/daemon/tests/windows_agent.rs",
        "docs/operations/install-uninstall.md",
        "tools/package_windows.py",
        "docs/implementation/STATE.md",
        "docs/reports/P137_packaging-windows-user-background-agent-y-windows-service-opcional.md",
        "docs/reports/P137_packaging-windows-user-background-agent-y-windows-service-opcional.evidence.json",
        "tools/p137_evidence.py",
    ]
    blockers = [
        "servicio Windows instalado y verificado unattended: exige consola elevada (NO VERIFICADO sin ella; el guard service_requires_elevation está probado REAL)",
    ]
    status = ev.finish(changed, blockers)
    return 0 if "FAIL" not in status else 1


if __name__ == "__main__":
    raise SystemExit(main())
