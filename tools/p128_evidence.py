#!/usr/bin/env python3
"""Evidencia focal del centro de notificaciones, watchers y H09 para P128."""

import sys

from evidence import Evidence, REPO, cargo_ok, exit_zero


def main() -> int:
    ev = Evidence("P128")
    python = REPO / ".venv-tools/Scripts/python.exe"

    ev.run(
        "P128-PACKAGE-COPY",
        "CONTRACT",
        [str(python), "tools/package_copy.py", "check"],
        "Las copias contractuales locales coinciden con el paquete v3 vigente.",
        ok=exit_zero,
        fixtures=[REPO / "docs/spec/PACKAGE_COPY_HASHES.json"],
    )
    ev.run(
        "P128-CAPABILITY-SCHEMAS",
        "CONTRACT",
        [str(python), "tools/p026_capability_schemas.py", "check"],
        "Los 18 contratos de capacidades mantienen sus 36 schemas autocontenidos y el índice coincide con los schemas canónicos actuales.",
        ok=exit_zero,
        fixtures=[
            REPO / "docs/adapters/capability-schemas/index.json",
            REPO / "crates/larm-adapter-sdk/src/capabilities/builtin.rs",
            REPO / "docs/contracts/capability-contracts.json",
            REPO / "docs/contracts/schemas/larm.schema.json",
            REPO / "docs/contracts/schemas/domain.schema.json",
        ],
    )
    ev.run(
        "P128-REFERENCE-CAPABILITIES",
        "CONTRACT",
        [str(python), "tools/p026_reference_capabilities.py", "check"],
        "Los descriptores fuente/runtime de referencia se derivan de las firmas y fixtures de contrato vigentes.",
        ok=exit_zero,
        fixtures=[
            REPO / "adapters/source/reference/capabilities.json",
            REPO / "adapters/runtime/reference/capabilities.json",
            REPO / "tools/p026_reference_capabilities.py",
        ],
    )
    ev.run(
        "P128-WATCH-QUERY-VALIDATION",
        "UNIT",
        ["cargo", "test", "-p", "larm-domain", "watch_query_accepts_exact_ranges_and_rejects_ambiguous_ranges", "-j", "1"],
        "El watcher valida filtros estrictos, tamaños exactos y rangos sin ambigüedad.",
        ok=cargo_ok,
        fixtures=[REPO / "crates/larm-domain/src/watchers.rs"],
    )
    ev.run(
        "P128-WATCH-QUERY-MATCHING",
        "UNIT",
        ["cargo", "test", "-p", "larm-models", "query_filters_capabilities_and_only_exact_size_values", "-j", "1"],
        "El worker aplica filtros de capacidad, compatibilidad y tamaño sin tratar estimaciones/desconocidos como exactos.",
        ok=cargo_ok,
        fixtures=[REPO / "crates/larm-models/src/watcher.rs"],
    )
    ev.run(
        "P128-WATCH-EDIT-API",
        "INTEGRATION",
        ["cargo", "test", "-p", "larm-api", "--test", "source_watches", "source_watch_routes_enforce_contract_scope_dto_replay_and_if_match", "-j", "1", "--", "--test-threads=1"],
        "Crear y editar watchers exige autorización, DTO cerrado, idempotencia y revisión CAS.",
        ok=cargo_ok,
        fixtures=[REPO / "crates/larm-api/src/source_watches.rs", REPO / "crates/larm-api/tests/source_watches.rs"],
    )
    ev.run(
        "P128-OPENAPI-ROUTES",
        "CONTRACT",
        ["cargo", "test", "-p", "larm-api", "p128_notification_and_watch_routes_are_published_as_implemented", "-j", "1"],
        "OpenAPI marca las rutas reales de notificaciones y CRUD de watchers como implementadas.",
        ok=cargo_ok,
        fixtures=[REPO / "crates/larm-api/src/openapi.rs", REPO / "docs/contracts/api-operations.json"],
    )
    ev.run(
        "P128-NOTIFICATION-API",
        "INTEGRATION",
        ["cargo", "test", "-p", "larm-api", "--test", "notifications", "notification_routes_enforce_scope_strict_patch_and_if_match", "-j", "1", "--", "--test-threads=1"],
        "La API de notificaciones valida permisos, rechaza campos extra y aplica cambios de lectura con If-Match.",
        ok=cargo_ok,
        fixtures=[REPO / "crates/larm-api/src/notifications.rs", REPO / "crates/larm-api/tests/notifications.rs"],
    )
    ev.run(
        "P128-NOTIFICATION-PERSISTENCE",
        "UNIT",
        ["cargo", "test", "-p", "larm-db", "--lib", "notifications::tests", "-j", "1", "--", "--test-threads=1"],
        "La lectura CAS y su outbox son atómicas; la paginación mantiene un cursor estable y no duplica mutaciones noop.",
        ok=cargo_ok,
        fixtures=[REPO / "crates/larm-db/src/notifications.rs"],
    )
    ev.run(
        "P128-H09-PHYSICAL-VOLUME",
        "INTEGRATION",
        ["cargo", "test", "-p", "larm-db", "--test", "storage", "h09_emite_una_alerta_por_volumen_y_transicion_y_omite_offline", "-j", "1", "--", "--test-threads=1"],
        "H09 respeta la igualdad, emite una alerta por volumen y transición, omite discos offline y reevalúa al cambiar Settings.",
        ok=cargo_ok,
        fixtures=[REPO / "migrations/0012_p128_low_disk_transition.sql", REPO / "crates/larm-db/tests/storage.rs"],
    )
    ev.run(
        "P128-H09-SETTINGS-REVISION",
        "UNIT",
        ["cargo", "test", "-p", "larm-api", "changing_h09_thresholds_applies_the_saved_revision", "-j", "1"],
        "Al guardar umbrales nuevos se propaga la revisión persistida para reevaluar el volumen.",
        ok=cargo_ok,
        fixtures=[REPO / "crates/larm-api/src/settings.rs", REPO / "crates/larm-core/src/hardware.rs"],
    )
    ev.run(
        "P128-DESKTOP-NOTIFICATIONS",
        "INTEGRATION",
        ["node", "node_modules/vitest/vitest.mjs", "run", "--pool=threads", "--maxWorkers=1", "--root", "apps/desktop", "src/pages/Notifications/notifications.test.tsx", "src/settings.test.tsx"],
        "La interfaz deduplica avisos, limita enlaces locales, crea watches con tamaños exactos y guarda H09 en PERCENT/BYTES.",
        ok=exit_zero,
        fixtures=[
            REPO / "apps/desktop/src/pages/Notifications/index.tsx",
            REPO / "apps/desktop/src/pages/Notifications/notifications.test.tsx",
            REPO / "apps/desktop/src/settings.tsx",
            REPO / "apps/desktop/src/settings.test.tsx",
        ],
    )
    ev.blocked(
        "P128-WINDOWS-PC-IA-REAL",
        "REAL",
        "tools/check.ps1 -Prompt P128 -Real; acceptance Windows/PC IA sobre datos y ubicaciones de prueba",
        "Ejecutar watchers, notificaciones y transiciones físicas H09 en el daemon real, con host, fixture y resultados observados.",
        "Las pruebas de código pasan, pero P050, P113, P032, P097 y P119 aún no tienen REAL_PASS; no se puede atribuir aceptación productiva a la integración local.",
    )

    changed_files = [
            "apps/daemon/src/hardware.rs",
            "apps/daemon/src/serve.rs",
            "apps/desktop/src/App.tsx",
            "apps/desktop/src/api/notifications.ts",
            "apps/desktop/src/api/settings.ts",
            "apps/desktop/src/pages/Notifications/index.tsx",
            "apps/desktop/src/pages/Notifications/notifications.test.tsx",
            "apps/desktop/src/pages/Updates/index.tsx",
            "apps/desktop/src/models.tsx",
            "apps/desktop/src/runtimes.tsx",
            "apps/desktop/src/settings.test.tsx",
            "apps/desktop/src/settings.tsx",
            "apps/desktop/src/sources.tsx",
            "apps/desktop/src/storage.tsx",
            "apps/desktop/src/styles.css",
            "adapters/source/reference/capabilities.json",
            "adapters/runtime/reference/capabilities.json",
            "crates/larm-api/src/notifications.rs",
            "crates/larm-api/src/openapi.rs",
            "crates/larm-api/src/settings.rs",
            "crates/larm-api/tests/notifications.rs",
            "crates/larm-core/src/hardware.rs",
            "crates/larm-adapter-sdk/src/capabilities/builtin.rs",
            "crates/larm-core/src/source_credentials.rs",
            "crates/larm-db/src/migrate.rs",
            "crates/larm-db/src/notifications.rs",
            "crates/larm-db/src/storage.rs",
            "crates/larm-db/tests/integration.rs",
            "crates/larm-db/tests/storage.rs",
            "crates/larm-domain/src/ports.rs",
            "crates/larm-domain/src/watchers.rs",
            "crates/larm-models/src/watcher.rs",
            "migrations/0012_p128_low_disk_transition.sql",
            "tools/p128_evidence.py",
            "tools/p026_capability_schemas.py",
            "tools/p026_reference_capabilities.py",
        ]
    changed_files.extend(
        path.relative_to(REPO).as_posix()
        for path in sorted((REPO / "docs/adapters/capability-schemas").glob("*.json"))
    )
    status = ev.finish(
        changed_files=changed_files,
        blockers=[
            "La aceptación obligatoria del comportamiento integrado en Windows/PC IA sobre datos y ubicaciones de prueba sigue pendiente.",
        ],
    )
    return 0 if status in ("CODE_PASS", "REAL_PASS", "BLOCKED_REAL") else 1


if __name__ == "__main__":
    sys.exit(main())
