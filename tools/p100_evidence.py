#!/usr/bin/env python3
"""Pruebas focales y evidencia de la base durable de leases P100."""
from __future__ import annotations

import sys
from pathlib import Path

from evidence import Evidence, REPO, cargo_ok, exit_zero


def main() -> int:
    ev = Evidence("P100")
    temp = REPO / "target/larm-test-tmp"
    temp.mkdir(parents=True, exist_ok=True)
    temp_env = {"TEMP": str(temp), "TMP": str(temp), "TMPDIR": str(temp)}

    ev.run(
        "P100-ADAPTER-METHOD-SCHEMAS",
        "CONTRACT",
        [sys.executable, "tools/p026_schemas.py", "check"],
        "Los 80 schemas de métodos, el índice y el código Rust corresponden a los contratos canónicos.",
    )
    ev.run(
        "P100-ADAPTER-CAPABILITY-SCHEMAS",
        "CONTRACT",
        [sys.executable, "tools/p026_capability_schemas.py", "check"],
        "Los 36 schemas de capacidades, el índice y el código Rust corresponden a sus descriptores canónicos.",
    )
    ev.run(
        "P100-SQLITE-LEASE-REPOSITORY",
        "INTEGRATION",
        [
            "cargo", "test", "-p", "larm-db", "--lib",
            "runtime_leases::tests", "-j", "1", "--", "--test-threads=1",
        ],
        "Siete pruebas cubren replay/conflicto idempotente, renovaciones con CAS y replay, vencimiento con gracia, transiciones owner/revisión/generación, reinicio, secretos y rollback del outbox.",
        ok=cargo_ok,
        fixtures=[REPO / "crates/larm-db/src/runtime_leases.rs"],
        env=temp_env,
    )
    ev.run(
        "P100-STARTUP-LEASE-RECOVERY",
        "INTEGRATION",
        [
            "cargo", "test", "-p", "larm-runtime", "--lib",
            "lease_recovery::tests", "-j", "1", "--", "--test-threads=1",
        ],
        "Tres pruebas ejecutan recuperación contra SQLite y secretos de fixture: expiran solo generaciones viejas, revocan tokens, reintentan fallos y rechazan generation cero.",
        ok=cargo_ok,
        fixtures=[
            REPO / "crates/larm-runtime/src/lease_recovery.rs",
            REPO / "crates/larm-runtime/src/external_adoption.rs",
            REPO / "crates/larm-runtime/src/external_adoption/tests.rs",
            REPO / "apps/daemon/src/serve.rs",
        ],
        env=temp_env,
    )
    ev.run(
        "P100-BROKER-PERSISTED-LEASE-READS",
        "INTEGRATION",
        [
            "cargo", "test", "-p", "larm-runtime", "--lib",
            "leases_reconciliadas_siguen_disponibles_en_get_y_list", "-j", "1",
            "--", "--test-threads=1",
        ],
        "Tras reiniciar el puente, GET/LIST leen leases reconciliadas desde SQLite, recorren más de una página, respetan propietario y dan prioridad al estado vigente en memoria.",
        ok=cargo_ok,
        fixtures=[
            REPO / "crates/larm-runtime/src/external_adoption.rs",
            REPO / "crates/larm-runtime/src/external_adoption/tests.rs",
            REPO / "crates/larm-db/src/runtime_leases.rs",
            REPO / "apps/daemon/src/serve.rs",
        ],
        env=temp_env,
    )
    ev.run(
        "P100-DURABLE-LEASE-ADMISSION-JOB",
        "INTEGRATION",
        [
            "cargo", "test", "-p", "larm-runtime", "--lib",
            "external_adoption::tests::admision_durable_no_termina_sin_instancia_y_endpoint_cualificados",
            "-j", "1", "--", "--test-threads=1",
        ],
        "La solicitud y el job se guardan en SQLite; sin instancia y endpoint cualificados el job termina FAILED, SQLite registra ADMITTING→FAILED y el runtime no se carga ni se devuelve una lease ACTIVE ficticia.",
        ok=cargo_ok,
        fixtures=[
            REPO / "crates/larm-domain/src/ports.rs",
            REPO / "crates/larm-db/src/jobs.rs",
            REPO / "crates/larm-db/src/runtime_leases.rs",
            REPO / "crates/larm-runtime/src/external_adoption.rs",
            REPO / "crates/larm-runtime/src/external_adoption/tests.rs",
            REPO / "apps/daemon/src/serve.rs",
        ],
        env=temp_env,
    )
    ev.run(
        "P100-DURABLE-ADMISSION-ACTIVE-BRIDGE",
        "INTEGRATION",
        [
            "cargo", "test", "-p", "larm-runtime", "--lib",
            "external_adoption::tests::admision_durable_persiste_referencias_y_transiciona_lease_a_active",
            "-j", "1", "--", "--test-threads=1",
        ],
        "La admisión durable sólo pasa a ACTIVE después de obtener y guardar las referencias cualificadas; SQLite conserva los mismos IDs y generación.",
        ok=cargo_ok,
        fixtures=[
            REPO / "crates/larm-domain/src/runtime_ports.rs",
            REPO / "crates/larm-db/src/instances.rs",
            REPO / "crates/larm-runtime/src/endpoints.rs",
            REPO / "crates/larm-runtime/src/external_adoption.rs",
            REPO / "crates/larm-runtime/src/external_adoption/tests.rs",
        ],
        env=temp_env,
    )
    ev.run(
        "P100-QUALIFIED-EXTERNAL-REFERENCE-STORE",
        "INTEGRATION",
        [
            "cargo", "test", "-p", "larm-daemon",
            "adoption_references::tests::reference_store_requires_catalog_bindings_and_http_qualification",
            "-j", "1", "--", "--test-threads=1",
        ],
        "El daemon valida host, versión, instalación, backend, variante y perfil locales; una sonda HTTP P036 cualificada registra instancia y endpoint en una sola transacción.",
        ok=cargo_ok,
        fixtures=[
            REPO / "apps/daemon/src/adoption_references.rs",
            REPO / "crates/larm-db/src/instances.rs",
            REPO / "crates/larm-runtime/src/endpoints.rs",
        ],
        env=temp_env,
    )
    ev.run(
        "P100-DURABLE-LEASE-RENEW-RELEASE",
        "INTEGRATION",
        [
            "cargo", "test", "-p", "larm-runtime", "--lib",
            "external_adoption::tests::renovacion_y_liberacion_durables_actualizan_sqlite_y_revocan_secreto",
            "-j", "1", "--", "--test-threads=1",
        ],
        "El puente renueva con CAS e idempotencia durables, repite la respuesta original y libera en SQLite; la revocación borra el secreto local.",
        ok=cargo_ok,
        fixtures=[
            REPO / "crates/larm-domain/src/ports.rs",
            REPO / "crates/larm-db/src/runtime_leases.rs",
            REPO / "crates/larm-runtime/src/external_adoption.rs",
            REPO / "crates/larm-runtime/src/external_adoption/tests.rs",
        ],
        env=temp_env,
    )
    ev.run(
        "P100-LEASE-RELEASE-DURING-ADMISSION",
        "INTEGRATION",
        [
            "cargo", "test", "-p", "larm-runtime", "--lib",
            "external_adoption::tests::liberacion_durante_admission_se_rechaza_hasta_que_la_lease_este_activa",
            "-j", "1", "--", "--test-threads=1",
        ],
        "La API rechaza liberar durante ADMITTING sin revocar el token; tras completar la admisión, permite ACTIVE→RELEASED y revoca la credencial.",
        ok=cargo_ok,
        fixtures=[
            REPO / "crates/larm-runtime/src/external_adoption.rs",
            REPO / "crates/larm-runtime/src/external_adoption/tests.rs",
        ],
        env=temp_env,
    )
    ev.run(
        "P100-LEASE-RELEASE-DURING-EXPIRY-GRACE",
        "INTEGRATION",
        [
            "cargo", "test", "-p", "larm-runtime", "--lib",
            "external_adoption::tests::unrenewed_lease_expires_after_grace_and_loses_its_token",
            "-j", "1", "--", "--test-threads=1",
        ],
        "Durante EXPIRING, DELETE falla cerrado y conserva el token hasta que finaliza la gracia y se registra EXPIRED.",
        ok=cargo_ok,
        fixtures=[
            REPO / "crates/larm-runtime/src/external_adoption.rs",
            REPO / "crates/larm-runtime/src/external_adoption/tests.rs",
        ],
        env=temp_env,
    )
    ev.run(
        "P100-LARM-DB-ALL-TARGETS",
        "INTEGRATION",
        ["cargo", "test", "-p", "larm-db", "--all-targets", "-j", "1", "--", "--test-threads=1"],
        "Todas las suites y binarios de prueba de larm-db pasan con temporales dentro del workspace.",
        ok=cargo_ok,
        fixtures=[REPO / "crates/larm-db/src/runtime_leases.rs", REPO / "crates/larm-db/src/lib.rs"],
        timeout=900,
        env=temp_env,
    )
    ev.run(
        "P100-FORMAT",
        "CONTRACT",
        ["cargo", "fmt", "--all", "--check"],
        "El workspace Rust está formateado.",
    )
    ev.run(
        "P100-CLIPPY",
        "CONTRACT",
        ["cargo", "clippy", "--workspace", "--all-targets", "--", "-D", "warnings"],
        "Clippy del workspace pasa sin advertencias de código.",
        timeout=900,
    )
    ev.run(
        "P100-PACKAGE-COPIES",
        "CONTRACT",
        [sys.executable, "tools/package_copy.py", "check"],
        "Las copias contractuales coinciden con el paquete canónico.",
    )
    ev.blocked(
        "P100-REAL-LEASE-LIFECYCLE",
        "REAL",
        "tools/check.ps1 -Prompt P100 -Real",
        "Crear, renovar, expirar, recuperar tras reinicio y liberar una lease a través del daemon Windows, con SQL, reservas, credenciales y eventos/outbox consistentes.",
        "La ruta durable ya conecta el worker con un resolvedor que valida el catálogo local, cualifica el protocolo por HTTP y registra RuntimeInstance e InferenceEndpoint en SQLite antes de activar la lease. Esta prueba REAL sigue sin ejecutarse con el daemon Windows y un runtime externo instalado; P033/P035/P036/P042/P019/P090 aún no tienen REAL_PASS.",
    )
    ev.finish(
        changed_files=[
            "docs/api/ai-broker-contract.md",
            "crates/larm-domain/src/ports.rs",
            "crates/larm-domain/src/runtime_ports.rs",
            "crates/larm-domain/src/auth.rs",
            "crates/larm-db/src/jobs.rs",
            "crates/larm-db/src/lib.rs",
            "crates/larm-db/src/runtime_leases.rs",
            "crates/larm-db/src/instances.rs",
            "crates/larm-runtime/src/lib.rs",
            "crates/larm-runtime/src/endpoints.rs",
            "crates/larm-runtime/src/external_adoption.rs",
            "crates/larm-runtime/src/external_adoption/tests.rs",
            "crates/larm-runtime/src/lease_recovery.rs",
            "apps/daemon/src/adoption_references.rs",
            "apps/daemon/src/main.rs",
            "apps/daemon/Cargo.toml",
            "apps/daemon/src/serve.rs",
            "tools/p100_evidence.py",
            "docs/reports/P100_runtime-lease-service.md",
            "docs/reports/P036_inferenceendpoint-y-registro-de-protocolos.md",
            "docs/reports/P101_contrato-larm-con-ai-broker-y-localruntimeresolver.md",
            "docs/reports/P107_P100_revalidacion_focal_2026-10-24.md",
            "docs/implementation/STATE.md",
            "crates/larm-adapter-sdk/src/contracts/generated.rs",
            "crates/larm-adapter-sdk/src/capabilities/generated.rs",
        ] + [
            path.relative_to(REPO).as_posix()
            for folder in (
                REPO / "docs/adapters/method-schemas",
                REPO / "docs/adapters/capability-schemas",
            )
            for path in sorted(folder.glob("*.json"))
        ],
        blockers=[
            "La ruta de admisión con referencias cualificadas pasa pruebas locales; falta la aceptación REAL con el daemon Windows y un runtime externo instalado para comprobar el ciclo operativo completo.",
        ],
    )
    return 0 if all(test["result"] != "FAIL" for test in ev.tests) else 1


if __name__ == "__main__":
    raise SystemExit(main())
