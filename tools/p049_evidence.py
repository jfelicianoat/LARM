#!/usr/bin/env python3
"""Focal tests and Windows qualification evidence for P049."""
from __future__ import annotations

import json
import subprocess
import sys

from evidence import Evidence, REPO, exit_zero


def record_live_probe(ev: Evidence) -> None:
    test_id = "P049-CIVITAI-PUBLIC-LIVE"
    command = [sys.executable, "tools/p049_live_probe.py", "--live"]
    log = ev.runs / f"{test_id}.log"
    print(f"==> {test_id}: {' '.join(command)}", flush=True)
    try:
        proc = subprocess.run(command, cwd=REPO, capture_output=True, timeout=360, check=False)
        code = proc.returncode
        output = (proc.stdout + b"\n" + proc.stderr).decode("utf-8", errors="replace")
    except subprocess.TimeoutExpired as exc:
        code = -1
        output = "TIMEOUT tras 360s\n" + ((exc.stdout or b"") + (exc.stderr or b"")).decode("utf-8", "replace")
    except OSError as exc:
        code, output = -2, f"no se pudo ejecutar: {exc}"
    log.write_text(f"$ {' '.join(command)}\n# exit={code}\n{output}", encoding="utf-8")
    parsed = None
    for line in reversed(output.splitlines()):
        try:
            parsed = json.loads(line)
            break
        except json.JSONDecodeError:
            continue
    if code == 0 and isinstance(parsed, dict) and parsed.get("status") == "PASS":
        result, observed = "PASS", f"Worker aislado Windows consultó Civitai API v1 en solo lectura; items={parsed.get('items')}"
    elif code == 2 and isinstance(parsed, dict) and parsed.get("status") == "BLOCKED":
        result, observed = "BLOCKED", f"Handshake y auth probe locales pasan; la consulta real no terminó: {parsed.get('reason', 'API no disponible desde este host')}"
    else:
        result, observed = "FAIL", f"exit={code}; {str(parsed)[:240] if parsed else 'transcripción inválida'}"
    ev._add(
        test_id, "REAL", "python tools/p049_live_probe.py --live",
        "Worker Windows empaquetado completa handshake y consulta read-only de modelos públicos Civitai.",
        observed, result, [log], [
            REPO / "tools/p049_live_probe.py", REPO / "tools/p049_worker.py",
            REPO / "adapters/source/p049/adapter.py", REPO / "adapters/source/civitai/capabilities.json",
        ],
    )
    print(f"    {result}: {observed[:260]}", flush=True)


def main() -> int:
    adapter_files = [
        REPO / "adapters/source/p049/adapter.py", REPO / "adapters/source/p049/main.py",
        REPO / "adapters/source/p049/package.py", REPO / "tools/p049_worker.py",
        REPO / "tools/p049_package_smoke.py", REPO / "tools/p049_live_probe.py",
        REPO / "tests/integration/p049_adapters.py",
        *sorted((REPO / "adapters/source/kaggle").glob("*")),
        *sorted((REPO / "adapters/source/ollama-library").glob("*")),
        *sorted((REPO / "adapters/source/openxlab").glob("*")),
        *sorted((REPO / "adapters/source/ngc").glob("*")),
        *sorted((REPO / "adapters/source/civitai").glob("*")),
    ]
    adapter_files = [path for path in adapter_files if path.is_file()]
    ev = Evidence("P049", extra_inputs=["contracts/REVISION_3_7_0_P052.md"])
    ev.run(
        "P049-PYTHON-CONTRACT-FOCAL", "UNIT",
        [sys.executable, "tests/integration/p049_adapters.py"],
        "Seis pruebas cubren los cinco perfiles, schemas, cursores, auth, rate limits, timeout/cancel, metadatos, tamaños y manifests separados.",
        ok=exit_zero, fixtures=adapter_files, timeout=180,
    )
    ev.run(
        "P049-P052-DOWNLOAD-CONTRACT", "UNIT",
        ["cargo", "test", "-p", "larm-adapter-sdk", "--test", "p052_source_download_contract"],
        "El SDK conserva selección opcional y el contrato de intervalos estimados añadidos por P052.",
        fixtures=[REPO / "crates/larm-adapter-sdk/tests/p052_source_download_contract.rs",
                  REPO / "crates/larm-adapter-sdk/src/source/wire.rs"],
        timeout=900,
    )
    ev.run(
        "P049-PACKAGED-WORKER", "REAL",
        [sys.executable, "tools/p049_package_smoke.py"],
        "Worker Windows aislado: hash de paquete, handshake JSON-RPC y auth probe anónimo Civitai.",
        ok=exit_zero, fixtures=adapter_files, timeout=420,
    )
    ev.run(
        "P049-PYTHON-COMPILE", "UNIT",
        [sys.executable, "-m", "py_compile", "adapters/source/p049/adapter.py",
         "adapters/source/p049/main.py", "adapters/source/p049/package.py",
         "adapters/source/kaggle/adapter.py", "adapters/source/kaggle/package.py",
         "adapters/source/ollama-library/adapter.py", "adapters/source/ollama-library/package.py",
         "adapters/source/openxlab/adapter.py", "adapters/source/openxlab/package.py",
         "adapters/source/ngc/adapter.py", "adapters/source/ngc/package.py",
         "adapters/source/civitai/adapter.py", "adapters/source/civitai/package.py",
         "tests/integration/p049_adapters.py", "tools/p049_worker.py",
         "tools/p049_package_smoke.py", "tools/p049_live_probe.py", "tools/p049_evidence.py"],
        "El adapter compartido, sus cinco entradas de provider y el tooling de evidencia compilan.",
        ok=exit_zero, fixtures=adapter_files,
    )
    ev.run(
        "P049-CONTRACT-COPIES", "CONTRACT",
        [sys.executable, "tools/package_copy.py", "check"],
        "Las copias del paquete contractual canónico permanecen íntegras tras P052.",
        ok=exit_zero, fixtures=[REPO / "tools/package_copy.py", REPO / "docs/spec/PACKAGE_COPY_HASHES.json"],
    )
    record_live_probe(ev)
    status = ev.finish(
        changed_files=[
            "adapters/source/p049/adapter.py", "adapters/source/p049/main.py", "adapters/source/p049/package.py",
            "adapters/source/kaggle/adapter.py", "adapters/source/kaggle/provider.json", "adapters/source/kaggle/capabilities.json", "adapters/source/kaggle/README.md", "adapters/source/kaggle/package.py",
            "adapters/source/ollama-library/adapter.py", "adapters/source/ollama-library/provider.json", "adapters/source/ollama-library/capabilities.json", "adapters/source/ollama-library/README.md", "adapters/source/ollama-library/package.py",
            "adapters/source/openxlab/adapter.py", "adapters/source/openxlab/provider.json", "adapters/source/openxlab/capabilities.json", "adapters/source/openxlab/README.md", "adapters/source/openxlab/package.py",
            "adapters/source/ngc/adapter.py", "adapters/source/ngc/provider.json", "adapters/source/ngc/capabilities.json", "adapters/source/ngc/README.md", "adapters/source/ngc/package.py",
            "adapters/source/civitai/adapter.py", "adapters/source/civitai/provider.json", "adapters/source/civitai/capabilities.json", "adapters/source/civitai/README.md", "adapters/source/civitai/package.py",
            "docs/adapters/p049-source-adapters.md", "docs/implementation/STATE.md",
            "docs/reports/P049_fuentes-adicionales-kaggle-ollama-library-openxlab-ngc-y-civitai.md",
            "docs/reports/P049_fuentes-adicionales-kaggle-ollama-library-openxlab-ngc-y-civitai.evidence.json",
            "tests/fixtures/p049/civitai-model-page.json", "tests/fixtures/p049/civitai-model-detail.json",
            "tests/fixtures/p049/unauthorized.json", "tests/fixtures/p049/rate-limited.json",
            "tests/integration/p049_adapters.py", "tools/p049_evidence.py", "tools/p049_live_probe.py",
            "tools/p049_package_smoke.py", "tools/p049_worker.py",
        ],
        blockers=[
            "P043, P045 y P025 no tienen REAL_PASS, requisito de cierre para P049.",
            "Kaggle requiere mapear la CLI oficial y P045 para resolver credential_reference_id; el perfil permanece UNSUPPORTED.",
            "Ollama Library, OpenXLab y NGC no tienen en este incremento un contrato de consulta pública estable cualificado; sus perfiles permanecen UNSUPPORTED y no hacen scraping.",
            "Descarga, plan y credenciales de Civitai quedan deshabilitados hasta P045 y la prueba real de aceptación.",
        ],
    )
    return 0 if status in ("CODE_PASS", "BLOCKED_REAL") else 1


if __name__ == "__main__":
    raise SystemExit(main())
