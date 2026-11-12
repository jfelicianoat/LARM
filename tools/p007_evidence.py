#!/usr/bin/env python3
"""P007 - CI local, quality gates y fixtures comunes: pruebas y evidencia."""
import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from evidence import REPO, Evidence, cargo_ok  # noqa: E402

PY = sys.executable
PS = ["powershell", "-NoProfile", "-ExecutionPolicy", "Bypass", "-File"]


def coverage_ok(code: int, _out: str) -> bool:
    path = REPO / "docs/reports/coverage.json"
    if code != 0 or not path.is_file():
        return False
    totals = json.loads(path.read_text(encoding="utf-8"))["data"][0]["totals"]
    return totals["lines"]["count"] > 0


def main() -> int:
    ev = Evidence("P007")
    ev.run("P007-CONTRACT-tags", "CONTRACT", [PY, "tools/verify_p007.py", "tags"],
           "ningún test omitido sin etiqueta real-hardware/network/destructive-disabled")
    ev.run("P007-FAULT-tags-untagged", "FAULT", [PY, "tools/verify_p007.py", "tags-neg"],
           "omisiones sin etiqueta detectadas; etiquetadas aceptadas")
    ev.run("P007-CONTRACT-selector", "CONTRACT", [PY, "tools/verify_p007.py", "selector"],
           "selector por prompt y escenario; escenario sin runner -> BLOCKED (exit 2)")
    ev.run("P007-INTEGRATION-jobs", "INTEGRATION", ["cargo", "test", "-p", "larm-api", "--test", "jobs"],
           "GET /jobs/{id}: fuentes registradas, visibilidad por principal, 400/404/503", ok=cargo_ok)
    for name, expected in [("jsonrpc", "worker JSON-RPC falso: 9 modos con procesos reales"),
                           ("http", "servidor HTTP falso: Range, ETag, 429, HEAD, corte, registro sin secretos"),
                           ("sparse", "fichero sparse de 8 GiB que ocupa KiB; denso exacto")]:
        ev.run(f"P007-INTEGRATION-fixture-{name}", "INTEGRATION", [PY, "tools/verify_fixtures.py", name], expected, timeout=600)
    ev.run("P007-UNIT-db-temp-helper", "UNIT", ["cargo", "test", "-p", "larm-db", "--test", "integration", "--", "helper_de_base_temporal"],
           "base temporal migrada por prueba que se borra al soltarla", ok=cargo_ok)
    ev.run("P007-REAL-check-ps1", "REAL", PS + ["tools/check.ps1"],
           "tools/check.ps1 completo (Rust, shell Tauri, arquitectura, copia, frontend) en PASS", timeout=3600)
    ev.run("P007-REAL-integration-suite", "REAL", PS + ["tools/test-integration.ps1"],
           "suite de integración base en PASS", timeout=3600)
    ev.run("P007-REAL-coverage", "REAL", PS + ["tools/check.ps1", "-SkipFrontend", "-SkipDesktop", "-Coverage"],
           "cobertura medida con cargo-llvm-cov 0.9.1 fijado en tools/.bin", ok=coverage_ok, timeout=3600)
    changed = [
        "tools/check.ps1", "tools/test-integration.ps1", "tools/verify_p007.py", "tools/verify_fixtures.py",
        "tools/make_sparse.py", "tools/p007_evidence.py", "tests/fixtures/http/http_fake.py",
        "tests/fixtures/workers/jsonrpc_fake.py", "docs/operations/testing.md", "rust-toolchain.toml", ".gitignore",
        "crates/larm-domain/src/ports.rs", "crates/larm-api/src/jobs.rs", "crates/larm-api/src/broker.rs",
        "crates/larm-api/src/lib.rs", "crates/larm-api/tests/jobs.rs", "apps/daemon/src/serve.rs",
    ]
    ev.finish(changed, [])
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
