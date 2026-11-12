#!/usr/bin/env python3
"""Focal checks and Windows qualification evidence for P048."""
from __future__ import annotations

import json
import subprocess
import sys

from evidence import Evidence, REPO, cargo_ok, exit_zero


def record_live_probe(ev: Evidence) -> None:
    test_id = "P048-GITHUB-RELEASES-LIVE"
    command = [sys.executable, "tools/p048_live_probe.py"]
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
        code = -2
        output = f"no se pudo ejecutar: {exc}"
    log.write_text(f"$ {' '.join(command)}\n# exit={code}\n{output}", encoding="utf-8")
    parsed = None
    for line in reversed(output.splitlines()):
        try:
            parsed = json.loads(line)
            break
        except json.JSONDecodeError:
            continue
    if code == 0 and isinstance(parsed, dict) and parsed.get("status") == "PASS":
        result = "PASS"
        observed = f"Consulta anónima read-only en vivo mediante el worker Windows; repo={parsed.get('repo')}; items={parsed.get('items')}"
    elif code == 2 and isinstance(parsed, dict) and parsed.get("status") == "BLOCKED":
        result = "BLOCKED"
        observed = f"No se completó la consulta real: {parsed.get('reason', parsed.get('code', 'red o catálogo no disponible'))}"
    else:
        result = "FAIL"
        observed = f"exit={code}; {str(parsed)[:240] if parsed else 'transcripción inválida'}"
    ev._add(
        test_id,
        "REAL",
        "python tools/p048_live_probe.py",
        "Búsqueda anónima read-only desde el worker empaquetado contra un repositorio público con releases.",
        observed,
        result,
        [log],
        [REPO / "tools/p048_live_probe.py", REPO / "adapters/source/p048/src/lib.rs"],
    )
    print(f"    {result}: {observed[:260]}", flush=True)


def main() -> int:
    ev = Evidence("P048", extra_inputs=["contracts/source-profiles.json", "contracts/REVISION_3_6_0_P048.md"])
    adapter_files = [
        REPO / "adapters/source/p048/Cargo.toml",
        REPO / "adapters/source/p048/src/lib.rs",
        REPO / "adapters/source/p048/src/main.rs",
        REPO / "adapters/source/p048/package.py",
        REPO / "tools/p048_package_smoke.py",
        REPO / "tools/p048_http_fixture.py",
        REPO / "apps/daemon/tests/support/settings_restart.rs",
        REPO / "crates/larm-api/src/settings.rs",
        REPO / "apps/daemon/src/serve.rs",
        REPO / "crates/larm-adapter-sdk/src/server.rs",
        REPO / "crates/larm-adapter-host/src/conformance/source_http.rs",
        REPO / "crates/larm-adapter-host/src/conformance/profiles.rs",
        REPO / "apps/daemon/tests/support/source_http.rs",
        REPO / "tools/p048_live_probe.py",
        REPO / "docs/adapters/p048-source-adapters.md",
        REPO / "docs/contracts/source-profiles.json",
        REPO / "docs/contracts/REVISION_3_6_0_P048.md",
    ]
    ev.run(
        "P048-RUST-FOCAL",
        "UNIT",
        ["cargo", "test", "-p", "larm-source-p048"],
        "Las pruebas focales de seguridad de rutas, UNC, HEAD/range, límites, rates de GitHub e indexado P017 pasan en Windows.",
        ok=cargo_ok,
        fixtures=adapter_files,
        timeout=900,
    )
    ev.run(
        "P048-CLIPPY-FOCAL",
        "UNIT",
        ["cargo", "clippy", "-p", "larm-source-p048", "--all-targets", "--", "-D", "warnings"],
        "El adaptador y todos sus targets compilan sin advertencias de Clippy.",
        fixtures=adapter_files,
        timeout=900,
    )
    ev.run(
        "P048-WORKER-BUILD",
        "UNIT",
        ["cargo", "build", "-p", "larm-source-p048"],
        "Se genera el ejecutable Windows usado por los smokes empaquetados.",
        fixtures=adapter_files,
        timeout=900,
    )
    ev.run(
        "P048-LOCAL-PACKAGED-WORKER",
        "REAL",
        [sys.executable, "tools/p048_package_smoke.py"],
        "El worker Windows empaquetado completa JSON-RPC real con P017, manifiesto, revisión, estimación, bloque read-only y rechazo de búsqueda import-only.",
        fixtures=adapter_files,
        timeout=300,
    )
    ev.run(
        "P048-CONTRACT-COPIES",
        "CONTRACT",
        [sys.executable, "tools/package_copy.py", "check"],
        "Las copias contractuales locales coinciden con el paquete canónico 3.6.0.",
        fixtures=[REPO / "tools/package_copy.py", REPO / "docs/spec/PACKAGE_COPY_HASHES.json"],
        timeout=120,
    )
    ev.run(
        "P048-NATIVE-HTTP-PRODUCT",
        "REAL",
        ["cargo", "test", "-p", "larm-daemon", "--test", "reference_adapters", "source_http_product::p048_http_producto_cualifica_bloques_cancelacion_crash_y_persistencia", "-j1", "--", "--exact", "--test-threads=1"],
        "El ejecutable P048 original supera 16 observaciones mediante perfil persistente, host Windows/P022, router autenticado y SQLite: plan HTTP con ID central, bloques/reanudación, progreso/cancelación, crash, permisos y fallos HTTP. El gate físico no ejecuta RPC. No acredita descarga TCP/CAS del daemon ni otro URL/upstream.",
        ok=cargo_ok,
        fixtures=adapter_files + [REPO / "apps/daemon/tests/reference_adapters.rs", REPO / "crates/larm-adapter-host/src/conformance/source.rs"],
        timeout=900,
    )
    ev.run(
        "P048-DAEMON-TCP-DOWNLOAD",
        "REAL",
        ["cargo", "test", "-p", "larm-daemon", "--test", "reference_adapters", "p048_download::p048_daemon_tcp_rechaza_corrupcion_y_recupera_descarga_con_cas", "-j1", "--", "--exact", "--test-threads=1"],
        "El daemon TCP cualifica P048, exige confirmación del plan, rechaza contenido alterado sin CAS/procedencia y libera la reserva. Una descarga correcta conserva la pausa tras reiniciar y recupera un job RUNNING tras otra caída: Range/If-Range, SHA, CAS/procedencia y journal/outbox. El catálogo se prepara desde metadatos RPC observados; no acredita ingestión P043, GUI ni otro proveedor.",
        ok=cargo_ok,
        fixtures=adapter_files + [REPO / "apps/daemon/tests/support/p048_download.rs", REPO / "apps/daemon/tests/support/download_restart.rs", REPO / "apps/daemon/tests/support/daemon.rs", REPO / "apps/daemon/src/download_execution.rs", REPO / "crates/larm-download/src/consumer.rs", REPO / "crates/larm-db/src/download_jobs.rs"],
        timeout=900,
    )
    ev.run(
        "P048-PYTHON-COMPILE",
        "UNIT",
        [sys.executable, "-m", "py_compile", "adapters/source/p048/package.py", "tools/p048_package_smoke.py", "tools/p048_http_fixture.py", "tools/p048_live_probe.py", "tools/p048_evidence.py"],
        "El empaquetador y las herramientas focales compilan.",
        fixtures=adapter_files,
    )
    record_live_probe(ev)
    ev.blocked(
        "P048-UNC-PRIVATE-SHARE",
        "REAL",
        "source.sync y source.auth_probe sobre una UNC privada autorizada",
        "El worker lee una share Windows real bajo la identidad integrada permitida.",
        "No se configuró una share privada de prueba ni una identidad de Windows dedicada; la fixture UNC inexistente cubre el rechazo retryable, no la autorización positiva.",
    )
    status = ev.finish(
        changed_files=[
            "Cargo.toml",
            "Cargo.lock",
            "adapters/source/p048/Cargo.toml",
            "adapters/source/p048/src/lib.rs",
            "adapters/source/p048/src/main.rs",
            "adapters/source/p048/package.py",
            "docs/adapters/p048-source-adapters.md",
            "docs/contracts/source-profiles.json",
            "docs/contracts/REVISION_3_6_0_P048.md",
            "docs/implementation/STATE.md",
            "docs/reports/P048_github-releases-url-directa-local-y-unc-source-adapters.md",
            "docs/reports/P048_github-releases-url-directa-local-y-unc-source-adapters.evidence.json",
            "docs/reports/P048_daemon_tcp_2026-10-27.md",
            "docs/spec/PACKAGE_COPY_HASHES.json",
            "docs/spec/manifest.json",
            "tools/p048_evidence.py",
            "tools/p048_live_probe.py",
            "tools/p048_package_smoke.py",
            "tools/p048_http_fixture.py",
            "crates/larm-adapter-sdk/src/server.rs",
            "crates/larm-adapter-host/src/conformance.rs",
            "crates/larm-adapter-host/src/conformance/source.rs",
            "crates/larm-adapter-host/src/conformance/source_http.rs",
            "crates/larm-adapter-host/src/conformance/profiles.rs",
            "apps/daemon/tests/reference_adapters.rs",
            "apps/daemon/tests/support/source_http.rs",
            "apps/daemon/tests/support/p048_download.rs",
            "apps/daemon/tests/support/download_restart.rs",
            "apps/daemon/tests/support/settings_restart.rs",
            "crates/larm-api/src/settings.rs",
            "apps/daemon/src/serve.rs",
            "docs/reports/P003_settings_file_2026-10-27.md",
            "docs/adapters/conformance.md",
        ],
        blockers=[
            "P048 depende de REAL_PASS en P043, P045 y P017.",
            "GitHub público se cualifica sin credenciales; autenticación privada/gated sigue pendiente de P045/P141.",
            "La lectura positiva de una share UNC privada requiere una identidad y una ubicación de prueba autorizadas.",
            "Git LFS permanece sin implementar ni ejecutar; requiere resolución y cualificación propias.",
        ],
    )
    return 0 if status in ("CODE_PASS", "BLOCKED_REAL") else 1


if __name__ == "__main__":
    raise SystemExit(main())
