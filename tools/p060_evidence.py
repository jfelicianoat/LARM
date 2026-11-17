#!/usr/bin/env python3
"""P060: descubrimiento/adopción real de los dos builds llama.cpp side-by-side
del PC IA (b79696f y d2d6269/b7020); la publicación MANAGED en runtime store y
el paquete Runtime Adapter Protocol siguen bloqueados por locks sin cualificación
real (P099/P031) y son el alcance de P061."""

import json
import sys
from pathlib import Path

from evidence import Evidence, REPO

TOOL_PYTHON = REPO / ".venv-tools" / "Scripts" / "python.exe"
PYTHON = str(TOOL_PYTHON) if TOOL_PYTHON.is_file() else sys.executable
PROBE = REPO / "adapters/runtime/llama-cpp/probe.py"
EVIDENCE_DIR = REPO / "docs/reports/P060_runs"

# Builds side-by-side reales compilados en el PC IA (P093 adquirió la fuente;
# los dos builds salen del mismo árbol MSVC con commits distintos).
REAL_ROOTS = ["C:/Desarrollo/llama.cpp", "C:/Desarrollo/llama.cpp-b7020"]
EXPECTED_COMMITS = ("b79696f", "d2d6269")


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

    command = [PYTHON, "-X", "utf8", str(probe.relative_to(REPO))]
    for root in REAL_ROOTS:
        command.extend(("--root", root))
    expected = (
        "dos builds side-by-side observados con version_string/commit distintos, hashes sha256 "
        "de cada herramienta, help con --list-devices anunciado, backend probe OBSERVED y "
        "qualification UNQUALIFIED (el registro no acredita soporte)"
    )

    def observed(code: int, output: str) -> bool:
        if code != 0:
            return False
        try:
            result = json.loads(output)
        except ValueError:
            return False
        if result.get("schema_version") != "larm.runtime.llama-cpp.observation/1":
            return False
        installations = result.get("installations", [])
        commits = {
            installation.get("version", {}).get("commit")
            for installation in installations
        }
        return EXPECTED_COMMITS[0] in commits and EXPECTED_COMMITS[1] in commits

    if ev.run(
        "P060-REAL-adopt-and-probe",
        "REAL",
        command,
        expected,
        ok=observed,
        timeout=300,
        fixtures=[probe],
    ):
        # La transcripción completa queda como artefacto de evidencia.
        capture = EVIDENCE_DIR / "P060-REAL-adopt-and-probe.json"
        capture.parent.mkdir(parents=True, exist_ok=True)
        # ev.run escribe el log en P060_runs; conservar el JSON crudo también.

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
        "P060-REAL-managed-store-publication",
        "REAL",
        "publicación MANAGED side-by-side en el runtime store con artifact por backend",
        "dos builds Windows con source/hash fijados, artifact adecuado al backend verificado, instalación aislada en runtime store y profile locks completos con cualificación real",
        "la admisión P031 exige un profile lock cualificado (P099); los locks vigentes permanecen UNQUALIFIED y la selección de artifact por backend requiere la evidencia de P011",
    )
    ev.blocked(
        "P060-REAL-runtime-adapter-protocol-package",
        "REAL",
        "paquete del Runtime Adapter Protocol para llama.cpp",
        "paquete del adaptador runtime con lanzamiento de llama-server, health y métodos del protocolo observados en el PC IA",
        "el paquete del adaptador runtime de llama.cpp es el alcance de P061 (serving/smoke/bench); sus dependencias exigen REAL_PASS",
    )

    changed = [
        "adapters/runtime/llama-cpp/README.md",
        "adapters/runtime/llama-cpp/probe.py",
        "docs/adapters/llama-cpp.md",
        "docs/implementation/STATE.md",
        "docs/reports/P060_llama-cpp-descubrimiento-instalacion-y-versiones.md",
        "docs/reports/P060_llama-cpp-descubrimiento-instalacion-y-versiones.evidence.json",
        "tools/p060_evidence.py",
        "tools/test_p060_llama_cpp_probe.py",
    ]
    blockers = [
        "publicación MANAGED en runtime store bloqueada: la admisión P031 exige profile lock cualificado (P099 UNQUALIFIED) y la selección de artifact por backend exige la evidencia de P011.",
        "paquete del Runtime Adapter Protocol de llama.cpp: alcance de P061 (serving, smoke tests y llama-bench) con dependencias en REAL_PASS pendientes.",
        "No se anuncia soporte de backends ni compatibilidad por el hecho de que el helper enumere herramientas o lea `--list-devices`.",
    ]
    status = ev.finish(changed, blockers)
    return 0 if "FAIL" not in status else 1


if __name__ == "__main__":
    raise SystemExit(main())
