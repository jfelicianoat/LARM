#!/usr/bin/env python3
"""Focal checks and qualification evidence for P047."""
from __future__ import annotations

import json
import sys

from evidence import Evidence, REPO, exit_zero


def record_live_probe(ev: Evidence) -> None:
    log = ev.runs / "P047-MODELSCOPE-PUBLIC-SEARCH.log"
    if not log.is_file():
        ev.blocked(
            "P047-MODELSCOPE-PUBLIC-SEARCH", "REAL", "python tools/p047_live_probe.py",
            "Una búsqueda pública anónima en vivo completa desde el worker portable.",
            "No hay transcripción de la consulta real; no se contará una fixture ni un test de red simulado como aceptación real.",
        )
        return
    text = log.read_text(encoding="utf-8", errors="replace")
    parsed = None
    for line in reversed(text.splitlines()):
        try:
            parsed = json.loads(line)
            break
        except json.JSONDecodeError:
            continue
    exit_line = next((line for line in text.splitlines() if line.startswith("# exit=")), "")
    try:
        code = int(exit_line.split("=", 1)[1])
    except (IndexError, ValueError):
        code = -1
    if code == 0 and isinstance(parsed, dict) and parsed.get("status") == "PASS":
        result, observed = "PASS", f"Búsqueda pública read-only ejecutada por el worker; items={parsed.get('items')}"
    elif code == 2 and isinstance(parsed, dict) and parsed.get("status") == "BLOCKED":
        result, observed = "BLOCKED", f"No se completó la consulta live: {parsed.get('reason', parsed.get('code', 'red no disponible'))}"
    else:
        result, observed = "FAIL", f"exit={code}; {str(parsed)[:240] if parsed else 'transcripción inválida'}"
    ev._add(
        "P047-MODELSCOPE-PUBLIC-SEARCH", "REAL", "python tools/p047_live_probe.py",
        "Una búsqueda pública anónima en vivo completa desde el worker portable sin descargar archivos.",
        observed, result, [log], [REPO / "tools/p047_live_probe.py", REPO / "adapters/source/modelscope/adapter.py"],
    )


def main() -> int:
    ev = Evidence("P047", extra_inputs=["contracts/source-profiles.json", "contracts/REVISION_3_5_0_P047.md"])
    adapter_files = [
        REPO / "adapters/source/modelscope/__init__.py",
        REPO / "adapters/source/modelscope/adapter.py",
        REPO / "adapters/source/modelscope/main.py",
        REPO / "adapters/source/modelscope/package.py",
        REPO / "tests/integration/modelscope_adapter.py",
        REPO / "docs/adapters/modelscope.md",
        REPO / "docs/contracts/source-profiles.json",
        REPO / "docs/contracts/REVISION_3_5_0_P047.md",
    ]
    ev.run(
        "P047-MODELSCOPE-MOCK-CONTRACT", "INTEGRATION",
        [sys.executable, "-m", "unittest", "tests.integration.modelscope_adapter", "-v"],
        "El contrato y los fixtures cubren identidad, metadatos, hash/tamaño, versiones, descarga por rangos, acceso, timeout, cancelación, errores y recuperación.",
        ok=exit_zero, fixtures=adapter_files, timeout=300,
    )
    ev.run(
        "P047-PORTABLE-WORKER", "INTEGRATION", [sys.executable, "tools/p047_package_smoke.py"],
        "El paquete portable conserva el hash y completa handshake aislado y auth probe anónimo.",
        ok=exit_zero, fixtures=adapter_files + [REPO / "tools/p047_package_smoke.py"], timeout=300,
    )
    ev.run(
        "P047-CANONICAL-PACKAGE-COPY", "CONTRACT",
        [sys.executable, "tools/package_copy.py", "check"],
        "Las copias de los contratos locales coinciden con el paquete canónico 3.5.0.",
        ok=exit_zero, fixtures=[REPO / "tools/package_copy.py", REPO / "docs/spec/PACKAGE_COPY_HASHES.json"], timeout=120,
    )
    ev.run(
        "P047-PYTHON-COMPILE", "UNIT",
        [sys.executable, "-m", "py_compile", "adapters/source/modelscope/__init__.py", "adapters/source/modelscope/adapter.py", "adapters/source/modelscope/main.py", "adapters/source/modelscope/package.py", "tests/integration/modelscope_adapter.py", "tools/p047_package_smoke.py", "tools/p047_live_probe.py"],
        "Todos los módulos del adaptador, pruebas y herramientas compilan.",
        ok=exit_zero, fixtures=adapter_files,
    )
    record_live_probe(ev)
    status = ev.finish(
        changed_files=[
            "adapters/source/modelscope/__init__.py", "adapters/source/modelscope/adapter.py",
            "adapters/source/modelscope/main.py", "adapters/source/modelscope/package.py",
            "docs/adapters/modelscope.md", "docs/contracts/REVISION_3_5_0_P047.md",
            "docs/contracts/source-profiles.json", "docs/implementation/STATE.md",
            "docs/reports/P047_modelscope-source-adapter.md",
            "docs/reports/P047_modelscope-source-adapter.evidence.json",
            "docs/spec/PACKAGE_COPY_HASHES.json", "docs/spec/manifest.json",
            "tests/integration/modelscope_adapter.py", "tools/p047_evidence.py",
            "tools/p047_live_probe.py", "tools/p047_package_smoke.py",
        ],
        blockers=[
            "P047 no puede cerrar hasta que P025, P043 y P045 tengan REAL_PASS.",
            "Autenticación privada y gated continúa pendiente de P045/P141.",
            "Los orígenes de object storage en redirecciones requieren calificación y permiso explícitos antes de habilitar descargas reales.",
        ],
    )
    return 0 if status in ("CODE_PASS", "BLOCKED_REAL") else 1


if __name__ == "__main__":
    raise SystemExit(main())
