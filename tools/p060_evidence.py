#!/usr/bin/env python3
"""P060: probe de llama.cpp listo para rutas explícitas; adopción real pendiente."""

import shutil
import sys
from pathlib import Path

from evidence import Evidence, REPO

TOOL_PYTHON = REPO / ".venv-tools" / "Scripts" / "python.exe"
PYTHON = str(TOOL_PYTHON) if TOOL_PYTHON.is_file() else sys.executable
PATH_TOOLS = ("llama-server", "llama-cli", "llama-bench", "llama-quantize")


def main() -> int:
    ev = Evidence("P060")
    probe = REPO / "adapters/runtime/llama-cpp/probe.py"
    ev.run(
        "P060-CONTRACT-discovery-and-probes",
        "CONTRACT",
        [PYTHON, "-X", "utf8", "tools/test_p060_llama_cpp_probe.py"],
        "siete casos prueban builds laterales, límites de búsqueda, hashes, versión desconocida, detección de opciones/dispositivos y entorno mínimo",
        fixtures=[probe],
    )

    installed = {name: shutil.which(name) for name in PATH_TOOLS}
    installed = {name: path for name, path in installed.items() if path}
    if installed:
        roots = sorted({str(Path(path).resolve().parent) for path in installed.values()})
        command = [PYTHON, "-X", "utf8", str(probe.relative_to(REPO))]
        for root in roots:
            command.extend(("--root", root))
        expected = "se capturan versión/build, hashes de cada binario, help y dispositivos de cada instalación presente en PATH"

        def observed(code: int, output: str) -> bool:
            return code == 0 and '"schema_version": "larm.runtime.llama-cpp.observation/1"' in output

        ev.run(
            "P060-REAL-path-installation-probe",
            "REAL",
            command,
            expected,
            ok=observed,
            timeout=120,
            fixtures=[probe],
        )
    else:
        ev.blocked(
            "P060-REAL-adopt-and-probe",
            "REAL",
            "ejecutar probe harmless de llama.cpp en una instalación Windows encontrada",
            "captura exacta de versión/build/help/hash y backend observado en una instalación presente",
            "P000 y la resolución actual de PATH no encuentran llama-server, llama-cli, llama-bench ni llama-quantize; no hay raíz configurada para ejecutar un probe real",
        )

    check_report = REPO / "docs/reports/check_P060_real.json"
    if check_report.is_file():
        validation = (
            "import json,pathlib,sys; "
            "p=pathlib.Path(sys.argv[1]); d=json.loads(p.read_text(encoding='utf-8')); "
            "assert d.get('prompt')=='P060' and d.get('real') is True and d.get('status')=='PASS'; "
            "assert d.get('checks') and all(x.get('result')=='PASS' for x in d['checks']); "
            "print('PASS: check.ps1 P060 -Real confirmó todas las puertas comunes')"
        )
        ev.run(
            "P060-CONTRACT-common-quality-gates",
            "CONTRACT",
            [PYTHON, "-X", "utf8", "-c", validation, str(check_report)],
            "el recibo de tools/check.ps1 -Prompt P060 -Real contiene fmt, clippy, fixtures, workspace, arquitectura, desktop, frontend y evidencia sin fallos",
            fixtures=[check_report],
        )
    else:
        ev.blocked(
            "P060-CONTRACT-common-quality-gates",
            "CONTRACT",
            "tools/check.ps1 -Prompt P060 -Real",
            "puertas comunes del workspace y escritorio completas",
            "check.ps1 escribe su recibo después de ejecutar p060_evidence; se verifica en la generación final del recibo P060",
        )

    ev.blocked(
        "P060-REAL-two-builds-and-managed-store",
        "REAL",
        "probar dos versiones reales y publicación MANAGED side-by-side",
        "dos builds Windows con source/hash fijados, artifact adecuado al backend verificado, instalación aislada en runtime store y profile locks completos",
        "no hay builds llama.cpp instalados ni una ubicación administrada con dependencias P025/P031/P032/P011/P099 en REAL_PASS",
    )

    changed = [
        "adapters/runtime/llama-cpp/README.md",
        "adapters/runtime/llama-cpp/probe.py",
        "docs/adapters/llama-cpp.md",
        "docs/implementation/STATE.md",
        "docs/reports/P060_llama-cpp-descubrimiento-instalacion-y-versiones.md",
        "tools/p060_evidence.py",
        "tools/test_p060_llama_cpp_probe.py",
    ]
    blockers = [
        "P060 es una fase parcial: falta implementar y empaquetar el adaptador Runtime Adapter Protocol, el plan de instalación MANAGED, adopción real, dos builds/versiones reales y captura por versión.",
        "No se anuncia soporte de backends ni compatibilidad por el hecho de que el helper enumere herramientas o lea `--list-devices`.",
    ]
    return int(ev.finish(changed, blockers) == "BLOCKED")


if __name__ == "__main__":
    raise SystemExit(main())
