#!/usr/bin/env python3
"""Evidencia de lifecycle de modelos y availability derivada (P042)."""

from __future__ import annotations

from evidence import Evidence, REPO, cargo_ok


def main() -> int:
    ev = Evidence("P042")
    domain = REPO / "crates/larm-domain/src/model.rs"
    lifecycle = REPO / "crates/larm-models/src/lifecycle.rs"
    database = REPO / "crates/larm-db/src/variants.rs"
    reconcile = REPO / "crates/larm-storage/src/reconcile.rs"
    maintenance = REPO / "crates/larm-models/tests/maintenance.rs"
    api_models = REPO / "crates/larm-api/tests/models.rs"
    registry = REPO / "crates/larm-models/tests/registry.rs"
    verification_worker = REPO / "crates/larm-models/tests/verification.rs"
    verification_db = REPO / "crates/larm-db/tests/model_verification.rs"
    verification_api = REPO / "crates/larm-api/src/models.rs"
    verification_store = REPO / "crates/larm-db/src/model_verification.rs"
    verification_domain = REPO / "crates/larm-domain/src/model_verification.rs"

    ev.run(
        "P042-LIFECYCLE-TRANSITIONS-STATUS-AND-SERIALIZATION",
        "UNIT",
        [
            "cargo",
            "test",
            "-p",
            "larm-domain",
            "--lib",
            "model::tests",
            "-j",
            "1",
            "--",
            "--test-threads=1",
        ],
        expected=(
            "La máquina rechaza transiciones no permitidas y reintentos sin nuevo intento; "
            "status respeta precedencia y los enums se serializan con valores contractuales."
        ),
        ok=cargo_ok,
        fixtures=[domain],
    )
    ev.run(
        "P042-AVAILABILITY-AND-BROKER-ELIGIBILITY",
        "UNIT",
        [
            "cargo",
            "test",
            "-p",
            "larm-models",
            "--lib",
            "lifecycle::tests",
            "-j",
            "1",
            "--",
            "--test-threads=1",
        ],
        expected=(
            "AVAILABLE exige artefactos requeridos verificados, bundle, gates PASS y load/health/smoke "
            "con la misma huella; BROKER_ELIGIBLE añade protocolo cualificado."
        ),
        ok=cargo_ok,
        fixtures=[lifecycle, domain],
    )
    ev.run(
        "P042-REAL-WINDOWS-OFFLINE-CORRUPT-RECOVERY-AND-UPDATE",
        "REAL",
        [
            "cargo",
            "test",
            "-p",
            "larm-models",
            "--test",
            "maintenance",
            "volumen_offline_y_vuelta_con_reverificacion",
            "-j",
            "1",
            "--",
            "--exact",
            "--nocapture",
            "--test-threads=1",
        ],
        expected=(
            "En Windows y SQLite reales, desconectar/reconectar conserva lifecycle y registros, "
            "marca OFFLINE, exige hashes reverificados, lleva corrupción a CORRUPT y tras reparar vuelve "
            "a VERIFIED/UNKNOWN; update_available conserva la versión instalada."
        ),
        ok=cargo_ok,
        fixtures=[domain, database, reconcile, maintenance, REPO / "migrations/0001_bootstrap.sql"],
    )
    ev.run(
        "P042-API-SELECTION-IDEMPOTENCY-AND-SCOPE",
        "CONTRACT",
        [
            "cargo",
            "test",
            "-p",
            "larm-api",
            "--test",
            "models",
            "seleccion_idempotente_y_detalle_de_variante",
            "-j",
            "1",
            "--",
            "--exact",
            "--test-threads=1",
        ],
        expected=(
            "La selección exige models:write, valida UUIDv7 y JSON estricto, requiere Idempotency-Key, "
            "repite la respuesta guardada y rechaza reutilizar la clave con otro cuerpo."
        ),
        ok=cargo_ok,
        fixtures=[api_models, REPO / "crates/larm-api/src/models.rs", REPO / "crates/larm-api/src/openapi.rs"],
    )
    ev.run(
        "P042-SQLITE-SELECTION-TRANSITION-AND-VARIANT-DETAIL",
        "CONTRACT",
        [
            "cargo",
            "test",
            "-p",
            "larm-models",
            "--test",
            "registry",
            "seleccionar_variante_y_consultar_detalle_completo",
            "-j",
            "1",
            "--",
            "--exact",
            "--test-threads=1",
        ],
        expected=(
            "SQLite registra DISCOVERED→SELECTED una sola vez con evento; el detalle contiene variante, "
            "revisión, distribución, modelo canónico y todos los arrays contractuales."
        ),
        ok=cargo_ok,
        fixtures=[registry, database, REPO / "crates/larm-domain/src/ports.rs", REPO / "migrations/0001_bootstrap.sql"],
    )
    ev.run(
        "P042-API-VERIFY-JOB",
        "CONTRACT",
        [
            "cargo",
            "test",
            "-p",
            "larm-api",
            "--test",
            "models",
            "verify_encola_con_probe_estricto_y_replay_idempotente",
            "-j",
            "1",
            "--",
            "--exact",
            "--test-threads=1",
        ],
        expected=(
            "POST verify exige models:write, UUIDv7, Probe estricto e Idempotency-Key; entrega 202, "
            "repite el recibo y detecta la reutilización de clave entre variantes distintas."
        ),
        ok=cargo_ok,
        fixtures=[api_models, verification_api, verification_domain],
    )
    ev.run(
        "P042-SQLITE-VERIFY-ATOMIC-JOURNAL-AND-ROLLBACK",
        "CONTRACT",
        [
            "cargo",
            "test",
            "-p",
            "larm-db",
            "--test",
            "model_verification",
            "job_deduplicates_and_commits_all_verification_state_atomically",
            "-j",
            "1",
            "--",
            "--exact",
            "--test-threads=1",
        ],
        expected=(
            "SQLite deduplica jobs activos por variante; actualiza artefacto, bundle, lifecycle y job en "
            "una transacción y revierte todo al forzar un fallo SQL."
        ),
        ok=cargo_ok,
        fixtures=[verification_store, verification_domain, REPO / "crates/larm-db/src/events.rs", REPO / "migrations/0001_bootstrap.sql"],
    )
    ev.run(
        "P042-REAL-VERIFY-WORKER-BYTES-AND-CORRUPTION",
        "REAL",
        [
            "cargo",
            "test",
            "-p",
            "larm-models",
            "--test",
            "verification",
            "worker_verifies_bytes_and_marks_a_later_mismatch_corrupt",
            "-j",
            "1",
            "--",
            "--exact",
            "--nocapture",
            "--test-threads=1",
        ],
        expected=(
            "Con ficheros locales y SQLite reales, el worker hashea el bundle, lo deja VERIFIED/UNKNOWN, "
            "y al modificar un byte marca réplica, bundle y variante CORRUPT."
        ),
        ok=cargo_ok,
        fixtures=[verification_worker, REPO / "crates/larm-models/src/verification.rs", verification_store, verification_domain, REPO / "migrations/0001_bootstrap.sql"],
    )
    ev.blocked(
        "P042-REAL-SAME-FINGERPRINT-LOAD-HEALTH-SMOKE-AND-PROTOCOL",
        "REAL",
        "Verificación de variante mediante runtime/backend/hardware reales y cualificación del protocolo Broker",
        "Evidence de gates, load, health y smoke vigente en una sola execution_fingerprint; elegibilidad Broker sólo con protocolo cualificado.",
        "El worker verifica bytes y bundle, pero no ejecuta runtime/backend/hardware ni obtiene gates, load, health, smoke "
        "y cualificación de protocolo en una execution_fingerprint común. La derivación pura se prueba por separado. "
        "Las dependencias P040, P041 y P016 tampoco están en REAL_PASS.",
    )

    ev.finish(
        changed_files=[
            "crates/larm-domain/src/model.rs",
            "crates/larm-domain/src/model_verification.rs",
            "crates/larm-domain/src/ports.rs",
            "crates/larm-db/src/variants.rs",
            "crates/larm-db/src/model_verification.rs",
            "crates/larm-db/tests/model_verification.rs",
            "crates/larm-api/src/models.rs",
            "crates/larm-api/src/openapi.rs",
            "apps/daemon/src/serve.rs",
            "crates/larm-models/src/lifecycle.rs",
            "crates/larm-models/src/lib.rs",
            "crates/larm-models/src/verification.rs",
            "crates/larm-models/tests/verification.rs",
            "crates/larm-models/tests/bundles.rs",
            "crates/larm-models/tests/import.rs",
            "crates/larm-models/tests/maintenance.rs",
            "crates/larm-models/tests/registry.rs",
            "crates/larm-api/tests/models.rs",
            "crates/larm-storage/src/reconcile.rs",
            "tools/check.ps1",
            "tools/p025_host_fixture.py",
            "tools/p042_evidence.py",
            "docs/implementation/STATE.md",
            "docs/reports/P042_lifecycle-de-modelos-y-availability-derivada.md",
        ],
        blockers=[
            "El worker realiza verificación de bytes y bundle; faltan runtime/backend/hardware y gates/load/health/smoke con una execution_fingerprint común, más cualificación de protocolo para Broker.",
            "P040, P041 y P016 siguen en BLOCKED_REAL; el contrato 00 impide completar P042 en REAL_PASS mientras cualquiera de esas dependencias siga bloqueada.",
            "package_copy detecta tres contratos locales distintos del paquete v3 (adapter-methods, api-operations y larm.schema); "
            "su reconciliación requiere una revisión de la fuente contractual canónica.",
        ],
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
