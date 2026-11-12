#!/usr/bin/env python3
"""Evidencia focal de cuentas de fuente y licencias P045."""

from evidence import Evidence, REPO, cargo_ok, exit_zero


def main() -> int:
    ev = Evidence("P045")
    ev.run(
        "P045-FMT",
        "UNIT",
        ["cargo", "fmt", "--all", "--check"],
        "El workspace Rust mantiene formato válido.",
        fixtures=[REPO / "crates/larm-domain/src/source_accounts.rs"],
    )
    ev.run(
        "P045-DOMAIN-DTO",
        "UNIT",
        ["cargo", "test", "-p", "larm-domain", "--lib", "source_accounts::tests", "-j", "1", "--", "--test-threads=1"],
        "Los DTO estrictos validan UUIDv7, hashes SHA-256, nombres y licencias.",
        ok=cargo_ok,
        fixtures=[REPO / "crates/larm-domain/src/source_accounts.rs"],
    )
    ev.run(
        "P045-SQLITE-ACCOUNTS-LICENSES",
        "INTEGRATION",
        ["cargo", "test", "-p", "larm-db", "--test", "sources", "-j", "1", "--", "--test-threads=1"],
        "SQLite conserva cuentas y referencias; licencia, outbox e idempotencia son atómicos y rechaza referencias cruzadas.",
        ok=cargo_ok,
        fixtures=[REPO / "crates/larm-db/src/source_accounts.rs", REPO / "crates/larm-db/tests/sources.rs"],
    )
    ev.run(
        "P045-API-SCOPES-AND-REPLAY",
        "INTEGRATION",
        ["cargo", "test", "-p", "larm-api", "--test", "source_accounts", "-j", "1", "--", "--test-threads=1"],
        "Las rutas respetan scopes, DTO cerrado y replay/conflicto idempotente de licencia; las respuestas no exponen secretos.",
        ok=cargo_ok,
        fixtures=[REPO / "crates/larm-api/src/source_accounts.rs", REPO / "crates/larm-api/tests/source_accounts.rs"],
    )
    ev.run(
        "P045-OPENAPI-ROUTES",
        "CONTRACT",
        ["cargo", "test", "-p", "larm-api", "--lib", "-j", "1", "p045_source_account_routes_are_published_as_implemented"],
        "Las cuatro operaciones de cuentas y la aceptación de licencia se publican como implementadas.",
        ok=cargo_ok,
        fixtures=[REPO / "crates/larm-api/src/openapi.rs", REPO / "docs/contracts/api-operations.json"],
    )
    ev.run(
        "P045-DAEMON-COMPOSITION",
        "INTEGRATION",
        ["cargo", "check", "-p", "larm-daemon", "-j", "1"],
        "El composition root conecta las rutas P045 con el repositorio SQLite.",
        ok=exit_zero,
        fixtures=[REPO / "apps/daemon/src/serve.rs"],
    )
    ev.blocked(
        "P045-SOURCE-PROBE",
        "UNIT",
        "POST /api/v1/sources/{id}/probe",
        "La solicitud crea un job durable idempotente y ejecuta el probe autenticado del adaptador source.",
        "La ruta de probe source y su consumidor durable aún no están implementados.",
    )
    ev.blocked(
        "P045-WINDOWS-SOURCE-AUTH-REAL",
        "REAL",
        "tools/check.ps1 -Prompt P045 -Real",
        "Credencial privada source se guarda, resuelve solo dentro del worker, se redacta en logs y se rota/revoca sin afectar otra cuenta.",
        "Faltan los flujos por mecanismo de P141 y un adaptador privado cualificado; no se reutilizan credenciales de integraciones distintas como fixture.",
    )
    status = ev.finish(
        changed_files=[
            "apps/daemon/src/serve.rs",
            "crates/larm-api/src/lib.rs",
            "crates/larm-api/src/openapi.rs",
            "crates/larm-api/src/source_accounts.rs",
            "crates/larm-api/tests/source_accounts.rs",
            "crates/larm-core/src/lib.rs",
            "crates/larm-core/src/source_accounts.rs",
            "crates/larm-db/src/lib.rs",
            "crates/larm-db/src/source_accounts.rs",
            "crates/larm-db/tests/sources.rs",
            "crates/larm-domain/src/lib.rs",
            "crates/larm-domain/src/ports.rs",
            "crates/larm-domain/src/source_accounts.rs",
        ],
        blockers=[
            "La ruta de sondeo source y su consumidor durable siguen pendientes.",
            "Los flujos privados por mecanismo requieren P141; falta aceptación REAL con una fuente y credencial source autorizadas.",
        ],
    )
    return 0 if status in ("CODE_PASS", "REAL_PASS", "BLOCKED_REAL") else 1


if __name__ == "__main__":
    raise SystemExit(main())
