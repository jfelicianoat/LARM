#!/usr/bin/env python3
"""P142: puerta del verificador de cobertura normativa — negativos, runners de
escenario y la ejecución REAL del harness de la matriz sobre el árbol actual
del PC IA. El manifiesto guarda la distribución exacta de estados por prompt:
CODE_PASS/REAL_PASS/BLOCKED_REAL/BLOCKED de las 73 evidencias vigentes y
NOT_STARTED del resto, sin etiquetar omitidos como PASS."""

import json
import sys

from evidence import Evidence, REPO

TOOL_PYTHON = REPO / ".venv-tools" / "Scripts" / "python.exe"
PYTHON = str(TOOL_PYTHON) if TOOL_PYTHON.is_file() else sys.executable


def main() -> int:
    ev = Evidence("P142")

    ev.run(
        "P142-CONTRACT-verifier-negatives",
        "CONTRACT",
        [PYTHON, "-X", "utf8", "tools/test_p142_requirements_matrix.py"],
        "los seis oráculos negativos del contrato: evidencia borrada, hash alterado, estado desconocido, NCNN omitido, mock etiquetado REAL y clausura incompleta hacen fallar al verificador",
    )
    ev.run(
        "P142-CONTRACT-matrix-build",
        "CONTRACT",
        [PYTHON, "-X", "utf8", "tools/requirements_matrix.py", "build"],
        "matriz determinista con 127 prompts, 20 runtimes, 5 familias, 34 entidades, 14 pantallas, 14 tipos de fuente y 15 escenarios",
    )
    ev.run(
        "P142-CONTRACT-scenario-runners-exist",
        "CONTRACT",
        [PYTHON, "-X", "utf8", "-c", (
            "import pathlib,sys;"
            "base=pathlib.Path('tools/scenarios');"
            "runners=[p.name for p in sorted(base.glob('g??.py') if base.exists() else [])];"
            "expected={f'g{i:02d}.py' for i in range(1,16)};"
            "assert set(runners)==expected, (runners, expected);"
            "print('PASS: 15 runners de escenario presentes')"
        )],
        "los 15 runners globales g01..g15 existen en tools/scenarios",
    )
    ev.run(
        "P142-CONTRACT-checkps1-scenario-support",
        "CONTRACT",
        [PYTHON, "-X", "utf8", "-c", (
            "import pathlib;"
            "text=pathlib.Path('tools/check.ps1').read_text(encoding='utf-8');"
            "assert '-Scenario' in text and 'G\\\\d{2}' in text and 'tools/scenarios' in text;"
            "print('PASS: check.ps1 admite selección por prompt y escenario')"
        )],
        "check.ps1 con selección por prompt (-Prompt) y escenario (-Scenario Gxx) que ejecuta tools/scenarios/<gxx>.py",
    )

    def real_matrix_ok(code: int, output: str) -> bool:
        return (
            code == 0
            and output.startswith("PASS:")
            and "127 prompts" in output
            and "15 escenarios" in output
        )

    ev.run(
        "P142-REAL-matrix-harness-pc-ia",
        "REAL",
        [PYTHON, "-X", "utf8", "tools/requirements_matrix.py", "check"],
        "harness ejecutado en el PC IA contra los módulos implementados: el manifiesto recalculado PASS con la distribución de estados exacta de las evidencias vigentes",
        ok=real_matrix_ok,
        timeout=300,
    )

    def scenario_runner_ok(code: int, output: str) -> bool:
        return code == 2 and "BLOCKED" in output and "G01" in output and "P060: BLOCKED_REAL" in output

    ev.run(
        "P142-REAL-scenario-g01-reports-blocked-with-real-owners",
        "REAL",
        [PYTHON, "-X", "utf8", "tools/scenarios/g01.py"],
        "el runner del escenario G01 reporta el estado REAL actual de sus propietarios (P060 BLOCKED_REAL con los dos builds reales de llama.cpp) y sale 2 mientras alguno no esté en REAL_PASS",
        ok=scenario_runner_ok,
        timeout=120,
    )

    changed = [
        "tools/requirements_matrix.py",
        "tools/scenarios/g01.py", "tools/scenarios/g02.py", "tools/scenarios/g03.py",
        "tools/scenarios/g04.py", "tools/scenarios/g05.py", "tools/scenarios/g06.py",
        "tools/scenarios/g07.py", "tools/scenarios/g08.py", "tools/scenarios/g09.py",
        "tools/scenarios/g10.py", "tools/scenarios/g11.py", "tools/scenarios/g12.py",
        "tools/scenarios/g13.py", "tools/scenarios/g14.py", "tools/scenarios/g15.py",
        "tools/test_p142_requirements_matrix.py",
        "tools/p142_evidence.py",
        "docs/reports/requirements-matrix.json",
        "docs/reports/P142_cobertura-normativa-y-manifiesto-de-evidencia-por-requisito.md",
        "docs/reports/P142_cobertura-normativa-y-manifiesto-de-evidencia-por-requisito.evidence.json",
        "docs/implementation/STATE.md",
    ]
    blockers = [
        "REAL_PASS de P142 exige el conjunto completo de prompts en REAL_PASS y los 15 escenarios realmente ejecutados en el PC IA (P138 lo exige); hoy 127 requisitos cubren 73 evidencias con la mayoría BLOCKED_REAL/NOT_STARTED — el verificador reporta esa distribución sin maquillarla.",
    ]
    status = ev.finish(changed, blockers)
    return 0 if "FAIL" not in status else 1


if __name__ == "__main__":
    raise SystemExit(main())
