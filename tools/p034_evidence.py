#!/usr/bin/env python3
"""Evidence for persistent LaunchProfiles, reproducible parameters, and UI (P034)."""

from __future__ import annotations

import sys

from evidence import Evidence, REPO, cargo_ok, exit_zero


def main() -> int:
    ev = Evidence("P034")
    fixtures = [
        REPO / "crates/larm-domain/src/launch_profile.rs",
        REPO / "crates/larm-domain/src/launch.rs",
        REPO / "crates/larm-runtime/src/profiles.rs",
        REPO / "crates/larm-db/src/launch_profiles.rs",
        REPO / "crates/larm-api/src/launch_profiles.rs",
        REPO / "crates/larm-api/src/openapi.rs",
        REPO / "apps/desktop/src/api/launchProfiles.ts",
        REPO / "apps/desktop/src/launchProfiles.tsx",
        REPO / "apps/desktop/src/runtimes.tsx",
        REPO / "apps/desktop/src/runtimes.test.tsx",
        REPO / "apps/daemon/src/serve.rs",
        REPO / "crates/larm-adapter-sdk/src/declarative.rs",
        REPO / "crates/larm-domain/src/instances.rs",
        REPO / "crates/larm-db/src/instances.rs",
    ]
    target_env = {"CARGO_TARGET_DIR": str(REPO / "target/verification-final")}

    ev.run(
        "P034-DOMAIN-HASH-AND-DIFF",
        "CONTRACT",
        ["cargo", "test", "-p", "larm-domain", "--lib", "launch_profile", "-j", "1", "--", "--test-threads=1"],
        expected="DTO cerrado, campos nullable obligatorios, hash sensible a parámetros/backend, diff JSON Pointer estable y corrupción rechazada.",
        ok=cargo_ok,
        fixtures=fixtures[:2],
        env=target_env,
    )
    ev.run(
        "P034-BUILTIN-MATERIALIZATION",
        "CONTRACT",
        ["cargo", "test", "-p", "larm-runtime", "--lib", "profiles::tests", "-j", "1", "--", "--test-threads=1"],
        expected="ECO/BALANCED/PERFORMANCE/LONG_CONTEXT materializan fórmulas de contrato 04 usando sólo hardware/contexto observado y parámetros explícitos.",
        ok=cargo_ok,
        fixtures=[fixtures[0], fixtures[2]],
        env=target_env,
    )
    ev.run(
        "P034-SQLITE-CRUD-IDEMPOTENCY-OUTBOX",
        "INTEGRATION",
        ["cargo", "test", "-p", "larm-db", "--lib", "launch_profiles::tests", "-j", "1", "--", "--test-threads=1"],
        expected="Create/replay/conflicto, edición CAS, borrado, recarga de dos parámetros y hashes, detección de corrupción y rollback conjunto de perfil/outbox/idempotencia.",
        ok=cargo_ok,
        fixtures=[fixtures[0], fixtures[3], REPO / "migrations/0001_bootstrap.sql"],
        env=target_env,
    )
    ev.run(
        "P034-API-AUTH-CRUD-ETAG",
        "INTEGRATION",
        ["cargo", "test", "-p", "larm-api", "--lib", "launch_profiles::tests", "-j", "1", "--", "--test-threads=1"],
        expected="Cinco operaciones contractuales, scope runtimes:write, query/body estrictos, idempotencia y ETag/If-Match se prueban con el puerto de repositorio; la persistencia real se prueba en el caso SQLite separado.",
        ok=cargo_ok,
        fixtures=[fixtures[0], fixtures[4], fixtures[5], REPO / "docs/contracts/api-operations.json"],
        env=target_env,
    )
    ev.run(
        "P034-INVALID-ADAPTER-OPTIONS-AND-ENV",
        "CONTRACT",
        ["cargo", "test", "-p", "larm-adapter-sdk", "--lib", "declarative::tests::simulacion_sin_ejecutar_e_inyeccion", "-j", "1", "--", "--exact", "--test-threads=1"],
        expected="El adaptador declarativo rechaza opciones desconocidas, argumentos extra y rutas inseguras antes de IO, y limita entorno al allowlist cualificado.",
        ok=cargo_ok,
        fixtures=[fixtures[11], REPO / "crates/larm-domain/src/launch.rs"],
        env=target_env,
    )
    ev.run(
        "P034-SNAPSHOT-IMMUTABILITY",
        "INTEGRATION",
        ["cargo", "test", "-p", "larm-domain", "--lib", "instances::tests::snapshot_persistido_detecta_cambios_en_plan_o_en_hash_exterior", "-j", "1", "--", "--exact", "--test-threads=1"],
        expected="Un cambio del plan efectivo o del hash externo invalida el snapshot inmutable de RuntimeInstance.",
        ok=cargo_ok,
        fixtures=[fixtures[12]],
        env=target_env,
    )
    ev.run(
        "P034-SNAPSHOT-SQLITE-ROUNDTRIP",
        "INTEGRATION",
        ["cargo", "test", "-p", "larm-db", "--lib", "instances::tests::create_transition_and_outbox_are_atomic_and_use_cas", "-j", "1", "--", "--exact", "--test-threads=1"],
        expected="El snapshot del plan y sus parámetros efectivos sobreviven a create/lectura SQLite junto a transición CAS y outbox atómico.",
        ok=cargo_ok,
        fixtures=[fixtures[12], fixtures[13], REPO / "migrations/0001_bootstrap.sql"],
        env=target_env,
    )
    ev.run(
        "P034-DESKTOP-PROFILE-WORKFLOW",
        "INTEGRATION",
        ["powershell", "-NoProfile", "-ExecutionPolicy", "Bypass", "-Command", "npm run test -- src/runtimes.test.tsx -t 'crea, copia, compara y edita perfiles reproducibles con If-Match'"],
        expected="La pantalla crea y copia perfiles, muestra los cambios efectivos y guarda una revisión con If-Match.",
        ok=exit_zero,
        cwd=REPO / "apps/desktop",
        fixtures=fixtures[6:10],
    )
    ev.run(
        "P034-DAEMON-WIRING",
        "INTEGRATION",
        ["cargo", "check", "-p", "larm-daemon", "-j", "1"],
        expected="El daemon compila con el repositorio LaunchProfile durable montado en las rutas contractuales.",
        ok=exit_zero,
        fixtures=[fixtures[3], fixtures[4], fixtures[10]],
        env=target_env,
    )
    ev.run(
        "P034-WINDOWS-EFFECTIVE-PROCESS",
        "REAL",
        ["cargo", "test", "-p", "larm-adapter-host", "--test", "runtime", "sesion_cualificada_inicia_proceso_propio_desde_snapshot_efectivo", "-j", "1", "--", "--exact", "--test-threads=1"],
        expected="En Windows, un worker fixture inicia un proceso propio con argumentos y entorno autorizados, observa el límite y snapshot efectivo, termina gracefulmente y no escribe el valor secreto en logs.",
        ok=cargo_ok,
        fixtures=[fixtures[12], REPO / "crates/larm-adapter-host/tests/runtime.rs"],
        env=target_env,
        timeout=900,
    )

    ev.blocked(
        "P034-REAL-PERSISTED-PROFILE-TO-PROCESS",
        "REAL",
        "tools/check.ps1 -Prompt P034 -Real",
        "Seleccionar un LaunchProfile cargado desde SQLite, resolverlo contra el adaptador cualificado, iniciar el proceso real desde ese perfil y comprobar snapshot y hash persistidos en RuntimeInstance.",
        "La prueba de proceso existente verifica parámetros efectivos construidos directamente, pero el daemon todavía no enlaza LaunchProfile guardado con LaunchPlan/RuntimeInstance; P033 mantiene ese flujo productivo bloqueado.",
    )

    ev.finish(
        changed_files=[
            "apps/daemon/src/serve.rs",
            "apps/desktop/src/api/launchProfiles.ts",
            "apps/desktop/src/launchProfiles.tsx",
            "apps/desktop/src/runtimes.test.tsx",
            "apps/desktop/src/runtimes.tsx",
            "apps/desktop/src/styles.css",
            "crates/larm-api/Cargo.toml",
            "crates/larm-api/src/broker.rs",
            "crates/larm-api/src/launch_profiles.rs",
            "crates/larm-api/src/lib.rs",
            "crates/larm-api/src/openapi.rs",
            "crates/larm-adapter-host/tests/worker.rs",
            "crates/larm-db/src/launch_profiles.rs",
            "crates/larm-db/src/lib.rs",
            "crates/larm-domain/src/launch_profile.rs",
            "crates/larm-domain/src/lib.rs",
            "crates/larm-runtime/src/lib.rs",
            "crates/larm-runtime/src/profiles.rs",
            "docs/implementation/STATE.md",
            "docs/reports/P034_launch-profiles-y-configuracion-reproducible.md",
            "tools/p034_evidence.py",
        ],
        blockers=[
            "Los perfiles CRUD ya son editables y comparables, pero aún no se consumen desde el flujo productivo LaunchPlan→RuntimeInstance; no se declara cumplimiento real H03 para un perfil guardado.",
            "La ruta de lista contractual P034 ahora devuelve perfiles durables; el antiguo GET configurado del Broker deja de ser la fuente de esa ruta y su consumidor debe migrar al catálogo después de cerrar el flujo P033/P030.",
        ],
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
