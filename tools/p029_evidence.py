#!/usr/bin/env python3
"""P029 - Outbox duradero, despacho at-least-once y eventos de API."""
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from evidence import REPO, Evidence, cargo_ok, exit_zero  # noqa: E402

PY = sys.executable


def main() -> int:
    ev = Evidence("P029")
    db_fixtures = [REPO / "crates/larm-db/src/events.rs", REPO / "crates/larm-db/tests/events.rs"]
    dispatcher_fixtures = [REPO / "apps/daemon/src/serve.rs", REPO / "crates/larm-api/src/app.rs"]
    api_fixtures = [REPO / "crates/larm-api/src/app.rs", REPO / "crates/larm-api/tests/app.rs"]

    ev.run(
        "P029-ENV-VERSIONS",
        "CONTRACT",
        ["powershell", "-NoProfile", "-Command", "cargo --version; rustc --version"],
        "registrar las versiones exactas de Cargo y Rust usadas en esta corrida",
        ok=exit_zero,
    )
    ev.run(
        "P029-DB-OUTBOX-ATOMIC-AND-ACK",
        "INTEGRATION",
        ["cargo", "test", "-p", "larm-db", "--test", "events", "-j", "1", "--", "--test-threads=1"],
        "append transaccional de evento+outbox, rollback, ack idempotente, replay ordenado, cursor expirado/límite y poda que conserva outbox pendiente",
        ok=cargo_ok,
        fixtures=db_fixtures,
    )
    ev.run(
        "P029-DAEMON-DISPATCH-RETRY",
        "INTEGRATION",
        ["cargo", "test", "-p", "larm-daemon", "--bin", "larm-daemon", "event_feed_tests", "-j", "1", "--", "--test-threads=1"],
        "el dispatcher espera suscriptor, publica en orden, reintenta el mismo event_id tras fallo de ack, y SQLite proyecta el historial al envelope SSE",
        ok=cargo_ok,
        fixtures=dispatcher_fixtures,
    )
    ev.run(
        "P029-API-SSE-AUTH-QUEUE-SHUTDOWN",
        "INTEGRATION",
        ["cargo", "test", "-p", "larm-api", "--test", "app", "-j", "1", "--", "--test-threads=1"],
        "la suite API verifica auth read, Last-Event-ID, replay antes de eventos vivos, dedupe en la carrera replay/bus, 410 antes de SSE, cursor inválido, cola lenta y cierre ordenado",
        ok=cargo_ok,
        fixtures=api_fixtures,
    )
    ev.run(
        "P029-FMT",
        "UNIT",
        ["cargo", "fmt", "--all", "--", "--check"],
        "todo el workspace cumple rustfmt",
    )
    ev.run(
        "P029-CLIPPY",
        "UNIT",
        ["cargo", "clippy", "--workspace", "--all-targets", "-j", "1", "--", "-D", "warnings"],
        "Clippy del workspace y todos los targets sin avisos",
    )
    ev.run(
        "P029-WORKSPACE-TESTS",
        "UNIT",
        ["cargo", "test", "--workspace", "-j", "1"],
        "pruebas de todos los crates y adaptadores del workspace",
        ok=cargo_ok,
        timeout=3600,
    )
    ev.run(
        "P029-DAEMON-BUILD",
        "UNIT",
        ["cargo", "build", "-p", "larm-daemon", "-j", "1"],
        "binario servidor actualizado para ejecutar la prueba REAL de reinicio",
    )
    ev.run(
        "P029-REAL-DAEMON-RESTART-WRITE-AND-CONSUMER-REPLAY",
        "REAL",
        [PY, "tools/p029_real.py"],
        "reiniciar el proceso daemon con escrituras concurrentes en vuelo; replay SSE recupera todas las filas confirmadas una sola vez y la escritura live posterior no repite IDs",
        ok=exit_zero,
        timeout=600,
        fixtures=[REPO / "tools/p029_real.py", REPO / "crates/larm-db/src/events.rs"],
    )

    changed = [
        "apps/daemon/src/serve.rs",
        "crates/larm-api/src/app.rs",
        "crates/larm-api/tests/app.rs",
        "crates/larm-core/src/config/mod.rs",
        "crates/larm-db/src/events.rs",
        "crates/larm-db/tests/events.rs",
        "docs/implementation/STATE.md",
        "docs/reports/P029_outbox-y-eventos-durables-antes-de-watchers-y-procesos.md",
        "tools/p029_evidence.py",
        "tools/p029_real.py",
    ]
    ev.finish(changed, [
        "contrato 07 pide snapshot_url al expirar un cursor, pero el schema Error del paquete canónico prohíbe campos adicionales y api-operations.json no define un endpoint de snapshot; el contrato debe aclararse antes de añadir una URL no enrutable",
    ])
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
