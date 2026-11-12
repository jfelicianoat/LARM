#!/usr/bin/env python3
"""Evidencia repetible de la UI de instancias y runtimes P124."""
import shutil
from pathlib import Path

from evidence import Evidence, REPO, cargo_ok


DESKTOP = REPO / "apps/desktop"
UI = [
    REPO / "apps/desktop/src/App.tsx",
    REPO / "apps/desktop/src/instances.tsx",
    REPO / "apps/desktop/src/instances.test.tsx",
    REPO / "apps/desktop/src/api/instances.ts",
    REPO / "apps/desktop/src/styles.css",
    REPO / "apps/desktop/src/runtimes.tsx",
    REPO / "apps/desktop/src/runtimes.test.tsx",
]
NODE = shutil.which("node") or "node"
NPM_CLI = str(Path(NODE).resolve().parent / "node_modules/npm/bin/npm-cli.js")


def main() -> int:
    ev = Evidence("P124")
    ev.run(
        "P124-FOCUSED-UI",
        "INTEGRATION",
        [NODE, NPM_CLI, "exec", "--", "vitest", "run", "src/instances.test.tsx", "src/runtimes.test.tsx", "--pool=threads", "--maxWorkers=1"],
        "Instancias y runtimes distinguen el proceso gestionado, confirman la parada y conservan los guards de versiones, candidatos e instalaciones externas.",
        cwd=DESKTOP,
        fixtures=UI,
    )
    ev.run(
        "P124-DESKTOP-SUITE",
        "INTEGRATION",
        [NODE, NPM_CLI, "run", "test"],
        "La suite completa del escritorio pasa sin regresiones.",
        cwd=DESKTOP,
        fixtures=UI,
    )
    for test_id, script, expected in [
        ("P124-LINT", "lint", "ESLint termina sin errores."),
        ("P124-TYPECHECK", "typecheck", "TypeScript compila todos los proyectos."),
        ("P124-BUILD", "build", "El escritorio genera el bundle de producción."),
    ]:
        ev.run(test_id, "UNIT", [NODE, NPM_CLI, "run", script], expected, cwd=DESKTOP, fixtures=UI)

    ev.run(
        "P124-INSTANCE-API",
        "INTEGRATION",
        ["cargo", "test", "-p", "larm-api", "--lib", "instances::tests", "-j1", "--", "--test-threads=1"],
        "La API aplica lectura autenticada, filtrado/paginación estable y parada idempotente con scope.",
        ok=cargo_ok,
        fixtures=[REPO / "crates/larm-api/src/instances.rs"],
    )
    ev.run(
        "P124-STOP-OWNERSHIP",
        "INTEGRATION",
        ["cargo", "test", "-p", "larm-db", "--lib", "runtime_instance_stops::tests", "-j1", "--", "--test-threads=1"],
        "La parada durable rechaza propiedad externa y conserva transacción, journal, outbox y replay.",
        ok=cargo_ok,
        fixtures=[REPO / "crates/larm-db/src/runtime_instance_stops.rs"],
    )
    ev.run(
        "P124-RUST-FORMAT",
        "UNIT",
        ["cargo", "fmt", "--all", "--", "--check"],
        "El workspace Rust conserva el formato requerido.",
        fixtures=[REPO / "Cargo.toml"],
    )
    ev.blocked(
        "P124-WINDOWS-RUNTIME-INTEGRATION",
        "REAL",
        "tools/check.ps1 -Prompt P124 -Real",
        "Rollback, candidato fallido y proceso gestionado validados en Windows con un runtime cualificado.",
        "P032/P033/P037 no han alcanzado REAL_PASS y no hay una instalación de runtime cualificada para este recorrido; no se altera un runtime externo.",
    )
    ev.finish(
        changed_files=[
            "apps/desktop/src/App.tsx",
            "apps/desktop/src/api/instances.ts",
            "apps/desktop/src/instances.tsx",
            "apps/desktop/src/instances.test.tsx",
            "apps/desktop/src/runtimes.test.tsx",
            "apps/desktop/src/styles.css",
            "docs/implementation/STATE.md",
            "docs/reports/P124_runtimes-versiones-backends-e-instancias.md",
            "docs/reports/P124_runtimes-versiones-backends-e-instancias.evidence.json",
            "tools/p124_evidence.py",
        ],
        blockers=[
            "La API contractual de RuntimeInstance no expone endpoint, memoria medida ni ubicación de logs; la UI solo muestra la estimación y el límite del snapshot efectivo.",
            "api-operations.json ya define promoción (P117) y rollback (P032), pero sus handlers de API y controles de UI no están conectados.",
            "La pantalla de runtimes aún necesita completar el flujo plan→instalación→validación→promoción y rollback usando las operaciones contractuales existentes.",
        ],
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
