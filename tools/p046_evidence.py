#!/usr/bin/env python3
"""P046 focal and qualification evidence."""
from __future__ import annotations

import json
import subprocess
import sys
import time
from pathlib import Path

from evidence import Evidence, REPO, exit_zero


def live_probe(ev: Evidence) -> tuple[str, str]:
    command = [sys.executable, "tools/p046_live_probe.py"]
    started = time.monotonic()
    try:
        proc = subprocess.run(command, cwd=REPO, capture_output=True, timeout=90, check=False)
        out = (proc.stdout + b"\n" + proc.stderr).decode("utf-8", errors="replace")
        exit_code = proc.returncode
    except subprocess.TimeoutExpired as error:
        out = f"TIMEOUT after 90s\n{(error.stdout or b'').decode('utf-8', errors='replace')}"
        exit_code = -1
    elapsed = time.monotonic() - started
    log = ev.runs / "P046-HF-PUBLIC-SEARCH.log"
    log.write_text(f"$ {' '.join(command)}\n# exit={exit_code} elapsed={elapsed:.1f}s\n{out}", encoding="utf-8")
    parsed = None
    for line in reversed(out.splitlines()):
        try:
            parsed = json.loads(line)
            break
        except json.JSONDecodeError:
            continue
    if exit_code == 0 and isinstance(parsed, dict) and parsed.get("status") == "PASS":
        result, observed = "PASS", f"Consulta pública de solo lectura completada; items={parsed.get('items')}"
        reason = ""
    elif exit_code == 2 and isinstance(parsed, dict) and parsed.get("status") == "BLOCKED":
        result, observed = "BLOCKED", f"La red del entorno no permitió completar la consulta: {parsed.get('reason')}"
        reason = "La ejecución del worker está confinada sin permiso de socket saliente en esta sesión; no había resolución de credenciales ni descarga en la prueba."
    else:
        result, observed = "FAIL", f"exit={exit_code}; {str(parsed)[:300] if parsed else out[-300:]}"
        reason = ""
    ev._add(
        "P046-HF-PUBLIC-SEARCH", "REAL", " ".join(command),
        "El worker completa una búsqueda pública read-only sin credenciales y valida la respuesta Hub.",
        observed, result, [log], [
            REPO / "tools/p046_live_probe.py",
            REPO / "adapters/source/huggingface/adapter.py",
            REPO / "adapters/source/huggingface/package.py",
        ],
    )
    return result, reason


def main() -> int:
    ev = Evidence("P046")
    fixtures = [
        REPO / "adapters/source/huggingface/adapter.py",
        REPO / "adapters/source/huggingface/main.py",
        REPO / "adapters/source/huggingface/package.py",
        REPO / "tests/integration/huggingface_adapter.py",
        REPO / "docs/contracts/source-profiles.json",
        REPO / "docs/contracts/schemas/larm.schema.json",
        REPO / "docs/adapters/method-schemas/index.json",
        REPO / "docs/contracts/02_STORAGE_DOWNLOAD.md",
    ]
    ev.run(
        "P046-HUB-CONTRACT-INTEGRATION", "INTEGRATION",
        [sys.executable, "-m", "unittest", "tests.integration.huggingface_adapter", "-v"],
        "Los métodos source cumplen sus schemas y las fixtures cubren búsqueda, formatos, tamaños, revisiones, acceso y rangos.",
        ok=exit_zero, fixtures=fixtures, timeout=300,
    )
    ev.run(
        "P046-PORTABLE-WORKER", "INTEGRATION",
        [sys.executable, "tools/p046_package_smoke.py"],
        "El paquete portable conserva su hash, arranca Python aislado y completa handshake y auth probe anónimo.",
        ok=exit_zero, fixtures=fixtures + [REPO / "tools/p046_package_smoke.py"], timeout=300,
    )
    live_result, live_reason = live_probe(ev)
    blockers = [live_reason] if live_reason else []
    status = ev.finish(
        changed_files=[
            "adapters/source/huggingface/__init__.py",
            "adapters/source/huggingface/adapter.py",
            "adapters/source/huggingface/main.py",
            "adapters/source/huggingface/package.py",
            "docs/adapters/huggingface.md",
            "docs/contracts/source-profiles.json",
            "docs/contracts/REVISION_3_4_0_P046.md",
            "docs/spec/manifest.json",
            "docs/spec/PACKAGE_COPY_HASHES.json",
            "tests/integration/huggingface_adapter.py",
            "tools/p046_evidence.py",
            "tools/p046_live_probe.py",
            "tools/p046_package_smoke.py",
            "docs/reports/P046_hugging-face-source-adapter.md",
        ],
        blockers=blockers,
    )
    return 0 if status in ("CODE_PASS", "REAL_PASS", "BLOCKED_REAL") else 1


if __name__ == "__main__":
    raise SystemExit(main())
