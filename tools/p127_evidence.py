#!/usr/bin/env python3
"""Evidencia de UI para fuentes, cuentas y detalle de adaptadores P127."""

import sys

from evidence import Evidence, REPO, cargo_ok, exit_zero


def main() -> int:
    ev = Evidence("P127")
    python = REPO / ".venv-tools/Scripts/python.exe"
    ev.run(
        "P127-SOURCE-PRODUCT-BUILD",
        "INTEGRATION",
        ["cargo", "build", "-p", "larm-source-p048", "-j1"],
        "Prepara el ejecutable original P048 que utiliza el ensayo nativo.",
        ok=exit_zero,
        fixtures=[REPO / "adapters/source/p048/src/lib.rs"],
    )
    ev.run(
        "P127-SOURCE-SYNC-EXECUTION-BINDING",
        "UNIT",
        ["cargo", "test", "-p", "larm-daemon", "--bin", "larm-daemon", "source_watches::tests::", "-j1", "--", "--test-threads=1"],
        "source.sync se admite por el método declarado, sin exigir capacidad duplicada; el contexto de ejecución conserva el actor aprobado por P026 y sigue rechazando método ausente.",
        ok=cargo_ok,
        fixtures=[REPO / "apps/daemon/src/source_watches.rs"],
    )
    ev.run(
        "P127-SOURCE-SYNC-NATIVE-TCP",
        "REAL",
        ["cargo", "test", "-p", "larm-daemon", "--test", "reference_adapters", "source_watch_tcp::source_watch_tcp_sin_capacidad_duplicada_con_actor_aprobado_y_reinicio", "-j1", "--", "--exact", "--test-threads=1"],
        "Daemon/P048/P022/P026 reales: fuente sin capabilities opcionales sincroniza con actor aprobado, eventos/outbox y snapshot HEALTHY persistidos, reinicio, sin descarga/registro físico automáticos y principal temporal revocado. Binding y momento due preparados como fixture; no acredita ingesta P043 ni GUI.",
        ok=cargo_ok,
        timeout=900,
        fixtures=[REPO / "apps/daemon/tests/support/source_watch_tcp.rs", REPO / "apps/daemon/tests/support/p048_download.rs", REPO / "apps/daemon/tests/support/source_http.rs", REPO / "apps/daemon/tests/support/download_restart.rs", REPO / "tools/p048_http_fixture.py", REPO / "apps/daemon/src/source_watches.rs"],
    )
    ev.run(
        "P127-CONTRACT-SCHEMAS",
        "CONTRACT",
        [str(python), "tools/p026_schemas.py", "check"],
        "Los schemas de API y capacidades derivados conservan los contratos publicados.",
        ok=exit_zero,
        fixtures=[REPO / "docs/contracts/schemas/larm.schema.json", REPO / "docs/contracts/schemas/domain.schema.json"],
    )
    ev.run(
        "P127-SOURCE-SYNC-WORKER",
        "UNIT",
        ["cargo", "test", "-p", "larm-models", "watcher::tests", "-j", "1", "--", "--test-threads=1"],
        "El worker conserva compatibilidad, persiste telemetría observada y clasifica auth, 429 y fallos sin inferir valores.",
        ok=cargo_ok,
        fixtures=[REPO / "crates/larm-models/src/watcher.rs", REPO / "crates/larm-domain/src/sources.rs"],
    )
    ev.run(
        "P127-SOURCE-SYNC-PERSISTENCE",
        "INTEGRATION",
        ["cargo", "test", "-p", "larm-db", "--test", "source_watches", "-j", "1", "--", "--test-threads=1"],
        "SQLite persiste UNKNOWN, SYNCING, resultados y límites observados, mantiene row_version y recupera sincronizaciones interrumpidas con evento/outbox atómicos.",
        ok=cargo_ok,
        fixtures=[REPO / "crates/larm-db/src/source_watches.rs", REPO / "crates/larm-db/tests/source_watches.rs"],
    )
    ev.run(
        "P127-SOURCE-SYNC-MIGRATION",
        "INTEGRATION",
        ["cargo", "test", "-p", "larm-db", "--lib", "p127_migration_seeds_existing_sources_and_defaults_future_sources", "-j", "1"],
        "La migración P127 siembra fuentes existentes y crea UNKNOWN para fuentes nuevas.",
        ok=cargo_ok,
        fixtures=[REPO / "migrations/0011_p127_source_operational_snapshot.sql", REPO / "crates/larm-db/src/migrate.rs"],
    )
    ev.run(
        "P127-SOURCE-SYNC-PROTOCOL",
        "CONTRACT",
        ["cargo", "test", "-p", "larm-adapter-sdk", "--test", "source_protocol", "-j", "1", "--", "--test-threads=1"],
        "SyncResult acepta telemetría opcional compatible y rechaza límites observados incoherentes.",
        ok=cargo_ok,
        fixtures=[REPO / "crates/larm-adapter-sdk/src/source/wire.rs", REPO / "crates/larm-adapter-sdk/tests/source_protocol.rs"],
    )
    ev.run(
        "P127-DAEMON-STARTUP-RECOVERY",
        "INTEGRATION",
        ["cargo", "check", "-p", "larm-daemon", "-j", "1"],
        "El daemon compone la recuperación de source.sync interrumpidos con el worker P050 al arrancar.",
        ok=exit_zero,
        fixtures=[REPO / "apps/daemon/src/source_watches.rs"],
    )
    ev.run(
        "P127-SOURCES-ADAPTERS-UI",
        "INTEGRATION",
        ["node", "node_modules/vitest/vitest.mjs", "run", "--pool=threads", "--maxWorkers=1", "--root", "apps/desktop", "src/sources.test.tsx", "src/adapters.test.tsx"],
        "La UI enmascara secretos, limpia campos Basic, avisa de firma ausente y protocolo incompatible, muestra permisos y permite reintentar lecturas.",
        ok=exit_zero,
        fixtures=[
            REPO / "apps/desktop/src/sources.tsx",
            REPO / "apps/desktop/src/sources.test.tsx",
            REPO / "apps/desktop/src/adapters.tsx",
            REPO / "apps/desktop/src/adapters.test.tsx",
        ],
    )
    ev.run(
        "P127-PACKAGE-COPY",
        "CONTRACT",
        [str(python), "tools/package_copy.py", "check"],
        "La copia normativa local coincide con el paquete contractual vigente.",
        ok=exit_zero,
        fixtures=[REPO / "docs/spec/PACKAGE_COPY_HASHES.json", REPO / "docs/contracts/00_PACKAGE.md"],
    )
    ev.run(
        "P127-PACKAGE-VERIFY",
        "CONTRACT",
        [str(python), "../LARM_prompts_v3/tools/verify_pack.py"],
        "El paquete vigente supera las reglas contractuales, la migración SQLite y los hashes normativos.",
        ok=exit_zero,
        fixtures=[REPO / "docs/contracts/REVISION_3_10_2_P127.md", REPO / "docs/contracts/migrations/0009_p127_source_operational_snapshot.sql"],
    )
    ev.run(
        "P127-DESKTOP-TYPECHECK",
        "INTEGRATION",
        ["node", "../../node_modules/typescript/bin/tsc", "-b"],
        "La UI tipada consume el snapshot operativo publicado por el contrato.",
        ok=exit_zero,
        cwd=REPO / "apps/desktop",
        fixtures=[REPO / "apps/desktop/src/sources.tsx"],
    )
    ev.run(
        "P127-DESKTOP-LINT",
        "INTEGRATION",
        ["node", "../../node_modules/eslint/bin/eslint.js", "."],
        "La interfaz de Fuentes y las pruebas cumplen las reglas del proyecto.",
        ok=exit_zero,
        cwd=REPO / "apps/desktop",
        fixtures=[REPO / "apps/desktop/src/sources.tsx", REPO / "apps/desktop/src/sources.test.tsx"],
    )
    ev.blocked(
        "P127-ACCOUNT-CREDENTIAL-DELETE",
        "INTEGRATION",
        "Borrado de cuenta y su CredentialReference asociado desde Fuentes",
        "El borrado retira de forma coordinada la referencia y el secreto protegido sin dejar credenciales huérfanas.",
        "La revisión automática rechazó el cambio por la eliminación permanente del secreto y requiere autorización explícita del operador.",
    )
    ev.run(
        "P127-ADAPTER-PACKAGE-PREVIEW",
        "INTEGRATION",
        ["node", "node_modules/vitest/vitest.mjs", "run", "--pool=threads", "--maxWorkers=1", "--root", "apps/desktop", "src/adapter-import.test.tsx"],
        "La revisión local muestra identidad, publicador, permisos, tamaño exacto y SHA-256; invalida la confirmación al cambiar la ruta y sólo crea el job tras confirmación explícita.",
        ok=exit_zero,
        fixtures=[
            REPO / "apps/desktop/src/adapter-import.tsx",
            REPO / "apps/desktop/src/adapter-import.test.tsx",
        ],
    )
    ev.blocked(
        "P127-SOURCE-OPERATIONS-REAL",
        "REAL",
        "tools/check.ps1 -Prompt P127 -Real en un PC IA con fuentes autorizadas",
        "Verificar estado operativo, límite de solicitudes y última sincronización con resultados del origen, sin inventar valores.",
        "La revisión 3.10.2 ya tipa y persiste estas observaciones; falta ejecutar el worker contra una fuente real autorizada y revisar sus resultados.",
    )
    status = ev.finish(
        changed_files=[
            "apps/desktop/src/App.tsx",
            "apps/desktop/src/adapters.tsx",
            "apps/desktop/src/adapters.test.tsx",
            "apps/desktop/src/adapter-import.tsx",
            "apps/desktop/src/adapter-import.test.tsx",
            "apps/desktop/src/sources.tsx",
            "apps/desktop/src/sources.test.tsx",
            "crates/larm-adapter-sdk/src/source/wire.rs",
            "crates/larm-adapter-sdk/tests/source_protocol.rs",
            "crates/larm-db/src/migrate.rs",
            "crates/larm-db/src/source_watches.rs",
            "crates/larm-db/tests/source_watches.rs",
            "crates/larm-domain/src/ports.rs",
            "crates/larm-domain/src/sources.rs",
            "crates/larm-domain/src/watchers.rs",
            "crates/larm-models/src/watcher.rs",
            "migrations/0011_p127_source_operational_snapshot.sql",
            "apps/daemon/src/source_watches.rs",
            "apps/daemon/tests/reference_adapters.rs",
            "apps/daemon/tests/support/source_watch_tcp.rs",
            "apps/daemon/tests/support/p048_download.rs",
            "apps/daemon/tests/support/source_http.rs",
            "apps/daemon/tests/support/download_restart.rs",
            "crates/larm-adapter-host/src/conformance/source_http.rs",
            "docs/reports/P127_sync_binding_2026-10-27.md",
            "tools/p127_evidence.py",
            "docs/contracts/schemas/domain.schema.json",
            "docs/contracts/schemas/larm.schema.json",
            "docs/contracts/migrations/0009_p127_source_operational_snapshot.sql",
            "docs/contracts/REVISION_3_10_2_P127.md",
            "docs/spec/PACKAGE_COPY_HASHES.json",
            "docs/implementation/STATE.md",
            "docs/reports/P127_fuentes-cuentas-y-adaptadores.md",
            "docs/reports/check_P127_real.json",
        ],
        blockers=[
            "La eliminación segura de cuentas con credenciales requiere autorización explícita tras el rechazo de revisión automática.",
            "La prueba contra una fuente real autorizada no se ha ejecutado.",
        ],
    )
    return 0 if status in ("CODE_PASS", "REAL_PASS", "BLOCKED_REAL") else 1


if __name__ == "__main__":
    sys.exit(main())
