#!/usr/bin/env python3
"""P024: HTTP real, aislamiento, aprobación, shutdown y muerte en PUBLISHING.

El worker copiado nunca se ejecuta. Raíz/principales propios; sólo se termina
el Popen de esta prueba. SQLite y hashes son oráculos independientes de HTTP.
"""
import concurrent.futures
import hashlib
import json
import os
import platform
import signal
import sqlite3
import subprocess
import sys
import time
import urllib.error
import urllib.request
import uuid
from datetime import datetime, timezone
from pathlib import Path

from p005_real import REPO, EXE, cli, clean_env, step
from p023_real import document, start, package_hash
from p024_update_real import package, sha

OPERATION = "P024_post__adapters_id_update"


def fetch(method, url, token=None, body=None, key=None):
    request = urllib.request.Request(url, method=method, data=json.dumps(body).encode() if body is not None else None)
    if token:
        request.add_header("authorization", f"Bearer {token}")
    if body is not None:
        request.add_header("content-type", "application/json")
    if key is not None:
        request.add_header("idempotency-key", key)
    try:
        with urllib.request.urlopen(request, timeout=120) as response:
            return response.status, json.loads(response.read())
    except urllib.error.HTTPError as error:
        return error.code, json.loads(error.read())


def snapshot(db):
    with sqlite3.connect(db) as conn:
        return {
            "installations": conn.execute("SELECT * FROM adapter_installation ORDER BY id").fetchall(),
            "selection": conn.execute("SELECT * FROM adapter_selection ORDER BY adapter_id").fetchall(),
            "jobs": conn.execute("SELECT * FROM operation_journal WHERE kind='adapter.update' ORDER BY id").fetchall(),
            "events": conn.execute("SELECT * FROM event WHERE event_type='adapter.updated' ORDER BY event_id").fetchall(),
            "outbox": conn.execute("SELECT o.event_id FROM event_outbox o JOIN event e ON e.event_id=o.event_id WHERE e.event_type='adapter.updated' ORDER BY o.event_id").fetchall(),
            "reservations": conn.execute("SELECT * FROM volume_reservation WHERE job_id IN (SELECT id FROM operation_journal WHERE kind='adapter.update') ORDER BY job_id").fetchall(),
            "locks": conn.execute("SELECT * FROM resource_lock WHERE owner_id IN (SELECT id FROM operation_journal WHERE kind='adapter.update') ORDER BY owner_id,resource_id").fetchall(),
            "replay": conn.execute("SELECT * FROM idempotency WHERE operation=? ORDER BY principal_id,key", (OPERATION,)).fetchall(),
        }


def job_record(db, identity):
    with sqlite3.connect(db) as conn:
        row = conn.execute("SELECT phase,payload FROM operation_journal WHERE id=? AND kind='adapter.update'", (identity,)).fetchone()
    return (row[0], json.loads(row[1])) if row else (None, None)


def wait(db, identity, predicate, timeout=90):
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        phase, record = job_record(db, identity)
        if record and predicate(phase, record):
            return phase, record
        time.sleep(0.01)
    raise AssertionError(f"no se observó la condición del job {identity}: {phase}, {record and record['step']}")


def main():
    out = REPO / "docs/reports/P024_runs"
    run = out / "api-real" / uuid.uuid4().hex[:12]
    for folder in ("rt", "models", "out", "packages"):
        (run / folder).mkdir(parents=True)
    data = run / "data"
    db = data / "state/larm.sqlite"
    worker = (REPO / "target/debug/larm-generic-adapter.exe").read_bytes()
    (run / "rt/fake_runtime.exe").write_bytes(worker)
    (run / "models/m.gguf").write_bytes(b"GGUF")
    logs, fixtures = [], []
    daemon = None
    names = ("writer", "reader", "other", "admin")
    tokens, principals = {}, {}

    def new_package(version, ballast=False, elevate=False):
        doc = document(run, version, secret=False)
        if elevate:
            doc["manifest"]["permissions"]["network_origins"] = ["https://fixture.example"]
        request, manifest = package(run / "packages" / f"{version}.zip", doc, worker, ballast)
        fixtures.append({"request": request.copy(), "manifest": manifest, "ballast": ballast})
        return request, manifest

    def submit(request, key, who="writer"):
        status, receipt = fetch("POST", f"{base}/adapters/{old['id']}/update", tokens[who], request, key)
        assert status == 202, (status, receipt)
        assert set(receipt) == {"job_id", "status_url", "state"} and receipt["state"] == "QUEUED"
        return receipt

    def finish(receipt, who="writer"):
        wait(db, receipt["job_id"], lambda p, r: p in ("COMPLETED", "FAILED"))
        status, job = fetch("GET", base.removesuffix("/api/v1") + receipt["status_url"], tokens[who])
        assert status == 200, (status, job)
        return job

    def graceful(process):
        os.kill(process.pid, signal.CTRL_BREAK_EVENT)
        process.wait(timeout=30)
        assert process.returncode == 0, process.returncode

    try:
        for name in names:
            enrolled = cli(data, "enroll-principal", "--name", f"p024-api-{name}", "--kind", "OPERATOR", "--scopes",
                           "admin" if name == "admin" else ("read" if name == "reader" else "read,adapters:write"))
            tokens[name], principals[name] = enrolled["token"], enrolled["principal"]["id"]
        daemon, base = start(data)
        status, old = fetch("POST", f"{base}/adapters/declarative", tokens["writer"], document(run, "1.0.0", secret=False), "source")
        assert status == 201, old
        old_dir = data / "adapters" / old["adapter_id"] / "1.0.0"
        original_hash = package_hash(old_dir)
        req2, manifest2 = new_package("2.0.0")
        before = snapshot(db)
        no_auth, _ = fetch("POST", f"{base}/adapters/{old['id']}/update", None, req2, "no-auth")
        read_only, _ = fetch("POST", f"{base}/adapters/{old['id']}/update", tokens["reader"], req2, "no-scope")
        malformed = req2.copy()
        malformed.pop("approval_id")
        bad_body, _ = fetch("POST", f"{base}/adapters/{old['id']}/update", tokens["writer"], malformed, "bad-body")
        no_key, _ = fetch("POST", f"{base}/adapters/{old['id']}/update", tokens["writer"], req2)
        step(logs, "guard-y-dto-sin-efectos", (no_auth, read_only, bad_body, no_key) == (401, 403, 400, 400) and snapshot(db) == before,
             "Bearer, scope, approval_id obligatorio e Idempotency-Key se comprueban antes de aceptar trabajos")
        with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
            receipts = list(pool.map(lambda _: submit(req2, "same-request"), range(2)))
        step(logs, "aceptacion-concurrente-202", receipts[0] == receipts[1], "dos requests reales simultáneos devuelven el mismo receipt original")
        receipt = receipts[0]
        job = finish(receipt)
        phase, persisted = job_record(db, receipt["job_id"])
        candidate = Path(persisted["plan"]["destination_path"])
        step(logs, "trabajo-y-paquete-http", phase == "COMPLETED" and job["state"] == "COMPLETED" and job["result"]["promoted"] is False
             and package_hash(candidate) == manifest2["package_sha256"] and persisted["actor_id"] == principals["writer"],
             "HTTP 202 ejecuta profiler, reserva y ZIP reales; JobState y SQLite coinciden; actor autenticado y hash independiente correctos")
        private = fetch("GET", base.removesuffix("/api/v1") + receipt["status_url"], tokens["reader"])[0]
        admin = fetch("GET", base.removesuffix("/api/v1") + receipt["status_url"], tokens["admin"])[0]
        step(logs, "propiedad-de-jobs-real", private == 404 and admin == 200,
             "otro principal con read no puede ver el job; admin sí, sin exponer paths del journal")
        before = snapshot(db)
        replay = submit(req2, "same-request")
        changed = req2 | {"expected_hash": "b" * 64}
        conflict, error = fetch("POST", f"{base}/adapters/{old['id']}/update", tokens["writer"], changed, "same-request")
        step(logs, "replay-original-y-conflicto", replay == receipt and conflict == 409 and error["code"] == "IDEMPOTENCY_CONFLICT"
             and snapshot(db) == before, "replay sigue QUEUED aunque el job terminó; otro cuerpo rechaza sin duplicados ni efectos")
        other_receipt = submit(req2, "same-request", "other")
        other_job = finish(other_receipt, "other")
        step(logs, "actores-no-comparten-idempotencia", other_receipt["job_id"] != receipt["job_id"] and other_job["state"] == "FAILED"
             and other_job["error"]["details"]["reason"] == "adapter_version_exists",
             "otro actor obtiene su propio trabajo; versión ya existente se rechaza y no sobrescribe archivos")
        req4, manifest4 = new_package("4.0.0", elevate=True)
        blocked = finish(submit(req4, "needs-approval"))
        step(logs, "elevacion-exige-aprobacion-http", blocked["state"] == "FAILED" and blocked["error"]["code"] == "APPROVAL_REQUIRED"
             and not (data / "adapters" / old["adapter_id"] / "4.0.0").exists(), "nuevos permisos no copian ni se aceptan implícitamente")
        delta = {k: sorted(set(manifest4["permissions"][k]) - set(old["manifest"]["permissions"][k])) for k in manifest4["permissions"]}
        value = {"operation": "adapter.permissions.elevate", "adapter_id": manifest4["id"], "version": manifest4["version"],
                 "package_sha256": manifest4["package_sha256"], "delta": delta}
        digest = hashlib.sha256(json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":")).encode()).hexdigest()
        approval = {"operation_digest": digest, "resource_ids": [f"adapter:{manifest4['id']}@{manifest4['version']}"],
                    "code_hash": manifest4["package_sha256"], "permissions": {}, "approved": True}
        status, granted = fetch("POST", f"{base}/approvals", tokens["admin"], approval, "grant-update")
        assert status == 201, granted
        req4["approval_id"] = granted["resource_id"]
        mismatch = finish(submit(req4, "wrong-actor"))
        step(logs, "aprobacion-no-transferible", mismatch["state"] == "FAILED" and mismatch["error"]["details"]["reason"] == "approval_actor_mismatch",
             "una aprobación del admin no autoriza el trabajo de otro principal")
        approved = finish(submit(req4, "approved-update", "admin"), "admin")
        with sqlite3.connect(db) as conn:
            approval_phase = conn.execute("SELECT phase FROM operation_journal WHERE id=?", (granted["resource_id"],)).fetchone()[0]
        step(logs, "aprobacion-exacta-consumida-http", approved["state"] == "COMPLETED" and approval_phase == "CONSUMED",
             "mismo actor + digest calculado independientemente + hash + recursos permiten actualizar; aprobación de un solo uso consumida")
        big, _ = new_package("3.0.0", ballast=True)
        graceful_receipt = submit(big, "graceful-copy")
        wait(db, graceful_receipt["job_id"], lambda p, r: r["step"] == "EXTRACTING" and
             any(p.stat().st_size > 0 for p in Path(r["plan"]["workspace_path"]).rglob("archive.zip") if p.exists()))
        graceful(daemon)
        daemon = None
        phase, closed = job_record(db, graceful_receipt["job_id"])
        after = snapshot(db)
        step(logs, "shutdown-http-espera-io", phase == "FAILED" and closed["error"]["details"]["reason"] == "operation_cancelled"
             and not after["reservations"] and not after["locks"] and not Path(closed["plan"]["workspace_path"]).exists(),
             "CTRL_BREAK durante copia cierra admisión, cancela, espera cleanup y sale con código 0; no libera mientras sigue IO")
        daemon, base = start(data)
        phase_job = finish(graceful_receipt)
        step(logs, "estado-preservado-tras-arranque", phase_job["state"] == "FAILED" and package_hash(old_dir) == original_hash,
             "nuevo servidor conserva resultado terminal del job y bytes del origen")
        publishing = submit(big, "kill-publishing")
        _, point = wait(db, publishing["job_id"], lambda p, r: r["step"] == "PUBLISHING" and Path(r["plan"]["destination_path"]).exists())
        locker = sqlite3.connect(db, timeout=0, isolation_level=None)
        try:
            locker.execute("BEGIN IMMEDIATE")  # Sin mutar datos: impide COMMIT hasta terminar el proceso de prueba.
            phase, point = job_record(db, publishing["job_id"])
            assert phase == "RUNNING" and point["step"] == "PUBLISHING", "el commit ya terminó antes de la inyección"
            daemon.kill()
            daemon.wait(timeout=15)
            daemon = None
        finally:
            locker.rollback()
            locker.close()
        after = snapshot(db)
        step(logs, "muerte-real-despues-de-publicar", job_record(db, publishing["job_id"])[0] == "RUNNING"
             and Path(point["plan"]["destination_path"]).exists() and len(after["reservations"]) == 1 and len(after["locks"]) == 2,
             "proceso HTTP terminado con paquete final publicado y commit bloqueado por otra conexión; journal y recursos conservados")
        daemon, base = start(data)
        recovered = finish(publishing)
        after = snapshot(db)
        step(logs, "arranque-reconcilia-publicacion", recovered["state"] == "COMPLETED" and recovered["job_id"] == publishing["job_id"]
             and recovered["result"]["promoted"] is False and not after["reservations"] and not after["locks"]
             and not Path(point["plan"]["workspace_path"]).exists() and not after["selection"] and package_hash(old_dir) == original_hash,
             "antes de escuchar, nuevo servidor verifica bytes y termina el mismo job; conserva default/origen y limpia recursos")
        replay = submit(big, "kill-publishing")
        step(logs, "replay-tras-recuperacion-http", replay == publishing and snapshot(db) == after,
             "el reinicio y recuperación no duplican la respuesta, instalación, journal ni eventos")
        print(f"PASS: {len(logs)} pasos HTTP reales, aprobación y muerte en PUBLISHING; P026 no ejecutado")
        return 0
    finally:
        if daemon is not None and daemon.poll() is None:
            try:
                graceful(daemon)
            except (subprocess.TimeoutExpired, AssertionError):
                daemon.kill()
                daemon.wait(timeout=15)
        for name in names:
            subprocess.run([str(EXE), "--data-root", str(data), "revoke-principal", "--name", f"p024-api-{name}"],
                           capture_output=True, env=clean_env(), timeout=30)
        record = {"scope": "HTTP, guard real, SQLite/profiler/NTFS; shutdown y muerte real en PUBLISHING; no conformidad P026",
                  "steps": logs, "fixtures": fixtures, "run": str(run.relative_to(REPO)), "host": platform.platform(),
                  "python": platform.python_version(), "daemon_sha256": sha(EXE), "generated_at": datetime.now(timezone.utc).isoformat()}
        (run / "steps.json").write_text(json.dumps(record, indent=2, ensure_ascii=False), encoding="utf-8")
        (out / "api-real.json").write_text(json.dumps(record, indent=2, ensure_ascii=False), encoding="utf-8")


if __name__ == "__main__":
    sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    raise SystemExit(main())
