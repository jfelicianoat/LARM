#!/usr/bin/env python3
"""Evidencia repetible para el detalle de modelo y el preflight P122."""
from pathlib import Path
import shutil

from evidence import Evidence, REPO


DESKTOP = REPO / "apps/desktop"
UI = [
    REPO / "apps/desktop/src/models.tsx",
    REPO / "apps/desktop/src/models.test.tsx",
    REPO / "apps/desktop/src/styles.css",
]
NODE = shutil.which("node") or "node"
NPM_CLI = str(Path(NODE).resolve().parent / "node_modules/npm/bin/npm-cli.js")


def main() -> int:
    ev = Evidence("P122")
    ev.run(
        "P122-FOCUSED-UI",
        "INTEGRATION",
        [NODE, NPM_CLI, "exec", "--", "vitest", "run", "src/models.test.tsx", "--pool=threads", "--maxWorkers=1"],
        "La pantalla compara variantes y destinos y exige confirmación explícita de un plan vigente; impide doble envío, recupera respuestas perdidas con la misma clave y muestra rechazos de autorización sin afirmar que se creó un trabajo.",
        cwd=DESKTOP,
        fixtures=UI,
    )
    ev.run(
        "P122-DESKTOP-SUITE",
        "INTEGRATION",
        [NODE, NPM_CLI, "run", "test"],
        "La suite completa del escritorio pasa sin regresiones.",
        cwd=DESKTOP,
        fixtures=UI,
    )
    for test_id, script, expected in [
        ("P122-LINT", "lint", "ESLint termina sin errores."),
        ("P122-TYPECHECK", "typecheck", "TypeScript compila todos los proyectos."),
        ("P122-BUILD", "build", "El escritorio genera el bundle de producción."),
    ]:
        ev.run(
            test_id,
            "UNIT",
            [NODE, NPM_CLI, "run", script],
            expected,
            cwd=DESKTOP,
            fixtures=UI,
        )
    ev.blocked(
        "P122-WINDOWS-DAEMON-REAL",
        "REAL",
        "tools/check.ps1 -Prompt P122 -Real",
        "Preflight y ejecución verificados contra daemon Windows y ubicaciones controladas, con autorización real de fuente y licencia.",
        "P051 ya demuestra admisión HTTP y reinicio con una fuente controlada cualificada; faltan upstream real, autorización de credenciales/licencias y el recorrido de la UI contra el daemon. P092/P121 aún no tienen REAL_PASS; las pruebas de interfaz usan respuestas simuladas.",
    )
    ev.finish(
        changed_files=[
            "apps/desktop/src/models.tsx",
            "apps/desktop/src/models.test.tsx",
            "apps/desktop/src/styles.css",
            "docs/reports/P122_detalle-de-modelo-variantes-y-preflight-de-descarga.md",
            "docs/reports/P122_detalle-de-modelo-variantes-y-preflight-de-descarga.evidence.json",
            "docs/implementation/STATE.md",
            "tools/p122_evidence.py",
        ],
        blockers=[
            "La UI presenta los datos declarados, pero la aceptación de licencia y el acceso real se deben validar en la ejecución P051.",
            "Falta el recorrido real en Windows con daemon y ubicaciones controladas.",
        ],
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
