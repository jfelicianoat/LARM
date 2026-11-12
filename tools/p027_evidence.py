#!/usr/bin/env python3
"""Evidence for the safe data model and API/UI slice of P027."""

from __future__ import annotations

import sys

from evidence import Evidence, REPO, cargo_ok, exit_zero


def main() -> int:
    ev = Evidence("P027")
    fixtures = [
        REPO / "crates/larm-domain/src/custom_rules.rs",
        REPO / "crates/larm-domain/src/locations.rs",
        REPO / "crates/larm-domain/tests/custom_rules.rs",
        REPO / "crates/larm-db/src/custom_rules.rs",
        REPO / "crates/larm-api/src/custom_rules.rs",
        REPO / "crates/larm-api/src/openapi.rs",
        REPO / "apps/desktop/src/api/customRules.ts",
        REPO / "apps/desktop/src/customRules.tsx",
        REPO / "apps/desktop/src/customRules.test.tsx",
        REPO / "crates/larm-runtime/src/nuget_lock.rs",
        REPO / "crates/larm-runtime/src/install.rs",
        REPO / "crates/larm-runtime/src/install_service.rs",
        REPO / "crates/larm-runtime/tests/install.rs",
    ]
    target = REPO / "target/verification-final"
    target_env = {"CARGO_TARGET_DIR": str(target)}

    ev.run(
        "P027-CUSTOM-RULES-DOMAIN",
        "CONTRACT",
        ["cargo", "test", "-p", "larm-domain", "--test", "custom_rules", "-j", "1", "--", "--test-threads=1"],
        expected="DSL cerrado, required nullable, operadores, AND, precedencia, límites, contradicciones y targets se validan con los casos positivos y negativos.",
        ok=cargo_ok,
        fixtures=fixtures[:3],
        env=target_env,
    )
    ev.run(
        "P027-CUSTOM-RULES-PLACEMENT",
        "INTEGRATION",
        ["cargo", "test", "-p", "larm-domain", "--lib", "custom_rules", "-j", "1", "--", "--test-threads=1"],
        expected="CUSTOM_RULES se aplica tras los hard gates de ubicación; denegaciones, requisitos y límites filtran candidatos y el desempate es determinista.",
        ok=cargo_ok,
        fixtures=fixtures[:3],
        env=target_env,
    )
    ev.run(
        "P027-NUGET-LOCK-PLAN-BINDING",
        "INTEGRATION",
        ["cargo", "test", "-p", "larm-runtime", "nuget", "-j", "1", "--", "--test-threads=1"],
        expected="El lock NuGet fija arquitectura, versiones resueltas y SHA-512 de cada paquete; rechaza rutas, referencias de proyecto y locks incompletos, y cambios del hash invalidan el plan.",
        ok=cargo_ok,
        fixtures=[fixtures[9], fixtures[10], fixtures[11], fixtures[12], REPO / "crates/larm-runtime/Cargo.toml"],
        env=target_env,
    )
    ev.run(
        "P027-SCHEMA-COMPATIBILITY",
        "CONTRACT",
        [sys.executable, "-X", "utf8", "tools/p027_schema.py"],
        expected="Las formas declarativas permitidas coinciden con el schema público y se rechazan campos u operadores desconocidos.",
        ok=exit_zero,
        fixtures=[REPO / "tools/p027_schema.py", REPO / "docs/contracts/schemas/larm.schema.json"],
    )
    ev.run(
        "P027-SQLITE-CAS",
        "INTEGRATION",
        ["cargo", "test", "-p", "larm-db", "--lib", "custom_rules::tests", "-j", "1", "--", "--test-threads=1"],
        expected="SQLite conserva el DTO, incrementa revisión con CAS, rechaza escrituras obsoletas o inválidas, detecta corrupción y revierte payload, revisión y evento si falla el outbox.",
        ok=cargo_ok,
        fixtures=[fixtures[0], fixtures[3], REPO / "migrations/0001_bootstrap.sql"],
        env=target_env,
    )
    ev.run(
        "P027-API-CAS-ETAG",
        "INTEGRATION",
        ["cargo", "test", "-p", "larm-api", "--lib", "custom_rules::tests", "-j", "1", "--", "--test-threads=1"],
        expected="GET/PATCH exponen ETag, If-Match requiere revisión actual y se rechazan schema y reglas semánticamente inválidos.",
        ok=cargo_ok,
        fixtures=[fixtures[0], fixtures[4], REPO / "docs/contracts/api-operations.json"],
        env=target_env,
    )
    ev.run(
        "P027-DAEMON-WIRING",
        "INTEGRATION",
        ["cargo", "check", "-p", "larm-daemon", "--target-dir", "target/verification-final", "-j", "1"],
        expected="El daemon compila con la ruta P027 conectada al repositorio SQLite del settings table.",
        ok=exit_zero,
        fixtures=[fixtures[3], fixtures[4], REPO / "apps/daemon/src/serve.rs"],
    )
    ev.run(
        "P027-FRONTEND-PREVIEW-ETAG",
        "INTEGRATION",
        ["powershell", "-NoProfile", "-ExecutionPolicy", "Bypass", "-Command", "npm run test -- src/customRules.test.tsx"],
        expected="Configuración lee la revisión HTTP, visualiza la prioridad y el límite y guarda con If-Match.",
        ok=exit_zero,
        cwd=REPO / "apps/desktop",
        fixtures=[fixtures[6], fixtures[7], fixtures[8]],
    )
    ev.run(
        "P027-FRONTEND-QUALITY",
        "INTEGRATION",
        ["powershell", "-NoProfile", "-ExecutionPolicy", "Bypass", "-Command", "npm run build"],
        expected="TypeScript, Vite y el empaquetado de interfaz compilan la pantalla y el cliente de reglas.",
        ok=exit_zero,
        cwd=REPO / "apps/desktop",
        fixtures=[fixtures[6], fixtures[7], fixtures[8], REPO / "apps/desktop/src/App.tsx"],
    )
    ev.blocked(
        "P027-REAL-NUGET-RESTORE",
        "REAL",
        "tools/check.ps1 -Prompt P027 -Real",
        "Restaurar un paquete NuGet pequeño en una instalación LARM lateral y aplicar reglas contra un catálogo local real; comprobar aislamiento, arquitectura, versión, dependencias y hashes.",
        "Se valida y sella el lock, pero todavía no existe el worker de restore con caché privado; P023/P031 siguen sin REAL_PASS y no se declara una restauración real.",
    )

    ev.finish(
        changed_files=[
            "apps/daemon/src/serve.rs",
            "apps/desktop/src/App.tsx",
            "apps/desktop/src/api/client.ts",
            "apps/desktop/src/api/customRules.ts",
            "apps/desktop/src/customRules.test.tsx",
            "apps/desktop/src/customRules.tsx",
            "apps/desktop/src/styles.css",
            "apps/desktop/src-tauri/src/api.rs",
            "crates/larm-api/src/custom_rules.rs",
            "crates/larm-api/src/lib.rs",
            "crates/larm-api/src/openapi.rs",
            "crates/larm-db/src/custom_rules.rs",
            "crates/larm-db/src/lib.rs",
            "crates/larm-domain/src/custom_rules.rs",
            "crates/larm-domain/src/lib.rs",
            "crates/larm-domain/src/locations.rs",
            "crates/larm-domain/tests/custom_rules.rs",
            "crates/larm-runtime/Cargo.toml",
            "crates/larm-runtime/src/install.rs",
            "crates/larm-runtime/src/install_service.rs",
            "crates/larm-runtime/src/lib.rs",
            "crates/larm-runtime/src/nuget_lock.rs",
            "crates/larm-runtime/tests/install.rs",
            "docs/implementation/STATE.md",
            "docs/reports/P027_custom-rules-como-datos-y-estrategia-nuget-gestionada.md",
            "tools/p027_evidence.py",
            "tools/p027_schema.py",
        ],
        blockers=[
            "La interfaz de gestión y preview de reglas, GET/PATCH y persistencia CAS están implementados; falta enlazar la política guardada con la selección productiva del catálogo.",
            "El restore NuGet con caché privado y la conexión productiva de CUSTOM_RULES al catálogo siguen pendientes.",
        ],
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
