#!/usr/bin/env python3
"""Evidencia del incremento P031 para instalación MANAGED side-by-side."""
from __future__ import annotations

import sys

from evidence import Evidence, REPO, cargo_ok, exit_zero


def main() -> int:
    ev = Evidence("P031")
    fixtures = [
        REPO / "crates/larm-domain/src/runtime_execution.rs",
        REPO / "crates/larm-domain/src/runtime_install.rs",
        REPO / "crates/larm-adapter-host/src/conformance/qualification.rs",
        REPO / "crates/larm-runtime/src/install_service.rs",
        REPO / "crates/larm-db/tests/runtime_installation.rs",
        REPO / "crates/larm-storage/src/runtime_install.rs",
        REPO / "crates/larm-storage/src/lib.rs",
        REPO / "crates/larm-storage/src/locations.rs",
        REPO / "crates/larm-adapter-host/src/runtime.rs",
        REPO / "crates/larm-adapter-host/src/runtime/discovery.rs",
        REPO / "crates/larm-adapter-host/tests/runtime.rs",
        REPO / "crates/larm-adapter-host/examples/fake_runtime_worker.rs",
        REPO / "crates/larm-api/src/jobs.rs",
        REPO / "crates/larm-api/src/runtimes.rs",
        REPO / "apps/daemon/src/runtime_install.rs",
        REPO / "apps/daemon/src/serve.rs",
        REPO / "apps/desktop/src/api/runtimes.ts",
        REPO / "apps/desktop/src/runtimes.tsx",
        REPO / "apps/desktop/src/runtimes.test.tsx",
    ]

    ev.run(
        "P031-MANAGED-ORCHESTRATION",
        "INTEGRATION",
        [
            "cargo", "test", "-p", "larm-runtime", "--lib",
            "install_service::tests", "-j", "1", "--", "--test-threads=1",
        ],
        expected=(
            "Plan-only, rechazo de efectos globales, worker fake, publicación tras cierre, "
            "fallo con limpieza propia, reconciliación durable y previsualización de planes "
            "que requieren aprobación pasan; un fallo de IO parcial al publicar conserva el "
            "job en RUNNING y su workspace para reconciliar."
        ),
        ok=cargo_ok,
        fixtures=fixtures,
    )
    ev.run(
        "P031-INSTALL-TIMEOUT-KEEPS-GUARDS",
        "INTEGRATION",
        [
            "cargo", "test", "-p", "larm-runtime", "--lib",
            "install_service::tests::timeout_del_worker_conserva_job_running_y_sus_guards",
            "-j", "1", "--", "--exact", "--test-threads=1",
        ],
        expected=(
            "Si runtime.install vence su RPC y el worker no entrega un estado terminal, "
            "el servicio cierra la sesión pero mantiene job e instalación en RUNNING para reconciliar."
        ),
        ok=cargo_ok,
        fixtures=fixtures,
    )
    ev.run(
        "P031-NATIVE-WORKER-CRASH-DRAINS",
        "REAL",
        [
            "cargo", "test", "-p", "larm-adapter-host", "--test", "runtime",
            "crash_del_worker_durante_install_drena_io_y_conserva_el_workspace",
            "-j", "1", "--", "--exact", "--test-threads=1",
        ],
        expected=(
            "Si el proceso worker termina durante runtime.install, el host detecta AdapterCrashed, "
            "drena el IO y permite cerrar el runner; el workspace parcial queda disponible para "
            "reconciliación o limpieza del coordinador."
        ),
        ok=cargo_ok,
        fixtures=fixtures,
    )
    ev.run(
        "P031-APPROVAL-ADMISSION-GUARD",
        "INTEGRATION",
        [
            "cargo", "test", "-p", "larm-db", "--test", "runtime_installation",
            "admisiones_invalidas_por_recursos_o_profile_lock_no_escriben",
            "-j", "1", "--", "--exact", "--test-threads=1",
        ],
        expected=(
            "Un plan con approval_required no encola instalaciones ni escribe journal, "
            "locks, reservas, evento o idempotencia cuando la solicitud no puede presentar "
            "una aprobación ligada al plan."
        ),
        ok=cargo_ok,
        fixtures=fixtures,
    )
    ev.run(
        "P031-MANAGED-STORE-WINDOWS",
        "REAL",
        [
            "cargo", "test", "-p", "larm-storage", "runtime_install::tests",
            "-j", "1", "--", "--test-threads=1",
        ],
        expected=(
            "En Windows, publicación atómica en una ubicación temporal de prueba, "
            "reintento idempotente, recuperación de staging interrumpido, traversal rechazado "
            "y limpieza limitada al workspace propio; un fallo de escritura durante la copia "
            "del manifiesto, su sincronización o el renombrado atómico retiran staging y "
            "conservan intactos el origen y el destino no publicado."
        ),
        ok=cargo_ok,
        fixtures=fixtures,
    )
    ev.run(
        "P031-NATIVE-FAKE-WORKER-WINDOWS",
        "REAL",
        [
            "cargo", "test", "-p", "larm-runtime", "--lib",
            "install_service::tests::worker_fake_recibe_y_reconcilia_hash_original_tras_sellado_larm",
            "-j", "1", "--", "--exact", "--test-threads=1",
        ],
        expected=(
            "El worker nativo fixture recibe el hash original del adaptador y crea un artefacto "
            "en el workspace de su plan, bajo la raíz P026 ya autorizada."
        ),
        ok=cargo_ok,
        fixtures=fixtures,
    )
    ev.run(
        "P031-DAEMON-COMPOSITION",
        "INTEGRATION",
        [
            "cargo", "test", "-p", "larm-daemon", "--bin", "larm-daemon",
            "-j", "1", "--", "--test-threads=1",
        ],
        expected=(
            "El daemon compila y sus pruebas unitarias, incluidas la composición del publisher "
            "y los guards de descubrimiento/instalación, pasan."
        ),
        ok=cargo_ok,
        fixtures=fixtures,
    )
    ev.run(
        "P031-NATIVE-TEST-FIXTURES",
        "INTEGRATION",
        [
            "cargo", "build", "-p", "larm-adapter-sdk", "-p", "larm-adapter-host",
            "--bins", "--examples",
        ],
        expected="Los ejecutables nativos usados por las pruebas declarativas se reconstruyen.",
        ok=exit_zero,
        fixtures=fixtures,
    )
    ev.run(
        "P031-FRONTEND-RUNTIMES",
        "INTEGRATION",
        ["cmd.exe", "/c", "npm", "test", "--silent"],
        expected=(
            "La suite del frontend pasa y el formulario de instalación conserva el recibo y "
            "el estado del job durante la recarga del detalle."
        ),
        ok=exit_zero,
        fixtures=[
            REPO / "apps/desktop/src/runtimes.tsx",
            REPO / "apps/desktop/src/runtimes.test.tsx",
            REPO / "apps/desktop/package.json",
            REPO / "package.json",
        ],
    )
    ev.run(
        "P031-H01-CHANNELS-BACKUP-RESTORE",
        "INTEGRATION",
        [
            "cargo", "test", "-p", "larm-db", "--test", "integration",
            "h01_conserva_los_cinco_canales_en_backup_y_restore", "-j", "1",
            "--", "--exact", "--test-threads=1",
        ],
        expected=(
            "stable, prerelease, nightly, commit y custom se persisten y consultan como "
            "identidades distintas y sobreviven backup/restore sin cambiar el esquema."
        ),
        ok=cargo_ok,
        fixtures=fixtures + [
            REPO / "crates/larm-db/tests/integration.rs",
            REPO / "crates/larm-db/src/backup.rs",
            REPO / "crates/larm-db/src/migrate.rs",
        ],
    )
    ev.run(
        "P031-RUNNING-CANCEL-ACK-DRAIN",
        "INTEGRATION",
        [
            "cargo", "test", "-p", "larm-runtime", "--lib",
            "install_service::tests::cancela_running_tras_ack_del_worker_y_cierre_limpio",
            "-j", "1", "--", "--exact", "--test-threads=1",
        ],
        expected=(
            "Una cancelación RUNNING sólo pasa a CANCELLED después del ack cooperativo del worker, "
            "el drenaje de la llamada y el cierre limpio de la sesión."
        ),
        ok=cargo_ok,
        fixtures=fixtures,
    )
    ev.run(
        "P031-RUNNING-CANCEL-WITHOUT-ACK",
        "INTEGRATION",
        [
            "cargo", "test", "-p", "larm-runtime", "--lib",
            "install_service::tests::cancel_running_sin_ack_conserva_estado_y_guards",
            "-j", "1", "--", "--exact", "--test-threads=1",
        ],
        expected=(
            "Sin ack de cancelación del worker, el servicio deja el job RUNNING y no libera guards."
        ),
        ok=cargo_ok,
        fixtures=fixtures,
    )
    ev.run(
        "P031-RUNNING-CANCEL-TRANSACTION",
        "INTEGRATION",
        [
            "cargo", "test", "-p", "larm-db", "--test", "runtime_installation",
            "cancela_solo_jobs_queued_y_libera_guards_atomicos_con_replay",
            "-j", "1", "--", "--exact", "--test-threads=1",
        ],
        expected=(
            "La transición RUNNING→CANCELLED y la liberación de reservas/locks se confirman "
            "en una transacción cercada por actor y aceptan replay idempotente."
        ),
        ok=cargo_ok,
        fixtures=fixtures,
    )
    ev.run(
        "P031-NATIVE-RUNNING-CANCEL-ID",
        "INTEGRATION",
        [
            "cargo", "test", "-p", "larm-adapter-host", "--test", "runtime",
            "sesion_cualificada_cancela_install_usando_el_id_durable_del_plan",
            "-j", "1", "--", "--exact", "--test-threads=1",
        ],
        expected=(
            "La sesión nativa envía core.cancel con el UUIDv7 durable del plan mientras runtime.install "
            "espera; el worker fixture confirma cancelación y drena IO antes del cierre."
        ),
        ok=cargo_ok,
        fixtures=fixtures,
    )
    test_temp = REPO / "target/larm-test-tmp"
    ev.run(
        "P031-WORKSPACE-RUST",
        "INTEGRATION",
        [
            "cargo", "test", "--workspace", "--target-dir",
            "target/verification-final", "-j", "1",
        ],
        expected=(
            "La suite Rust completa pasa en Windows con target aislado, incluidas workers declarativos, "
            "publicación gestionada, adaptadores de referencia y daemon."
        ),
        ok=cargo_ok,
        fixtures=fixtures,
        timeout=1800,
        env={
            "TEMP": str(test_temp),
            "TMP": str(test_temp),
            "TMPDIR": str(test_temp),
        },
    )
    ev.run(
        "P031-FORMAT",
        "CONTRACT",
        ["cargo", "fmt", "--all", "--", "--check"],
        expected="Todo el workspace Rust cumple el formato fijado.",
        ok=exit_zero,
        fixtures=fixtures,
    )
    ev.run(
        "P031-CLIPPY",
        "INTEGRATION",
        ["cargo", "clippy", "--workspace", "--all-targets", "-j", "1", "--", "-D", "warnings"],
        expected="El workspace compila sin advertencias de Clippy.",
        ok=exit_zero,
        fixtures=fixtures,
    )
    ev.blocked(
        "P031-REAL-ENGINE-INSTALL",
        "REAL",
        "runtime.install con un perfil de runtime cualificado en P099",
        "Un runtime real queda publicado side-by-side y arranca desde la ubicación registrada.",
        "La tabla de cualificaciones P099 sigue vacía y los locks disponibles son UNQUALIFIED; "
        "la barrera de admisión impide ejecutar mecanismos reales. El fake Windows verifica "
        "worker y filesystem, pero no acredita soporte de un engine ni sustituye P099.",
    )
    status = ev.finish(
        changed_files=[
            "crates/larm-domain/src/runtime_execution.rs",
            "crates/larm-domain/src/runtime_install.rs",
            "crates/larm-adapter-host/src/conformance/qualification.rs",
            "crates/larm-adapter-host/src/runtime.rs",
            "crates/larm-adapter-host/src/runtime/discovery.rs",
            "crates/larm-adapter-host/examples/fake_runtime_worker.rs",
            "crates/larm-api/src/jobs.rs",
            "crates/larm-api/src/runtimes.rs",
            "crates/larm-runtime/src/install_service.rs",
            "crates/larm-db/tests/runtime_installation.rs",
            "crates/larm-db/tests/integration.rs",
            "crates/larm-storage/src/lib.rs",
            "crates/larm-storage/src/locations.rs",
            "crates/larm-storage/src/runtime_install.rs",
            "apps/daemon/src/runtime_install.rs",
            "apps/daemon/src/serve.rs",
            "apps/desktop/src/runtimes.tsx",
            "apps/desktop/src/runtimes.test.tsx",
            "apps/desktop/src/api/runtimes.ts",
            "tools/p031_evidence.py",
            "docs/reports/P031_descubrimiento-e-instalacion-de-versiones-de-runtime.md",
            "docs/implementation/STATE.md",
        ],
        blockers=[
            "No hay perfiles/locks P099 cualificados para ejecutar una instalación real de runtime.",
            "P030, P021 y P019 no tienen REAL_PASS; el consumidor P031 no puede cerrarse sin esas dependencias.",
            "La prueba fake de Windows no acredita los comandos reales UV/CONDA/MAMBA ni arranque de un engine.",
        ],
    )
    return 0 if status in {"CODE_PASS", "REAL_PASS", "BLOCKED_REAL"} else 1


if __name__ == "__main__":
    sys.exit(main())
