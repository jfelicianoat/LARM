#!/usr/bin/env python3
"""Focused evidence for the LARM–AI Broker contract and current bridge slice."""
from __future__ import annotations

import sys

from evidence import Evidence, REPO, exit_zero


def cargo_ok(code: int, output: str) -> bool:
    if code != 0:
        return False
    results = [line for line in output.splitlines() if line.strip().startswith("test result:")]
    passed = sum(int(line.split(" passed")[0].rsplit(" ", 1)[-1]) for line in results if " passed" in line)
    failed = sum(int(line.split(" failed")[0].rsplit(" ", 1)[-1]) for line in results if " failed" in line)
    ignored = sum(int(line.split(" ignored")[0].rsplit(" ", 1)[-1]) for line in results if " ignored" in line)
    return passed > 0 and failed == 0 and ignored == 0


def main() -> int:
    ev = Evidence("P101")
    ev.run(
        "P101-API-ROUTES-AUTH-AND-LEASE-HEADERS",
        "INTEGRATION",
        ["cargo", "test", "-p", "larm-api", "broker::tests", "-j1", "--", "--test-threads=1"],
        "El router exige principal y scopes, valida body estricto, clave idempotente, If-Match, UUID y token de gateway por lease.",
        ok=cargo_ok,
        fixtures=[REPO / "crates/larm-api/src/broker.rs", REPO / "crates/larm-api/src/broker/tests.rs"],
    )
    ev.run(
        "P101-ADOPTION-CATALOG-LEASE-GATEWAY",
        "INTEGRATION",
        ["cargo", "test", "-p", "larm-runtime", "--lib", "external_adoption::tests", "-j1", "--", "--test-threads=1"],
        "El fixture HTTP de Ollama verifica catálogo aceptado, admisión, job, replay, renovación, expiry, liberación, memoria y gateway con modelo fijo.",
        ok=cargo_ok,
        fixtures=[REPO / "crates/larm-runtime/src/external_adoption.rs"],
        timeout=180,
    )
    ev.run(
        "P101-ENDPOINT-CREDENTIAL-PIPE",
        "INTEGRATION",
        ["cargo", "test", "-p", "larm-runtime", "--lib", "credentials_pipe::tests", "-j1", "--", "--test-threads=1"],
        "El pipe local vincula nonce, owner, scope, lease y generación; rechazos nunca contienen el token.",
        ok=cargo_ok,
        fixtures=[REPO / "crates/larm-runtime/src/credentials_pipe.rs"],
    )
    ev.run(
        "P101-DOMAIN-CONTRACT-DTOS",
        "UNIT",
        ["cargo", "test", "-p", "larm-domain", "--lib", "bridge::tests", "-j1", "--", "--test-threads=1"],
        "DTOs Broker mantienen schema estricto, errores, bytes acotados, UUIDv7 y cadence de renovación.",
        ok=cargo_ok,
        fixtures=[REPO / "crates/larm-domain/src/bridge.rs"],
    )
    ev.run(
        "P101-CATALOG-CONTRACT-AND-OPENAPI",
        "CONTRACT",
        [sys.executable, "tools/p036_broker_contract_probe.py"],
        "P101_get__broker_catalog coincide en paquete canónico, espejo, scope, BrokerCatalog y OpenAPI.",
        fixtures=[
            REPO / "tools/p036_broker_contract_probe.py",
            REPO / "docs/contracts/api-operations.json",
            REPO / "docs/api/openapi.json",
        ],
    )
    ev.run(
        "P101-OPENAPI-GENERATED-CHECK",
        "CONTRACT",
        ["cargo", "run", "-p", "larm-daemon", "--", "openapi", "--out", "docs/api/openapi.json", "--check"],
        "El OpenAPI generado permanece sincronizado con api-operations.json y sus schemas.",
        ok=exit_zero,
        timeout=180,
    )
    ev.run(
        "P101-FORMAT",
        "CONTRACT",
        ["cargo", "fmt", "--all", "--check"],
        "El workspace Rust está formateado.",
    )
    ev.run(
        "P101-CLIPPY",
        "CONTRACT",
        ["cargo", "clippy", "--workspace", "--all-targets", "--", "-D", "warnings"],
        "Clippy del workspace pasa sin warnings.",
        timeout=600,
    )
    ev.run(
        "P101-PACKAGE-COPIES",
        "CONTRACT",
        [sys.executable, "tools/package_copy.py", "check"],
        "Las copias normativas locales coinciden con el paquete 3.3.0.",
        ok=exit_zero,
    )
    ev.blocked(
        "P101-REAL-BROKER-RUNTIME-INTEGRATION",
        "REAL",
        "tools/check.ps1 -Prompt P101 -Real y consumo desde Broker con runtime cualificado",
        "Broker obtiene catálogo activo, crea y renueva lease, usa credencial de endpoint por canal local y libera tras respuesta/cancelación.",
        "P100 no tiene evidencia de servicio durable, P009 conserva una aceptación Windows pendiente y P069 no está cualificado. La adopción mantiene estado en memoria; la ruta interna del gateway tampoco figura aún en el contrato canónico.",
    )
    status = ev.finish(
        changed_files=[
            "crates/larm-api/src/broker.rs",
            "crates/larm-api/src/broker/tests.rs",
            "crates/larm-domain/src/bridge.rs",
            "crates/larm-runtime/src/credentials_pipe.rs",
            "crates/larm-runtime/src/external_adoption.rs",
            "docs/api/ai-broker-contract.md",
            "docs/implementation/STATE.md",
            "docs/reports/P101_contrato-larm-con-ai-broker-y-localruntimeresolver.md",
            "tools/p101_evidence.py",
        ],
        blockers=[
            "El slice de adopción es provisional: leases/jobs/idempotencia viven en memoria y deben pasar a SQLite/outbox con recuperación P100.",
            "La ruta interna del gateway de endpoint debe quedar definida en la fuente contractual propietaria antes de anunciarla al Broker.",
        ],
    )
    return 1 if status == "BLOCKED" else 0


if __name__ == "__main__":
    raise SystemExit(main())
