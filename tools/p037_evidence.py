#!/usr/bin/env python3
"""Evidencia del onboarding de runtimes desde GUI/API (P037)."""

from __future__ import annotations

from evidence import Evidence, REPO, cargo_ok, exit_zero


def main() -> int:
    ev = Evidence("P037")
    dto = REPO / "crates/larm-domain/src/runtime.rs"
    api = REPO / "crates/larm-api/src/runtimes.rs"
    api_test = REPO / "apps/daemon/tests/external_hosts.rs"
    wizard = REPO / "apps/desktop/src/runtime-wizard.tsx"
    ui_test = REPO / "apps/desktop/src/runtime-wizard.test.tsx"

    ev.run(
        "P037-EXTERNAL-HOST-STRICT-DOMAIN-VALIDATION",
        "UNIT",
        [
            "cargo",
            "test",
            "-p",
            "larm-domain",
            "--lib",
            "runtime::external_host_create_tests",
            "-j",
            "1",
            "--",
            "--test-threads=1",
        ],
        expected=(
            "La creación conserva conectividad UNKNOWN y credenciales por referencia; rechaza URL no HTTP(S), "
            "userinfo, query/fragment, modo MANAGED y campos secretos/desconocidos."
        ),
        ok=cargo_ok,
        fixtures=[dto, REPO / "migrations/0001_bootstrap.sql"],
    )
    ev.run(
        "P037-OPENAPI-IMPLEMENTED-CATALOG",
        "CONTRACT",
        ["cargo", "test", "-p", "larm-api", "openapi::implemented_tests", "-j", "1", "--", "--test-threads=1"],
        expected="La operación POST /external-hosts se anuncia como implementada y coincide con el OpenAPI canónico.",
        ok=cargo_ok,
        fixtures=[REPO / "crates/larm-api/src/openapi.rs", REPO / "docs/api/openapi.json", REPO / "docs/contracts/api-operations.json"],
    )
    ev.run(
        "P037-DECLARATIVE-WIZARD-DRAFT-PREVIEW-SAVE-AND-PROBE",
        "INTEGRATION",
        ["cmd.exe", "/d", "/c", "npm run test --workspace @larm/desktop -- runtime-wizard"],
        expected=(
            "La interfaz rechaza JSON inválido sin llamar a la API, previsualiza comandos/estrategias/permisos, "
            "guarda con Idempotency-Key y consulta el job de probe con su identidad."
        ),
        ok=exit_zero,
        fixtures=[wizard, ui_test, REPO / "apps/desktop/src/runtimes.tsx"],
    )
    ev.run(
        "P037-REAL-WINDOWS-LOOPBACK-SQLITE-AND-SCOPE",
        "REAL",
        [
            "cargo",
            "test",
            "-p",
            "larm-daemon",
            "--test",
            "external_hosts",
            "windows_real_loopback_external_host_registration_sqlite_and_scope",
            "-j",
            "1",
            "--",
            "--exact",
            "--nocapture",
            "--test-threads=1",
        ],
        expected=(
            "Un servidor HTTP loopback real registra el host en SQLite como UNKNOWN/EXTERNAL; URL inválida da "
            "400, lectura sin runtimes:write da 403 y no aparece ninguna fila extra."
        ),
        ok=cargo_ok,
        fixtures=[api, dto, api_test, REPO / "migrations/0001_bootstrap.sql"],
    )
    ev.blocked(
        "P037-DRAFT-VALIDATE-AND-DRY-RUN-API-CONTRACT",
        "CONTRACT",
        "API normativa para draft manifest, validate, dry-run probe y health/version probe HTTP",
        "Una API revisa un borrador sin persistirlo y comprueba salud/versión de un host antes de guardar.",
        "El api-operations.json canónico sólo define POST /external-hosts (registro, sin probe) y POST /adapters/declarative (alta durable). "
        "No define rutas, cuerpos ni errores para draft, validate, dry-run o health/version probe de un host. La UI expone preview local y "
        "probe de conformidad después del alta mediante P026, sin atribuirle una verificación previa del runtime.",
    )
    ev.blocked(
        "P037-ADAPTER-ENABLE-DISABLE-API-CONTRACT",
        "CONTRACT",
        "API normativa para activar y desactivar adaptadores/runtime declarativos",
        "El operador habilita o deshabilita de forma durable un adaptador después de revisar sus permisos.",
        "El contrato canónico no contiene una operación enable/disable ni define transiciones de estado para ella. No se inventa una ruta ni se presenta el alta UNTRUSTED como activación.",
    )

    ev.finish(
        changed_files=[
            "crates/larm-domain/Cargo.toml",
            "crates/larm-domain/src/runtime.rs",
            "crates/larm-api/Cargo.toml",
            "apps/daemon/Cargo.toml",
            "crates/larm-api/src/runtimes.rs",
            "crates/larm-api/src/openapi.rs",
            "apps/daemon/tests/external_hosts.rs",
            "docs/api/openapi.json",
            "apps/desktop/src/api/runtimes.ts",
            "apps/desktop/src/runtime-wizard.tsx",
            "apps/desktop/src/runtime-wizard.test.tsx",
            "apps/desktop/src/runtimes.tsx",
            "apps/desktop/src/styles.css",
            "tools/p037_evidence.py",
            "docs/implementation/STATE.md",
            "docs/reports/P037_onboarding-generico-de-runtimes-desde-gui-api.md",
        ],
        blockers=[
            "P037 cubre las operaciones ya publicadas: registrar host externo y crear/probar adaptador declarativo. "
            "El alcance genérico completo requiere que el contrato publique draft/validate/dry-run, health/version probe y enable/disable.",
            "El probe real de salud de un host y la comprobación de executable nonexistent quedan pendientes mientras falten esas operaciones y P023/P030/P031/P036 no estén en REAL_PASS.",
        ],
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
