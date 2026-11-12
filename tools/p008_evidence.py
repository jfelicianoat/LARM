#!/usr/bin/env python3
"""P008 - prerrequisitos y packaging de desarrollo Windows: pruebas y evidencia."""
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from evidence import REPO, Evidence  # noqa: E402

PY = sys.executable


def main() -> int:
    ev = Evidence("P008")
    ev.run("P008-UNIT-doctor-parser", "UNIT", [PY, "tools/verify_p008.py", "parser"],
           "16 salidas falsas -> estados esperados, incluida la instancia única de vswhere y JSON basura; "
           "cada no-PASS con paso manual", fixtures=[REPO / "environment.lock.json"], timeout=900)
    ev.run("P008-REAL-doctor-pc-ia", "REAL", [PY, "tools/verify_p008.py", "readonly"],
           "doctor real en el PC IA: informe JSON válido y PATH/registro sin cambios", timeout=300)
    ev.run("P008-REAL-doctor-script", "REAL",
           ["powershell", "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", "tools/doctor.ps1"],
           "tools/doctor.ps1 sin FAIL en el PC IA (avisos permitidos)")
    ev.finish(["tools/doctor.ps1", "tools/verify_p008.py", "tools/p008_evidence.py",
               "docs/operations/windows-prerequisites.md", "docs/reports/P008_runs/doctor.json"], [])
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
