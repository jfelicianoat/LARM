#!/usr/bin/env python3
"""Evidencia repetible para la cola UI de descargas y transformaciones P125."""
import shutil
from pathlib import Path

from evidence import Evidence, REPO, cargo_ok, exit_zero


DESKTOP = REPO / "apps/desktop"
NODE = shutil.which("node") or "node"
NPM_CLI = str(Path(NODE).resolve().parent / "node_modules/npm/bin/npm-cli.js")
UI = [
    REPO / "apps/desktop/src/App.tsx",
    REPO / "apps/desktop/src/App.test.tsx",
    REPO / "apps/desktop/src/downloads.tsx",
    REPO / "apps/desktop/src/downloads.test.tsx",
    REPO / "apps/desktop/src/download-controls.tsx",
    REPO / "apps/desktop/src/download-controls.test.tsx",
    REPO / "apps/desktop/src/styles.css",
]


def main() -> int:
    ev = Evidence("P125")
    ev.run(
        "P125-DOWNLOAD-API",
        "INTEGRATION",
        ["cargo", "test", "-p", "larm-api", "--test", "downloads", "-j1", "--", "--test-threads=1"],
        "La cola devuelve registros paginados con cursor firmado bajo el principal autenticado.",
        ok=cargo_ok,
        fixtures=[REPO / "crates/larm-api/src/downloads.rs", REPO / "crates/larm-api/tests/downloads.rs"],
    )
    ev.run(
        "P125-DOWNLOAD-DB-OWNERSHIP",
        "INTEGRATION",
        ["cargo", "test", "-p", "larm-db", "--lib", "download_jobs::tests::lista_solo_descargas_del_actor_y_pagina_por_fecha_e_id", "-j1", "--", "--test-threads=1"],
        "SQLite filtra por actor y pagina de forma estable por fecha e ID.",
        ok=cargo_ok,
        fixtures=[REPO / "crates/larm-db/src/download_jobs.rs"],
    )
    ev.run(
        "P125-FOCUSED-UI",
        "INTEGRATION",
        [NODE, NPM_CLI, "exec", "--", "vitest", "run", "src/downloads.test.tsx", "src/download-controls.test.tsx", "src/App.test.tsx", "--pool=threads", "--maxWorkers=1"],
        "La cola muestra datos suficientes, permite pausa/reanudación y confirma cancelación; evita doble envío y conserva la clave al recuperar una respuesta perdida sin afirmar estados finales antes de observarlos.",
        cwd=DESKTOP,
        fixtures=UI,
    )
    ev.run(
        "P125-DESKTOP-SUITE",
        "INTEGRATION",
        [NODE, NPM_CLI, "run", "test"],
        "La suite completa del escritorio pasa sin regresiones.",
        cwd=DESKTOP,
        fixtures=UI,
    )
    for test_id, script, expected in [
        ("P125-LINT", "lint", "ESLint termina sin errores."),
        ("P125-TYPECHECK", "typecheck", "TypeScript compila todos los proyectos."),
        ("P125-BUILD", "build", "El escritorio genera el bundle de producción."),
    ]:
        ev.run(test_id, "UNIT", [NODE, NPM_CLI, "run", script], expected, cwd=DESKTOP, fixtures=UI)
    ev.run(
        "P125-CRATE-SUITE",
        "INTEGRATION",
        ["cargo", "test", "-p", "larm-domain", "-p", "larm-db", "-p", "larm-api", "-j1", "--", "--test-threads=1"],
        "Las suites de dominio, persistencia y API pasan en serie.",
        ok=cargo_ok,
        fixtures=[REPO / "crates/larm-domain/src/ports.rs", REPO / "crates/larm-db/src/download_jobs.rs", REPO / "crates/larm-api/src/downloads.rs"],
    )
    ev.run(
        "P125-DAEMON-UNIT-SUITE",
        "INTEGRATION",
        ["cargo", "test", "-p", "larm-daemon", "--bin", "larm-daemon", "-j1", "--", "--test-threads=1"],
        "El binario del daemon compila y registra la ruta de descargas.",
        ok=cargo_ok,
        fixtures=[REPO / "apps/daemon/src/serve.rs"],
    )
    ev.run(
        "P125-OPENAPI",
        "INTEGRATION",
        ["cargo", "run", "-p", "larm-daemon", "--", "openapi", "--out", "docs/api/openapi.json", "--check"],
        "OpenAPI declara GET /api/v1/downloads como operación implementada.",
        ok=exit_zero,
        fixtures=[REPO / "crates/larm-api/src/openapi.rs", REPO / "docs/api/openapi.json"],
    )
    ev.run(
        "P125-RUST-FORMAT",
        "UNIT",
        ["cargo", "fmt", "--all", "--", "--check"],
        "El workspace Rust conserva el formato requerido.",
        fixtures=[REPO / "Cargo.toml"],
    )
    ev.run(
        "P125-CLIPPY",
        "UNIT",
        ["cargo", "clippy", "--workspace", "--all-targets", "-j1", "--", "-D", "warnings"],
        "Clippy termina sin advertencias en el workspace.",
        ok=exit_zero,
        fixtures=[REPO / "Cargo.toml"],
    )
    ev.blocked(
        "P125-WINDOWS-DOWNLOAD-TRANSFORM-REAL",
        "REAL",
        "tools/check.ps1 -Prompt P125 -Real",
        "Descarga con pausa/reanudación, hash fail y transformación interrumpida/reiniciada en Windows.",
        "P051 demuestra pausa/reanudación, cancelación activa con cierre del árbol y conservación de otra descarga pausada, y cancelación en cola con fuente controlada. La API también verifica ETag cambiado, hash corrupto y tres escenarios HTTP 416. Faltan GUI→daemon integrado, upstream cualificado, disco lleno, la matriz HTTP restante y el reinicio de transformaciones P093.",
    )
    ev.finish(
        changed_files=[
            "apps/desktop/src/App.tsx",
            "apps/desktop/src/App.test.tsx",
            "apps/desktop/src/downloads.tsx",
            "apps/desktop/src/downloads.test.tsx",
            "apps/desktop/src/download-controls.tsx",
            "apps/desktop/src/download-controls.test.tsx",
            "apps/desktop/src/styles.css",
            "apps/daemon/src/serve.rs",
            "crates/larm-api/src/downloads.rs",
            "crates/larm-api/src/lib.rs",
            "crates/larm-api/src/openapi.rs",
            "docs/api/openapi.json",
            "crates/larm-api/tests/downloads.rs",
            "crates/larm-db/src/download_jobs.rs",
            "crates/larm-db/src/download_controls.rs",
            "crates/larm-domain/src/ports.rs",
            "docs/implementation/STATE.md",
            "docs/reports/P125_descargas-y-transformaciones.md",
            "docs/reports/P051_controls_2026-10-26.md",
            "docs/reports/P051_active_cancel_2026-10-26.md",
            "docs/reports/P051_integrity_2026-10-26.md",
            "docs/reports/P051_http416_2026-10-26.md",
            "docs/reports/P125_descargas-y-transformaciones.evidence.json",
            "tools/p125_evidence.py",
        ],
        blockers=[
            "La vista ya conecta lectura, prioridad y controles. Falta la aceptación GUI→daemon de esos controles con un proveedor cualificado.",
            "La transformación no expone una operación HTTP de reinicio con nuevo preflight; la UI conserva el diagnóstico y no ofrece un reinicio ficticio.",
            "Faltan upstream real, speed/ETA medidos, disco lleno y la matriz HTTP restante en el recorrido completo; transformación P093 continúa sin worker cualificado ni reinicio con preflight.",
        ],
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
