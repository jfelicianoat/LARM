#!/usr/bin/env python3
"""Evidencia P090: gates deterministas; ejecución/admisión real sigue bloqueada."""

from evidence import Evidence, REPO, cargo_ok


def main() -> int:
    ev = Evidence("P090")
    domain = REPO / "crates/larm-domain/src/compatibility.rs"
    core = REPO / "crates/larm-core/src/compatibility.rs"
    api = REPO / "crates/larm-api/src/compatibility.rs"
    db = REPO / "crates/larm-db/src/compatibility.rs"
    openapi = REPO / "crates/larm-api/src/openapi.rs"
    schema_test = REPO / "tools/p090_schema_evidence.py"

    ev.run(
        "P090-FMT",
        "UNIT",
        ["cargo", "fmt", "--all", "--check"],
        "El workspace Rust cumple el formato establecido.",
        fixtures=[domain, core, api, db, openapi],
    )
    ev.run(
        "P090-DOMAIN-SCHEMA-AND-IDENTITY",
        "CONTRACT",
        ["cargo", "test", "-p", "larm-domain", "compatibility::tests", "-j", "1", "--", "--test-threads=1"],
        "El DTO cerrado valida UUIDv7, timestamps, hashes, gates requeridos y scoring condicionado.",
        fixtures=[domain],
        ok=cargo_ok,
    )
    ev.run(
        "P090-ENGINE-HARD-GATES",
        "CONTRACT",
        ["cargo", "test", "-p", "larm-core", "compatibility::tests", "-j", "1", "--", "--test-threads=1"],
        "Format FAIL, arquitectura UNKNOWN, backend ausente, memoria insuficiente y bundle incompleto se resuelven sin score.",
        fixtures=[core, domain],
        ok=cargo_ok,
    )
    ev.run(
        "P090-API-LIST",
        "CONTRACT",
        ["cargo", "test", "-p", "larm-api", "compatibility::tests", "-j", "1", "--", "--test-threads=1"],
        "GET /api/v1/compatibility devuelve {items,next_cursor,snapshot_at} bajo el contrato de respuesta.",
        fixtures=[api, openapi],
        ok=cargo_ok,
    )
    ev.run(
        "P090-OPENAPI-IMPLEMENTATION",
        "CONTRACT",
        ["cargo", "run", "-p", "larm-daemon", "--", "openapi", "--out", "docs/api/openapi.json", "--check"],
        "OpenAPI refleja GET /api/v1/compatibility como implementado desde el router real.",
        fixtures=[openapi, REPO / "docs/api/openapi.json"],
    )
    ev.run(
        "P090-SQLITE-READ",
        "CONTRACT",
        ["cargo", "test", "-p", "larm-db", "--test", "compatibility", "-j", "1", "--", "--test-threads=1"],
        "El repositorio lee el registro contractual desde SQLite migrado y devuelve una lista vacía válida.",
        fixtures=[db, domain],
        ok=cargo_ok,
    )
    ev.run(
        "P090-CANONICAL-JSON-SCHEMA",
        "CONTRACT",
        [".venv-tools/Scripts/python.exe", "tools/p090_schema_evidence.py"],
        "El schema v3 acepta el registro canónico y rechaza campo extra, enum inválido, campo ausente y SHA malformado.",
        fixtures=[schema_test],
    )
    ev.run(
        "P090-CLIPPY-FOCAL",
        "CONTRACT",
        ["cargo", "clippy", "-p", "larm-domain", "-p", "larm-core", "-p", "larm-db", "-p", "larm-api", "--all-targets", "-j", "1", "--", "-D", "warnings"],
        "Dominio, motor, persistencia y API pasan Clippy sin warnings.",
        fixtures=[domain, core, db, api],
    )
    ev.run(
        "P090-ARCHITECTURE",
        "CONTRACT",
        ["powershell", "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", "tools/check_architecture.ps1"],
        "El grafo de dependencias conserva los límites de dominio, core, DB y API.",
        fixtures=[domain, core, db, api, REPO / "docs/contracts/crate-graph.json"],
    )
    ev.run(
        "P090-REAL-WINDOWS-ENGINE",
        "REAL",
        ["cargo", "test", "-p", "larm-core", "compatibility::tests", "-j", "1", "--", "--test-threads=1"],
        "El motor ejecuta sus fixtures de rechazo y UNKNOWN en el host Windows actual; la salida incluye host y versiones en PromptEvidence.",
        fixtures=[core, domain],
        ok=cargo_ok,
    )
    ev.blocked(
        "P090-REAL-RUNTIME-ADMISSION",
        "REAL",
        "Evaluar y persistir gates contra un modelo, runtime, backend y perfil cualificados en Windows",
        "El registro refleja evidencia real del tuple y sólo es COMPATIBLE cuando todas las puertas requeridas pasan.",
        "El módulo sólo consume observaciones; no está conectado aún a support matrix/version probe, perfiles, hardware/bundle, admisión ni persistencia de evaluaciones. Las dependencias P013/P035/P041/P030/P019 no tienen REAL_PASS.",
    )

    blockers = [
        "La combinación pura y el DTO están implementados, pero la adquisición de evidencias de Adapter, hardware, bundle, licencia y memory admission aún no compone el tuple real.",
        "La API canónica es de solo lectura: el cálculo no tiene operación contractual de escritura ni worker/repository para persistir un nuevo CompatibilityRecord.",
        "P013, P035, P041, P030 y P019 no tienen REAL_PASS; P090 queda BLOCKED_REAL según contrato 00.",
        "TechnicalFitScore pertenece a P091; P090 deja technical_fit null incluso con PASS y no declara puntuaciones antes de ese incremento.",
    ]
    ev.finish(
        changed_files=[
            "crates/larm-domain/src/lib.rs",
            "crates/larm-domain/src/compatibility.rs",
            "crates/larm-domain/src/ports.rs",
            "crates/larm-core/src/lib.rs",
            "crates/larm-core/src/compatibility.rs",
            "crates/larm-db/src/lib.rs",
            "crates/larm-db/src/compatibility.rs",
            "crates/larm-db/tests/compatibility.rs",
            "crates/larm-api/src/lib.rs",
            "crates/larm-api/src/compatibility.rs",
            "crates/larm-api/src/openapi.rs",
            "apps/daemon/src/serve.rs",
            "docs/api/openapi.json",
            "tools/p090_evidence.py",
            "tools/p090_schema_evidence.py",
            "docs/implementation/STATE.md",
            "docs/reports/P090_compatibility-engine-hard-gates.md",
        ],
        blockers=blockers,
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
