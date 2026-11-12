#!/usr/bin/env python3
"""Evidencia focal del Source Watcher P050."""

import sys

from evidence import Evidence, REPO, cargo_ok


def main() -> int:
    ev = Evidence("P050")
    ev.run(
        "P050-SOURCE-PRODUCT-BUILD",
        "INTEGRATION",
        ["cargo", "build", "-p", "larm-source-p048", "-j1"],
        "Prepara el ejecutable original P048 que utiliza el ensayo nativo.",
        ok=lambda code, _out: code == 0,
        fixtures=[REPO / "adapters/source/p048/src/lib.rs"],
    )
    ev.run(
        "P050-NATIVE-SYNC-EXECUTION",
        "REAL",
        ["cargo", "test", "-p", "larm-daemon", "--test", "reference_adapters", "source_watch_tcp::source_watch_tcp_sin_capacidad_duplicada_con_actor_aprobado_y_reinicio", "-j1", "--", "--exact", "--test-threads=1"],
        "Scheduler real con P048 original cualificado: método source.sync sin capacidad redundante, actor de ejecución aprobado, snapshot/eventos/outbox y reinicio, sin descarga automática. Binding y due son preparación controlada; no acredita ingesta remota o GUI.",
        ok=cargo_ok,
        timeout=900,
        fixtures=[REPO / "apps/daemon/src/source_watches.rs", REPO / "apps/daemon/tests/support/source_watch_tcp.rs", REPO / "apps/daemon/tests/support/p048_download.rs", REPO / "apps/daemon/tests/support/source_http.rs", REPO / "tools/p048_http_fixture.py"],
    )
    ev.run(
        "P050-EVENT-CLASSIFIER",
        "UNIT",
        ["cargo", "test", "-p", "larm-events", "source_watch", "--", "--nocapture"],
        "La clasificación distingue modelo/revisión nueva y actualizaciones; la clave de dedupe es estable y opaca.",
        ok=cargo_ok,
        fixtures=[REPO / "crates/larm-events/src/source_watch.rs"],
    )
    ev.run(
        "P050-WORKER-CURSOR-RETRY",
        "UNIT",
        ["cargo", "test", "-p", "larm-models", "watcher", "--", "--nocapture"],
        "El worker conserva cursor/ETag, aplica jitter/backoff acotados, sanitiza errores y no avanza ante compatibilidad desconocida.",
        ok=cargo_ok,
        fixtures=[REPO / "crates/larm-models/src/watcher.rs"],
    )
    ev.run(
        "P050-SQLITE-ATOMIC-OUTBOX",
        "FAULT",
        ["cargo", "test", "-p", "larm-db", "--test", "source_watches", "--", "--nocapture"],
        "SQLite confirma cursor, notificación y outbox de forma atómica, deduplica revisiones y revierte detecciones inválidas.",
        ok=cargo_ok,
        fixtures=[REPO / "crates/larm-db/src/source_watches.rs", REPO / "crates/larm-db/tests/source_watches.rs"],
    )
    ev.run(
        "P050-API-CONTRACT",
        "INTEGRATION",
        ["cargo", "test", "-p", "larm-api", "--test", "source_watches", "--", "--nocapture"],
        "Las rutas aplican scopes, DTO estricto, replay idempotente, paginación y If-Match/CAS.",
        ok=cargo_ok,
        fixtures=[REPO / "crates/larm-api/src/source_watches.rs", REPO / "crates/larm-api/tests/source_watches.rs"],
    )
    ev.run(
        "P050-DAEMON-WIRING",
        "INTEGRATION",
        ["cargo", "check", "-p", "larm-daemon"],
        "El daemon compone el scheduler con SQLite y solo invoca source.sync mediante una instalación y cualificación vigentes, con permisos y timeout acotados.",
        ok=lambda code, _out: code == 0,
        fixtures=[REPO / "apps/daemon/src/source_watches.rs", REPO / "apps/daemon/src/serve.rs"],
    )
    ev.blocked(
        "P050-REAL-QUALIFIED-SOURCE",
        "REAL",
        "tools/check.ps1 -Prompt P050 -Real con fuente upstream cualificada y datos de prueba en Windows/PC IA",
        "Una instalación source.sync cualificada completa sincronización real, conserva cursor y ETag tras reinicio y verifica dedupe/notificaciones/outbox en SQLite.",
        "P044, P046, P047 y P029 aún no tienen REAL_PASS; no hay una instalación upstream cualificada admisible para este recorrido. Los fixtures locales solo acreditan CODE_PASS.",
    )
    status = ev.finish(
        changed_files=[
            "Cargo.lock",
            "apps/daemon/src/main.rs",
            "apps/daemon/src/serve.rs",
            "apps/daemon/src/source_watches.rs",
            "apps/daemon/tests/reference_adapters.rs",
            "apps/daemon/tests/support/source_watch_tcp.rs",
            "apps/daemon/tests/support/p048_download.rs",
            "apps/daemon/tests/support/source_http.rs",
            "apps/daemon/tests/support/download_restart.rs",
            "crates/larm-adapter-host/src/conformance/source_http.rs",
            "docs/reports/P127_sync_binding_2026-10-27.md",
            "crates/larm-api/src/lib.rs",
            "crates/larm-api/src/source_watches.rs",
            "crates/larm-api/tests/source_watches.rs",
            "crates/larm-db/Cargo.toml",
            "crates/larm-db/src/lib.rs",
            "crates/larm-db/src/source_watches.rs",
            "crates/larm-db/tests/source_watches.rs",
            "crates/larm-domain/src/lib.rs",
            "crates/larm-domain/Cargo.toml",
            "crates/larm-domain/src/ports.rs",
            "crates/larm-domain/src/watchers.rs",
            "crates/larm-events/Cargo.toml",
            "crates/larm-events/src/lib.rs",
            "crates/larm-events/src/source_watch.rs",
            "crates/larm-models/src/lib.rs",
            "crates/larm-models/src/watcher.rs",
            "docs/implementation/STATE.md",
            "docs/reports/P050_source-watcher-sincronizacion-incremental-y-notificaciones.md",
            "tools/p050_evidence.py",
        ],
        blockers=[
            "P044/P046/P047/P029 siguen sin REAL_PASS; P050 no puede completar su cualificación hasta validar esas dependencias.",
            "La evaluación de compatibilidad de P090 aún no está conectada al daemon; las vigilancias que exigen compatibilidad fallan cerradas y conservan el cursor.",
        ],
    )
    return 0 if status in ("CODE_PASS", "REAL_PASS", "BLOCKED_REAL") else 1


if __name__ == "__main__":
    sys.exit(main())
