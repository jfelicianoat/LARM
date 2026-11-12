#!/usr/bin/env python3
"""Pruebas focales y evidencia de la infraestructura P093."""

import json

from evidence import Evidence, REPO, cargo_ok, exit_zero


def main() -> int:
    ev = Evidence("P093")
    temp = str(REPO / "target" / "larm-test-tmp")
    env = {"TEMP": temp, "TMP": temp, "TMPDIR": temp}

    ev.run(
        "P093-UNIT-domain",
        "UNIT",
        ["cargo", "test", "-p", "larm-domain", "--lib", "transformations::tests", "-j1", "--", "--test-threads=1"],
        "El contrato de transformation plan/job valida snapshots, hashes, parámetros, actor, correlación, timestamps y estados.",
        ok=cargo_ok,
        env=env,
    )
    ev.run(
        "P093-UNIT-transform-crate",
        "UNIT",
        ["cargo", "test", "-p", "larm-transform", "-j1", "--", "--test-threads=1"],
        "El crate de transformación conserva su fachada pública y su identificador estable.",
        ok=cargo_ok,
        env=env,
    )
    ev.run(
        "P093-INTEGRATION-api",
        "INTEGRATION",
        ["cargo", "test", "-p", "larm-api", "--test", "transformations", "-j1", "--", "--test-threads=1"],
        "Las rutas de plan, confirmación idempotente y listado respetan DTO cerrado, autorización, paginación y OpenAPI.",
        ok=cargo_ok,
        env=env,
    )
    ev.run(
        "P093-INTEGRATION-sql-recovery",
        "INTEGRATION",
        ["cargo", "test", "-p", "larm-db", "--lib", "transformations::tests", "-j1", "--", "--test-threads=1"],
        "SQLite reconcilia jobs interrumpidos como FAILED_RECOVERABLE con causa, journal, evento y outbox atómicos; no publica sin journal y aísla por actor.",
        ok=cargo_ok,
        env=env,
    )
    ev.run(
        "P093-INTEGRATION-migration",
        "INTEGRATION",
        ["cargo", "test", "-p", "larm-db", "--test", "integration", "h07_transformation_job_acepta_failed_recoverable", "-j1", "--", "--test-threads=1"],
        "La migración admite FAILED_RECOVERABLE y rechaza estados fuera del contrato.",
        ok=cargo_ok,
        env=env,
    )
    ev.run(
        "P093-INTEGRATION-worker-reference",
        "INTEGRATION",
        ["cargo", "test", "-p", "larm-daemon", "--test", "reference_adapters", "transformation_python_por_host_protegido_convierte_y_verifica_bundle_canonico", "-j1", "--", "--exact", "--test-threads=1"],
        "El host protegido ejecuta el adaptador de referencia, rechaza escritura sobre el input y red no autorizada, y verifica el bundle producido.",
        ok=cargo_ok,
        env=env,
        timeout=900,
    )
    ev.run(
        "P093-CONTRACT-daemon-build",
        "CONTRACT",
        ["cargo", "check", "-p", "larm-daemon", "-j1"],
        "El daemon compila con las rutas P093, recuperación de arranque y repositorio SQLite.",
        ok=exit_zero,
        env=env,
    )
    ev.run(
        "P093-CONTRACT-openapi",
        "CONTRACT",
        ["cargo", "run", "-p", "larm-daemon", "--", "openapi", "--out", "docs/api/openapi.json", "--check"],
        "OpenAPI documenta las tres operaciones P093 implementadas y coincide con el contrato generado.",
        ok=exit_zero,
        env=env,
    )
    ev.run(
        "P093-CONTRACT-architecture",
        "CONTRACT",
        ["powershell", "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", "tools/check_architecture.ps1"],
        "El grafo permitido incluye las dependencias P093 y conserva los límites del workspace y Tauri.",
        ok=exit_zero,
        env=env,
    )

    ev.blocked(
        "P093-REAL-preflight-worker-recovery",
        "REAL",
        "Ejecutar el flujo público del daemon en Windows con input de prueba, herramienta aislada y volúmenes controlados",
        "Comprobar preflight de espacio insuficiente, crash de herramienta, hash mismatch y recuperación H07; no publicar salidas parciales.",
        "La API pública queda cerrada con UPSTREAM_UNAVAILABLE porque el servicio SQLite todavía no conecta el worker, la reserva/preflight ni la ingestión y verificación de outputs. La prueba del adaptador de referencia valida su proceso aislado, pero no ejercita un job P093 del daemon.",
    )
    ev.blocked(
        "P093-REAL-restart-new-preflight",
        "REAL",
        "Reiniciar un job FAILED_RECOVERABLE desde el daemon en Windows",
        "Verificar el hash intacto del original, nuevo plan/preflight/reserva/guard, temporales propios y attempt incrementado con trazabilidad.",
        "El cierre H07 persiste FAILED_RECOVERABLE al recuperar interrupciones; aún no existe la operación de reinicio con nuevo preflight y fencing para ejecutar este oráculo end-to-end.",
    )

    blockers = [
        "La capa durable de H07 y los contratos/API están preparados, pero planificar y ejecutar fallan cerrado con UPSTREAM_UNAVAILABLE hasta conectar el worker, el preflight peak-disk, guards/reservas, staging y verificación/smoke de outputs.",
        "Falta implementar reinicio desde FAILED_RECOVERABLE bajo nuevo preflight, comprobar integridad del input, gestionar temporales con ownership y conservar el historial del intento anterior.",
        "Faltan los oráculos operativos P093 de disco insuficiente, crash de herramienta, hash mismatch y recovery/restart en un job del daemon sobre Windows.",
    ]
    changed_files = [
        "apps/daemon/Cargo.toml",
        "apps/daemon/src/serve.rs",
        "crates/larm-api/Cargo.toml",
        "crates/larm-api/src/lib.rs",
        "crates/larm-api/src/openapi.rs",
        "crates/larm-api/src/transformations.rs",
        "crates/larm-api/tests/transformations.rs",
        "crates/larm-db/Cargo.toml",
        "crates/larm-db/src/lib.rs",
        "crates/larm-db/src/migrate.rs",
        "crates/larm-db/src/transformations.rs",
        "crates/larm-db/tests/adapter_selection.rs",
        "crates/larm-db/tests/integration.rs",
        "crates/larm-transform/Cargo.toml",
        "crates/larm-transform/src/lib.rs",
        "crates/larm-domain/src/lib.rs",
        "crates/larm-domain/src/ports.rs",
        "crates/larm-domain/src/transformations.rs",
        "docs/api/openapi.json",
        "docs/implementation/STATE.md",
        "migrations/0008_p093_failed_recoverable.sql",
        "Cargo.lock",
        "tools/p093_evidence.py",
        "docs/reports/P093_transformation-pipeline-framework.md",
    ]
    status = ev.finish(changed_files=changed_files, blockers=blockers)
    evidence_path = ev.runs.parent / f"{ev.stem}.evidence.json"
    evidence = json.loads(evidence_path.read_text(encoding="utf-8"))
    passed = sum(test["result"] == "PASS" for test in evidence["tests"])
    failed = sum(test["result"] == "FAIL" for test in evidence["tests"])
    blocked = sum(test["result"] == "BLOCKED" for test in evidence["tests"])
    report = f"""# P093 — Transformation Pipeline framework

Fecha: {evidence['generated_at'][:10]}  
Estado: {status}

P093 añade el contrato de dominio para planes y trabajos de transformación, DTOs estrictos y rutas API para crear planes, confirmar ejecuciones con idempotencia y consultar jobs. SQLite persiste actor, correlación, request, plan sellado y hashes de entrada. Al arrancar, la recuperación convierte interrupciones RUNNING/PAUSING/VERIFYING en FAILED_RECOVERABLE junto con causa, journal, evento y outbox dentro de una transacción; no publica un output parcial. La migración 0008 habilita el estado y su índice de recuperación. OpenAPI publica las tres operaciones.

El grafo de crates publicado permanece intacto: DTOs e invariantes de transformation viven en `larm-domain`, su API usa el puerto de aplicación del dominio, y `larm-api`/`larm-db` no añaden dependencias internas nuevas. La revisión del código y las pruebas muestran un límite funcional pendiente: el repositorio todavía no conecta el worker al preflight de espacio pico, guards/reservas, staging ni verificación de artefactos. Por ello las escrituras devuelven UPSTREAM_UNAVAILABLE antes de crear filas. Tampoco está implementado el reinicio durable de FAILED_RECOVERABLE bajo un plan y preflight nuevos. El adaptador de referencia protegido sí ejecuta un proceso local, rechaza accesos fuera de permisos y verifica el bundle; esa prueba no sustituye el job público del daemon.

La evidencia focal contiene {passed} pruebas PASS, {failed} FAIL y {blocked} aceptaciones reales bloqueadas. Incluye tests del dominio, API, recuperación SQLite, migración, conformance del adaptador protegido, compilación del daemon y coherencia de OpenAPI. La puerta completa `tools/check.ps1 -Prompt P093 -Real` queda enlazada en [check_P093_real.json](check_P093_real.json), con sus logs y el resultado del workspace, escritorio y frontend.

Pendientes para cerrar la fase:

- Conectar worker y preflight peak-disk por volumen con input inmutable, herramienta y parámetros fijados, guards/reservas y provenance.
- Implementar verificación de hashes y smoke/load antes de registrar outputs; conservar el original por defecto.
- Implementar reinicio desde FAILED_RECOVERABLE con input íntegro, nuevo preflight, ownership de temporales, attempt incrementado y trazabilidad anterior.
- Ejecutar en Windows los oráculos reales de disco insuficiente, crash, hash mismatch y recovery/restart a través de jobs del daemon.
- Obtener `REAL_PASS` de P016, P040 y P051, requerido por el manifiesto y el contrato 00.

Las pruebas, sus salidas y hashes de entrada constan en [la evidencia contractual](P093_transformation-pipeline-framework.evidence.json). La matriz de dependencias impide completar P093 hasta cumplir esas puertas.
"""
    (evidence_path.parent / f"{ev.stem}.md").write_text(report, encoding="utf-8")
    return 1 if status == "BLOCKED" else 0


if __name__ == "__main__":
    raise SystemExit(main())
