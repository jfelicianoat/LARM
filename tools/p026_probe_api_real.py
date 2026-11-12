#!/usr/bin/env python3
"""Daemon/TCP/guard/SQLite reales; instalación propia sin perfil registrado.

Prueba rechazo explícito sin cualificación ni ejecución del paquete propio.
Los cinco éxitos nativos se verifican por router autenticado en Rust, por separado.
"""
import hashlib
import json
import os
import platform
import signal
import sqlite3
import subprocess
import sys
import time
import uuid
from datetime import datetime, timezone

from p005_real import REPO, EXE, cli, clean_env, step
from p023_real import document, start, package_hash
from p024_api_real import fetch


def main():
    out = REPO / "docs/reports/P026_runs"
    run = out / "probe-api-real" / uuid.uuid4().hex[:12]
    for name in ("rt", "models", "out"):
        (run / name).mkdir(parents=True)
    data = run / "data"
    db = data / "state/larm.sqlite"
    (run / "rt/fake_runtime.exe").write_bytes((REPO / "target/debug/larm-generic-adapter.exe").read_bytes())
    (run / "models/m.gguf").write_bytes(b"GGUF")
    names = ("writer", "reader", "other", "admin")
    tokens, principals, steps = {}, {}, []
    daemon = None

    def snapshot():
        with sqlite3.connect(db) as conn:
            return {
                "installation": conn.execute("SELECT id,row_version,state,trust,metadata FROM adapter_installation ORDER BY id").fetchall(),
                "probe": conn.execute("SELECT id,phase,plan_hash,payload FROM operation_journal WHERE kind='adapter.probe' ORDER BY id").fetchall(),
                "conformance": conn.execute("SELECT id FROM operation_journal WHERE kind='adapter.conformance'").fetchall(),
                "events": conn.execute("SELECT event_id FROM event ORDER BY event_id").fetchall(),
                "outbox": conn.execute("SELECT event_id FROM event_outbox ORDER BY event_id").fetchall(),
                "replay": conn.execute("SELECT principal_id,operation,key,body_hash FROM idempotency ORDER BY principal_id,operation,key").fetchall(),
            }

    try:
        for name in names:
            enrolled = cli(data, "enroll-principal", "--name", f"p026-probe-{name}", "--kind", "OPERATOR", "--scopes",
                           "admin" if name == "admin" else ("read" if name == "reader" else "read,adapters:write"))
            tokens[name], principals[name] = enrolled["token"], enrolled["principal"]["id"]
        daemon, base = start(data)
        status, installation = fetch("POST", base + "/adapters/declarative", tokens["writer"], document(run, "1.0.0", secret=False), "package")
        assert status == 201
        package = data / "adapters" / installation["adapter_id"] / "1.0.0"
        original = package_hash(package)
        path = f"{base}/adapters/{installation['id']}/probe"
        request = {"resource_id": None, "profile_id": None}
        before = snapshot()
        for name, token, expected in (("auth", None, 401), ("scope", tokens["reader"], 403)):
            status, _ = fetch("POST", path, token, request, "auth")
            step(steps, name, status == expected and snapshot() == before, "rechazo antes de aceptar trabajo")
        for name, body, expected in (
            ("missing-nullable", {"resource_id": None}, 400),
            ("external-pass", request | {"passed": True}, 400),
            ("unknown-profile", request | {"profile_id": "unregistered"}, 422),
        ):
            status, _ = fetch("POST", path, tokens["writer"], body, name)
            step(steps, name, status == expected and snapshot() == before, "sin journal/evento/outbox/replay nuevos")
        status, accepted = fetch("POST", path, tokens["writer"], request, "probe")
        step(steps, "accepted-202", status == 202 and set(accepted) == {"job_id", "status_url", "state"} and accepted["state"] == "QUEUED", "JobReceipt exacto, sin aceptar veredicto")
        status_url = base.removesuffix("/api/v1") + accepted["status_url"]
        deadline = time.monotonic() + 30
        while True:
            status, state = fetch("GET", status_url, tokens["writer"])
            assert status == 200
            if state["state"] in ("FAILED", "COMPLETED"):
                break
            assert time.monotonic() < deadline, "sondeo no terminó"
            time.sleep(0.02)
        step(steps, "no-suite-no-false-validation", state["state"] == "FAILED" and state["error"]["code"] == "UNSUPPORTED_CAPABILITY" and state["result"] is None, "catálogo de producto vacío se expresa como fallo; no se ejecuta el paquete")
        after = snapshot()
        step(steps, "installation-unchanged", after["installation"] == before["installation"] and not after["conformance"] and package_hash(package) == original, "INSTALLED/UNTRUSTED, sin recibo de conformidad ni bytes alterados")
        step(steps, "journal-outbox", len(after["probe"]) == 1 and after["probe"][0][1] == "FAILED" and after["events"] == after["outbox"], "transiciones durables y outbox correspondiente")
        status, replay = fetch("POST", path, tokens["writer"], request, "probe")
        step(steps, "same-key-original-receipt", status == 202 and replay == accepted and snapshot() == after, "replay no repite trabajo ni eventos")
        status, _ = fetch("POST", path, tokens["writer"], request | {"resource_id": installation["id"]}, "probe")
        step(steps, "body-conflict", status == 409 and snapshot() == after, "mismo key con otros bytes/datos no reutiliza resultado")
        step(steps, "owner-private", fetch("GET", status_url, tokens["other"])[0] == 404 and fetch("GET", status_url, tokens["admin"])[0] == 200, "propietario/admin, sin fuga por ID")
        os.kill(daemon.pid, signal.CTRL_BREAK_EVENT)
        daemon.wait(timeout=30)
        step(steps, "graceful-shutdown", daemon.returncode == 0, "cierre de admisión y drenaje de servicios")
        daemon = None
        daemon, base = start(data)
        status, reopened = fetch("GET", base.removesuffix("/api/v1") + accepted["status_url"], tokens["writer"])
        step(steps, "reopen-terminal-job", status == 200 and reopened == state and snapshot() == after, "nuevo proceso conserva resultado y no lo reejecuta")
        print(f"PASS: {len(steps)} checks daemon/TCP/auth/SQLite; rechazo de suite ausente, sin cualificación funcional")
        return 0
    finally:
        if daemon is not None and daemon.poll() is None:
            try:
                os.kill(daemon.pid, signal.CTRL_BREAK_EVENT)
                daemon.wait(timeout=30)
            except subprocess.TimeoutExpired:
                daemon.kill()
                daemon.wait(timeout=15)
        for name in names:
            subprocess.run([str(EXE), "--data-root", str(data), "revoke-principal", "--name", f"p026-probe-{name}"], capture_output=True, env=clean_env(), timeout=30)
        record = {"scope": "real daemon/TCP/guard/Windows credentials/SQLite; no registered profile for this fixture: explicit failure, not adapter qualification",
                  "status": "PASS" if len(steps) == 14 and all(s["result"] == "PASS" for s in steps) else "FAIL",
                  "steps": steps, "run": run.relative_to(REPO).as_posix(), "daemon_sha256": hashlib.sha256(EXE.read_bytes()).hexdigest(),
                  "host": platform.platform(), "python": platform.python_version(), "generated_at": datetime.now(timezone.utc).isoformat()}
        (run / "steps.json").write_text(json.dumps(record, ensure_ascii=False, indent=2), encoding="utf-8")
        (out / "probe-api-real.json").write_text(json.dumps(record, ensure_ascii=False, indent=2), encoding="utf-8")


if __name__ == "__main__":
    sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    raise SystemExit(main())
