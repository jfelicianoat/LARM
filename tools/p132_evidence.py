#!/usr/bin/env python3
"""P132: puerta de la reconciliación de arranque — orden contractual,
protección de crash loop con persistencia, modo offline con rechazo explícito
de búsquedas remotas y huérfanos por identidad fuerte. Ejecución real sobre el
PC IA con el journal y el historial persistidos en disco."""

import sys

from evidence import Evidence, REPO

TOOL_PYTHON = REPO / ".venv-tools" / "Scripts" / "python.exe"
PYTHON = str(TOOL_PYTHON) if TOOL_PYTHON.is_file() else sys.executable


def main() -> int:
    ev = Evidence("P132")

    ev.run(
        "P132-CONTRACT-startup-order-and-crash-loop",
        "CONTRACT",
        ["cargo", "test", "-p", "larm-core", "--lib", "startup"],
        "orden contractual de 7 pasos con validación incremental y estricta, modo offline con rechazo explícito, huérfanos por identidad fuerte y digest determinista",
    )
    ev.run(
        "P132-REAL-startup-reconcile-pc-ia",
        "REAL",
        ["cargo", "test", "-p", "larm-daemon", "--test", "startup_reconcile", "--", "--nocapture"],
        "journal real del arranque con los 7 pasos contractuales (paso desconocido falla cerrado), crash loop de runtime con la política del contrato 04 (3 reinicios por 10 min, backoff [1, 5, 15] y FAILED en el cuarto), modo offline con rechazo explícito y huérfanos sin kill por ambigüedad",
        ok=lambda code, output: (
            code == 0
            and "test result: ok. 1 passed" in output
            and "REAL P132: startup_order_violation detectado" in output
            and "REAL P132: crash loop runtime con backoff 1/5/15" in output
            and "REAL P132: offline mode con rechazo" in output
            and "ORPHAN_LARM" in output
        ),
        timeout=600,
    )
    ev.blocked(
        "P132-REAL-crash-recovery-e2e",
        "REAL",
        "reconciliación end-to-end tras kill abrupto del daemon con jobs activos",
        "kill abrupto del daemon con downloads/transforms activos → arranque siguiente reconcilia downloads seguros y persiste transformaciones no reanudables como FAILED_RECOVERABLE con la causa y el journal",
        "las piezas cubiertas: H07 (public_h07) demuestra el crash con bytes parciales y la reconciliación FAILED_RECOVERABLE, y las suites de referencia del daemon reinician el proceso con recuperación (source_suite_nativa descarga_recupera, settings_restart) — la aceptación e2e dedicada con jobs concurrentes queda como extensión del REAL_PASS que las dependencias en BLOCKED_REAL ya impiden",
    )

    changed = [
        "crates/larm-core/src/lib.rs",
        "crates/larm-core/src/startup.rs",
        "apps/daemon/src/serve.rs",
        "apps/daemon/tests/startup_reconcile.rs",
        "docs/operations/recovery.md",
        "docs/implementation/STATE.md",
        "docs/reports/P132_startup-reconciliation-offline-mode-y-crash-recovery.md",
        "docs/reports/P132_startup-reconciliation-offline-mode-y-crash-recovery.evidence.json",
        "tools/p132_evidence.py",
    ]
    blockers = [
        "reconciliación end-to-end tras kill abrupto con jobs activos concurrentes: requiere la suite del workspace en el host (fallo ambiental transformation_suite confirmado en HEAD); la política y las decisiones están verificadas y la aceptación H07 de transformaciones ya cubre el crash con bytes parciales",
    ]
    status = ev.finish(changed, blockers)
    return 0 if "FAIL" not in status else 1


if __name__ == "__main__":
    raise SystemExit(main())
