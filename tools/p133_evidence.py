#!/usr/bin/env python3
"""P133: puerta del updater de LARM, backup y rebuild — bundle determinista
con digests, guardas de leases/jobs activas del updater, verificación del
staged, backup verificado previo al update, import con staging y rebuild del
registro desde las ubicaciones (P018). Ejecución REAL sobre el daemon real
del PC IA."""

import sys

from evidence import Evidence, REPO

TOOL_PYTHON = REPO / ".venv-tools" / "Scripts" / "python.exe"
PYTHON = str(TOOL_PYTHON) if TOOL_PYTHON.is_file() else sys.executable


def main() -> int:
    ev = Evidence("P133")

    ev.run(
        "P133-CONTRACT-bundle-y-guardas",
        "CONTRACT",
        ["cargo", "test", "-p", "larm-core", "--lib", "backup"],
        "bundle determinista (manifiesto con versiones y SHA-256 por fichero), validación de import (versión de bundle/contrato, incompleto, digest alterado), staging sin tocar la instalación viva y guardas del updater (restart_decision: leases/jobs activas exigen confirmación)",
    )
    ev.run(
        "P133-CONTRACT-api-y-dominio",
        "CONTRACT",
        ["cargo", "test", "-p", "larm-api"],
        "rutas POST /api/v1/updates/larm y POST /api/v1/recovery/rebuild montadas con el guard real (ExecutePlan estricto, UUIDv7/sha256, aprobación ligada al plan_hash) y el resto de la superficie API sin regresiones (128 pruebas)",
    )
    ev.run(
        "P133-REAL-updater-backup-rebuild-pc-ia",
        "REAL",
        ["cargo", "test", "-p", "larm-daemon", "--test", "backup_recovery", "--", "--nocapture"],
        "REAL sobre el daemon real del PC IA: update con lease ACTIVA y sin confirmación bloqueado (update_blocked_active_leases); con aprobación real → 202 y job COMPLETED que verifica el staged por sha256, deja backup verificado y bundle determinista; restore a temporal verificado tabla a tabla; import con bytes alterados rechazado (backup_import_digest_mismatch); rebuild en staging COMPLETED; staged corrupto → job FAILED (update_staged_hash_mismatch)",
        ok=lambda code, output: (
            code == 0
            and "test result: ok. 1 passed" in output
            and "REAL P133: update con lease activa sin confirmacion bloqueado" in output
            and "REAL P133: update COMPLETED con backup verificado" in output
            and "REAL P133: restore a temporal verificado tabla a tabla" in output
            and "REAL P133: import con bytes alterados rechazado" in output
            and "REAL P133: rebuild en staging COMPLETED" in output
            and "REAL P133: staged con hash corrupto → job FAILED" in output
        ),
        timeout=600,
    )
    ev.blocked(
        "P133-REAL-update-binario-real",
        "REAL",
        "actualización real side-by-side con binario nuevo firmado y migración incompatible con rollback del binario",
        "un release real de LARM se instala side-by-side, la migración incompatible corre con backup previo y el rollback del binario restaura la instalación",
        "el flujo staged→verificación→backup→bundle→reinicio está probado REAL sobre el daemon real; un release firmado real con migración incompatible exige una versión nueva publicada del binario (el árbol actual es la única versión)",
    )

    changed = [
        "crates/larm-core/src/backup.rs",
        "crates/larm-core/src/lib.rs",
        "crates/larm-core/Cargo.toml",
        "crates/larm-domain/src/ports.rs",
        "crates/larm-api/src/recovery.rs",
        "crates/larm-api/src/lib.rs",
        "apps/daemon/src/recovery.rs",
        "apps/daemon/src/main.rs",
        "apps/daemon/src/serve.rs",
        "apps/daemon/tests/backup_recovery.rs",
        "docs/operations/backup-restore.md",
        "docs/implementation/STATE.md",
        "docs/reports/P133_updater-de-larm-backup-y-rebuild.md",
        "docs/reports/P133_updater-de-larm-backup-y-rebuild.evidence.json",
        "tools/p133_evidence.py",
    ]
    blockers = [
        "actualización real side-by-side con binario nuevo firmado y migración incompatible con rollback: exige un release publicado distinto del árbol actual",
    ]
    status = ev.finish(changed, blockers)
    return 0 if "FAIL" not in status else 1


if __name__ == "__main__":
    raise SystemExit(main())
