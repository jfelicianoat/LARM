#!/usr/bin/env python3
"""Evidencia de registro y cualificación de inference endpoints (P036)."""

from __future__ import annotations

import sys

from evidence import Evidence, REPO, cargo_ok


def main() -> int:
    ev = Evidence("P036")
    protocol = REPO / "crates/larm-domain/src/protocol.rs"
    repository = REPO / "crates/larm-db/src/inference_endpoints.rs"
    service = REPO / "crates/larm-runtime/src/endpoints.rs"

    ev.run(
        "P036-PROTOCOL-SCHEMA-AND-UNKNOWN-PRESERVATION",
        "CONTRACT",
        [
            "cargo",
            "test",
            "-p",
            "larm-domain",
            "--lib",
            "protocol::tests",
            "-j",
            "1",
            "--",
            "--test-threads=1",
        ],
        expected=(
            "Perfiles versionados y múltiples por protocolo; unknown se conserva sin cualificarse; DTO estricto "
            "rechaza campos secretos, hashes y estados inválidos."
        ),
        ok=cargo_ok,
        fixtures=[protocol, REPO / "crates/larm-domain/src/bridge.rs"],
    )
    ev.run(
        "P036-SQLITE-MULTI-ENDPOINT-CAS-AND-OUTBOX",
        "INTEGRATION",
        [
            "cargo",
            "test",
            "-p",
            "larm-db",
            "--lib",
            "inference_endpoints::tests",
            "-j",
            "1",
            "--",
            "--test-threads=1",
        ],
        expected=(
            "SQLite conserva varios protocolos por instancia, expone sólo QUALIFIED, degrada por CAS y confirma "
            "endpoint/evento/outbox en una transacción; UNTESTED o fallo de outbox no deja estado parcial."
        ),
        ok=cargo_ok,
        fixtures=[repository, REPO / "migrations/0001_bootstrap.sql"],
    )
    ev.run(
        "P036-PROBE-UNKNOWN-TIMEOUT-IO-AND-RECOVERY",
        "INTEGRATION",
        [
            "cargo",
            "test",
            "-p",
            "larm-runtime",
            "--lib",
            "endpoints::tests",
            "-j",
            "1",
            "--",
            "--test-threads=1",
        ],
        expected=(
            "Probe habilita varios protocolos conocidos, preserva unknown sin publicarlo, convierte timeout y "
            "endpoint muerto a FAILED, permite recuperar con registro nuevo y no persiste ante error interno."
        ),
        ok=cargo_ok,
        fixtures=[service, protocol, repository],
    )
    ev.run(
        "P036-REAL-WINDOWS-LOOPBACK-AND-SQLITE",
        "REAL",
        [
            "cargo",
            "test",
            "-p",
            "larm-runtime",
            "--lib",
            "endpoints::tests::windows_real_loopback_health_protocol_and_sqlite_registration_then_dead_endpoint",
            "-j",
            "1",
            "--",
            "--exact",
            "--nocapture",
            "--test-threads=1",
        ],
        expected=(
            "En Windows, un servidor HTTP loopback responde health y handshake OpenAI; el endpoint queda "
            "QUALIFIED en SQLite y, al cerrar el proceso fixture, pasa a FAILED y desaparece de la vista Broker."
        ),
        ok=cargo_ok,
        fixtures=[service, repository, REPO / "migrations/0001_bootstrap.sql"],
    )
    ev.run(
        "P036-BROKER-READ-ONLY-API-CONTRACT",
        "CONTRACT",
        [sys.executable, "tools/p036_broker_contract_probe.py"],
        "P101 publica la operación GET /api/v1/broker/catalog con scope read y respuesta BrokerCatalog en el contrato canónico, el espejo LARM y OpenAPI.",
        fixtures=[
            REPO / "docs/contracts/api-operations.json",
            REPO / "docs/contracts/schemas/larm.schema.json",
            REPO / "docs/api/openapi.json",
            REPO / "tools/p036_broker_contract_probe.py",
        ],
    )

    ev.finish(
        changed_files=[
            "crates/larm-domain/src/protocol.rs",
            "crates/larm-domain/src/lib.rs",
            "crates/larm-domain/src/bridge.rs",
            "crates/larm-domain/src/runtime_ports.rs",
            "crates/larm-db/src/inference_endpoints.rs",
            "crates/larm-db/src/lib.rs",
            "crates/larm-runtime/src/endpoints.rs",
            "crates/larm-runtime/src/lib.rs",
            "tools/p036_evidence.py",
            "tools/p036_broker_contract_probe.py",
            "docs/implementation/STATE.md",
            "docs/reports/P036_inferenceendpoint-y-registro-de-protocolos.md",
        ],
        blockers=[
            "P036 depende de P033, que sigue BLOCKED_REAL; la operación de catálogo Broker está definida por P101 "
            "y su integración durable con instancias/leases sigue pendiente.",
        ],
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
