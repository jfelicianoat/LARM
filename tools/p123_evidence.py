#!/usr/bin/env python3
"""Evidencia repetible para la biblioteca y gestión local P123."""
import shutil
from pathlib import Path

from evidence import Evidence, REPO, cargo_ok


DESKTOP = REPO / "apps/desktop"
UI = [
    REPO / "apps/desktop/src/models.tsx",
    REPO / "apps/desktop/src/models.test.tsx",
    REPO / "apps/desktop/src/styles.css",
    REPO / "apps/desktop/src/move-model.tsx",
    REPO / "apps/desktop/src/model-storage.tsx",
    REPO / "apps/desktop/src/model-import.tsx",
    REPO / "apps/desktop/src/model-delete.tsx",
]
NODE = shutil.which("node") or "node"
NPM_CLI = str(Path(NODE).resolve().parent / "node_modules/npm/bin/npm-cli.js")


def main() -> int:
    ev = Evidence("P123")
    ev.run(
        "P123-FOCUSED-UI",
        "INTEGRATION",
        [NODE, NPM_CLI, "exec", "--", "vitest", "run", "src/models.test.tsx", "--pool=threads", "--maxWorkers=1"],
        "Biblioteca y comparación local incluyen avisos de licencia/acceso; movimiento, importación, análisis y borrado mantienen confirmaciones y límites seguros.",
        cwd=DESKTOP,
        fixtures=UI,
    )
    ev.run(
        "P123-DESKTOP-SUITE",
        "INTEGRATION",
        [NODE, NPM_CLI, "run", "test"],
        "La suite completa del escritorio pasa sin regresiones.",
        cwd=DESKTOP,
        fixtures=UI,
    )
    for test_id, script, expected in [
        ("P123-LINT", "lint", "ESLint termina sin errores."),
        ("P123-TYPECHECK", "typecheck", "TypeScript compila todos los proyectos."),
        ("P123-BUILD", "build", "El escritorio genera el bundle de producción."),
    ]:
        ev.run(test_id, "UNIT", [NODE, NPM_CLI, "run", script], expected, cwd=DESKTOP, fixtures=UI)

    task_temp = REPO / "target/test-tmp"
    task_temp.mkdir(parents=True, exist_ok=True)
    temp_env = {"TEMP": str(task_temp), "TMP": str(task_temp)}
    ev.run(
        "P123-LOCAL-SOURCE-IDEMPOTENCY",
        "INTEGRATION",
        ["cargo", "test", "-p", "larm-db", "--lib", "model_import::tests", "-j1", "--", "--test-threads=1"],
        "La carpeta canónica crea o reutiliza su fuente local; la creación deja journal y outbox auditable.",
        ok=cargo_ok,
        fixtures=[REPO / "crates/larm-db/src/model_import.rs"],
        env=temp_env,
    )
    ev.run(
        "P123-LOCAL-SOURCE-WORKER",
        "INTEGRATION",
        ["cargo", "test", "-p", "larm-models", "--test", "import_worker", "-j1", "--", "--test-threads=1"],
        "El worker usa la fuente automática de la carpeta canónica para las variantes importadas.",
        ok=cargo_ok,
        fixtures=[REPO / "crates/larm-models/src/import_worker.rs", REPO / "crates/larm-models/tests/import_worker.rs"],
        env=temp_env,
    )
    ev.blocked(
        "P123-WINDOWS-MULTIDISK-REAL",
        "REAL",
        "tools/check.ps1 -Prompt P123 -Real",
        "Biblioteca, offline, leases, movimiento e importación comprobados con daemon Windows y varios volúmenes controlados.",
        "Las pruebas de interfaz usan transporte simulado y P016/P018/P042/P098 aún no tienen REAL_PASS.",
    )
    ev.finish(
        changed_files=[
            "apps/desktop/src/models.tsx",
            "apps/desktop/src/models.test.tsx",
            "apps/desktop/src/styles.css",
            "docs/reports/P123_mis-modelos-y-gestion-multidisco.md",
            "docs/reports/P123_mis-modelos-y-gestion-multidisco.evidence.json",
            "docs/implementation/STATE.md",
            "tools/p123_evidence.py",
        ],
        blockers=[
            "No se ejecutó movimiento, importación o borrado real contra daemon con volúmenes controlados.",
            "P016/P018/P042/P098 aún no tienen REAL_PASS.",
        ],
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
