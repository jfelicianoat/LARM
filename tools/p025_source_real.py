#!/usr/bin/env python3
"""Source Python autónomo: catálogo, RPC, SQLite y fixture HTTP reales."""
import copy
import base64
import hashlib
import http.server
import importlib.util
import json
import platform
import socket
import sqlite3
import subprocess
import sys
import threading
import time
import urllib.parse
import uuid
import zipfile
from datetime import datetime, timezone
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(REPO / "tools/adapter-conformance"))
from session import Session  # noqa: E402
from runner import common, executable  # noqa: E402


class HTTP(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_args):
        pass

    def do_GET(self):
        self.server.requests.append({"path": self.path, "range": self.headers.get("Range"), "if_range": self.headers.get("If-Range"), "authorization": self.headers.get("Authorization")})
        probe = self.path == "/probe"
        mode = self.server.probe_mode if probe else self.server.mode
        if mode in ("auth", "rate"):
            self.send_response(401 if mode == "auth" else 429)
            if mode == "rate":
                self.send_header("Retry-After", "900")
            self.send_header("Content-Length", "0")
            self.end_headers()
            return
        if probe:
            self.send_response(200)
            self.send_header("Content-Length", "0")
            self.end_headers()
            return
        index = int(self.path.rsplit("/", 1)[1])
        data = self.server.artifacts[index]["bytes"]
        offset = int(self.headers["Range"].removeprefix("bytes=").removesuffix("-")) if self.headers.get("Range") else 0
        self.send_response(206 if offset else 200)
        self.send_header("ETag", '"' + hashlib.sha256(data).hexdigest() + '"')
        if offset:
            self.send_header("Content-Range", f"bytes {offset}-{len(data)-1}/{len(data)}")
        self.send_header("Content-Length", str(len(data) - offset))
        self.end_headers()
        payload = data[offset:]
        if mode == "cut":
            payload = payload[:128 << 10]
        try:
            for at in range(0, len(payload), 16 << 10):
                self.wfile.write(payload[at:at + (16 << 10)])
                self.wfile.flush()
                if mode == "slow":
                    time.sleep(0.02)
            if mode == "cut":
                self.connection.shutdown(socket.SHUT_RDWR)
                self.connection.close()
        except OSError:
            pass


def factory():
    spec = importlib.util.spec_from_file_location("reference_source_package", REPO / "adapters/source/reference/package.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def main():
    run = REPO / "docs/reports/P025_runs/source-real" / uuid.uuid4().hex[:12]
    run.mkdir(parents=True)
    artifacts = [{"name": "data.bin", "bytes": bytes([index]) * (2 << 20), "capability": f"vendor.reference.case{index}"} for index in range(6)]
    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), HTTP)
    server.artifacts, server.mode, server.probe_mode, server.requests = artifacts, "normal", "normal", []
    base = f"http://127.0.0.1:{server.server_port}"
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    session, steps, success, manifest = None, [], False, None
    package, work = run / "package", run / "work"
    db = work / "reference-source.sqlite"

    def step(name, condition, detail):
        assert condition, name
        steps.append({"id": name, "result": "PASS", "detail": detail})
        print(f"PASS {name}", flush=True)

    def connect():
        value = Session(executable(package, manifest), manifest, cwd=work, docs=package / "docs")
        value.handshake()
        return value

    def rows():
        with sqlite3.connect(db) as connection:
            return {table: connection.execute(f"SELECT * FROM {table} ORDER BY rowid").fetchall() for table in ("ref_job", "ref_plan", "ref_journal", "ref_event", "ref_outbox", "ref_reservation", "ref_lock", "ref_checkpoint")}

    def error(method, params, reason=None, validate=True):
        response = session.call(method, params, validate=validate, timeout=30)
        assert "error" in response, method
        if reason:
            assert response["error"].get("data", {}).get("details", {}).get("reason") == reason, response["error"]
        return response["error"]

    def plan(index, **over):
        params = {"variant_id": catalog["entries"][index]["model"]["id"], "storage_location_id": config["storage_location_id"], "source_id": config["source_id"], "hard_budget_bytes": None, "priority": "HIGH"}
        return session.result("source.download_plan", params | over)

    def execute(value):
        return {"plan_id": value["plan_id"], "plan_hash": value["plan_hash"], "approval_id": None}

    try:
        builder = factory()
        print("P025 source: preparando Python y dependencias locales fijadas", flush=True)
        manifest, config, catalog = builder.create(REPO, package, work, base, artifacts)
        session = connect()
        step("autonomous-python-worker", session.process.poll() is None and session.result("core.heartbeat", {})["alive"], "intérprete y dependencias bajo paquete; -I/-B, sin importar Python global ni escribir bytecode")
        search = {"source_id": config["source_id"], "q": "Reference", "capabilities": [], "cursor": None, "limit": 1}
        page = session.result("source.search", search)
        models = page["items"][:]
        cursor = page["next_cursor"]
        while cursor:
            page = session.result("source.search", search | {"cursor": cursor})
            models.extend(page["items"])
            cursor = page["next_cursor"]
        step("catalog-pagination", len(models) == 6 and len({m["id"] for m in models}) == 6, "seis páginas reales sin duplicados; IDs/snapshot/cursor del catálogo local")
        meta = models[0]["metadata"]["source_v1"]
        step("unknown-metadata-and-null", meta["original"]["tokenizer"] == {"unknown": [1, None]} and meta["original"]["api_token"] == "[REDACTED]" and meta["normalized"]["architecture"] is None and models[0]["availability"] == "UNKNOWN", "datos desconocidos y null preservados; secreto fixture redactado; no inventa elegibilidad")
        selected = [artifacts[0]["capability"], artifacts[1]["capability"]]
        filtered = session.result("source.search", search | {"limit": 200, "capabilities": selected})
        step("capabilities-or-and-query", len(filtered["items"]) == 2 and not session.result("source.search", search | {"q": "absent", "capabilities": selected})["items"], "OR en capacidades, AND con texto, sin convertir filtros de Core en soporte upstream")
        first = session.result("source.search", search)
        error("source.search", search | {"cursor": first["next_cursor"], "q": "other"}, "source_cursor_invalid")
        error("source.search", search | {"source_id": str(uuid.uuid7())}, "source_id_mismatch")
        invalid = error("source.search", search | {"architectures": ["fake"]}, validate=False)
        step("invalid-query-no-invented-support", invalid["code"] == -32602, "cursor ligado al filtro/fuente; un campo de Core extra se rechaza antes del callback")
        catalog_path = work / "catalog/catalog.json"
        original_catalog = catalog_path.read_bytes()
        stale_plan = plan(0)
        changed = copy.deepcopy(catalog)
        changed["entries"][0]["original"]["new_vendor_field"]["nested"] = False
        catalog_path.write_text(json.dumps(changed), encoding="utf-8")
        error("source.search", search | {"cursor": first["next_cursor"]}, "source_cursor_invalid")
        before = rows()
        error("source.download", execute(stale_plan), "source_snapshot_changed")
        step("changed-snapshot-cursor-and-plan", rows() == before, "catálogo modificado invalida el cursor y el plan previos sin admitir trabajo")
        catalog_path.write_bytes(original_catalog)
        ref = {"source_id": config["source_id"], "upstream_namespace": "reference", "upstream_model_id": "fixture-0"}
        inspection = session.result("source.get_model", ref)
        revisions = session.result("source.list_revisions", ref)
        revision = ref | {"revision_key": revisions["items"][0]["revision_key"]}
        listed = session.result("source.get_manifest", revision)
        estimate = session.result("source.estimate_size", revision)
        step("inspection-revision-manifest-estimate", inspection["artifacts"] == listed["artifacts"] and inspection["limits"]["vendor_unknown"]["context"] is None and estimate["download_bytes"] == str(len(artifacts[0]["bytes"])) and not estimate["per_volume"][0]["eligible"], "tamaños/hash/revisión fijados; GUID/free del volumen observados; sin acreditar admisión Core")
        sync = session.result("source.sync", {"source_id": config["source_id"], "cursor": None, "etag": None, "query": {}})
        unchanged = session.result("source.sync", {"source_id": config["source_id"], "cursor": None, "etag": sync["etag"], "query": {}})
        step("sync-etag", len(sync["models"]) == 6 and not unchanged["changed"] and not unchanged["models"], "ETag del catálogo real; no-change no inventa modelos")
        probe = {"source_id": config["source_id"], "auth_mode": "NONE", "account_id": None, "credential_reference_id": None}
        server.probe_mode = "auth"
        auth = session.result("source.auth_probe", probe)
        denied = plan(0)
        before = rows()
        error("source.download", execute(denied), "source_auth_required")
        step("auth-required-before-admission", auth["auth_required"] and not auth["authenticated"] and rows() == before, "401 HTTP real; sin job/journal/evento/reserva ni bytes para una descarga no autenticada")
        server.probe_mode = "normal"
        error("source.auth_probe", probe | {
            "auth_mode": "BASIC",
            "account_id": str(uuid.uuid7()),
            "credential_reference_id": str(uuid.uuid7()),
        }, "reference_credentials_not_resolved")
        small = plan(0, hard_budget_bytes="1")
        before = rows()
        error("source.download", execute(small), "reference_budget_exceeded")
        step("budget-before-admission", rows() == before, "presupuesto insuficiente rechaza sin admitir un job")
        expired = plan(0)
        expired["expires_at"] = "2000-01-01T00:00:00.000Z"
        expired["plan_hash"] = hashlib.sha256(json.dumps({k: v for k, v in expired.items() if k != "plan_hash"}, sort_keys=True, ensure_ascii=False, separators=(",", ":")).encode()).hexdigest()
        with sqlite3.connect(db) as connection:
            connection.execute("UPDATE ref_plan SET hash=?,document=? WHERE id=?", (expired["plan_hash"], json.dumps(expired), expired["plan_id"]))
        before = rows()
        error("source.download", execute(expired), "plan_expired")
        step("expired-plan-no-admission", rows() == before, "plan sellado de fixture con fecha pasada rechaza sin tocar estado ni bytes")
        download = plan(0)
        source_spec = download["steps"][0]["request"]["source_download_v1"]
        chunk_request = {
            "source_id": config["source_id"],
            "job_id": str(uuid.uuid7()),
            "plan_id": download["plan_id"],
            "plan_hash": download["plan_hash"],
            "approval_id": None,
            "revision": source_spec["revision"],
            "relative_path": source_spec["artifacts"][0]["relative_path"],
            "offset": "0",
            "max_bytes": str(16 << 10),
            "validator": None,
        }
        chunk = session.result("source.download_chunk", chunk_request)
        first = artifacts[0]["bytes"][: 16 << 10]
        step(
            "download-manager-bounded-chunk",
            chunk["offset"] == "0"
            and base64.b64decode(chunk["data_base64"], validate=True) == first
            and chunk["eof"] is False
            and chunk["total_bytes"] == str(len(artifacts[0]["bytes"]))
            and chunk["validator"] == {"kind": "STRONG_ETAG", "value": '"' + hashlib.sha256(artifacts[0]["bytes"]).hexdigest() + '"'},
            "source.download_chunk devuelve bytes reales acotados, tamaño y ETag fuerte; el Core conserva checkpoint y verificación final",
        )
        chunk_request.update(
            {
                "offset": str(len(first)),
                "validator": chunk["validator"],
            }
        )
        resumed_chunk = session.result("source.download_chunk", chunk_request)
        step(
            "download-manager-range-resume",
            resumed_chunk["offset"] == str(len(first))
            and base64.b64decode(resumed_chunk["data_base64"], validate=True)
            == artifacts[0]["bytes"][len(first) : 2 * (16 << 10)]
            and resumed_chunk["validator"] == chunk["validator"],
            "el worker valida If-Range y continúa desde el offset solicitado con el mismo ETag",
        )
        unsigned = {key: value for key, value in download.items() if key != "plan_hash"}
        calculated = hashlib.sha256(json.dumps(unsigned, sort_keys=True, ensure_ascii=False, separators=(",", ":")).encode()).hexdigest()
        before = rows()
        error("source.download", execute(download) | {"plan_hash": "f" * 64}, "plan_hash_mismatch")
        step("plan-binding-and-hash", calculated == download["plan_hash"] and rows() == before and download["steps"][0]["request"]["source_download_v1"]["priority"] == "HIGH", "hash independiente cubre manifest, destino, prioridad, auth y presupuesto")
        receipt = session.result("source.download", execute(download), timeout=30)
        status_path = Path(urllib.parse.unquote(urllib.parse.urlsplit(receipt["status_url"]).path).lstrip("/"))
        state = json.loads(status_path.read_text(encoding="utf-8"))
        session.contracts.validate("larm.schema.json#/$defs/JobState", state)
        target = work / "storage" / listed["manifest_hash"] / "data.bin"
        step("actual-rpc-download-and-worker-job", receipt["state"] == state["state"] == "COMPLETED" and target.read_bytes() == artifacts[0]["bytes"], "RPC source.download copia bytes y devuelve JobState durable realmente legible; no inventa /api/jobs")
        with sqlite3.connect(db) as connection:
            event_versions = connection.execute("SELECT version FROM ref_event WHERE job_id=? ORDER BY id", (receipt["job_id"],)).fetchall()
            outbox = connection.execute("SELECT count(*) FROM ref_outbox").fetchone()[0]
            held = connection.execute("SELECT (SELECT count(*) FROM ref_lock)+(SELECT count(*) FROM ref_reservation)").fetchone()[0]
        step("private-state-journal-outbox-reservation", event_versions == [(1,), (2,), (3,)] and outbox == 3 and held == 0, "ledger privado transaccional: RUNNING/VERIFYING/COMPLETED, outbox y liberación; no base ni reservas globales Core")
        before = rows()
        mtime = target.stat().st_mtime_ns
        step("execute-plan-replay", session.result("source.download", execute(download)) == receipt and rows() == before and target.stat().st_mtime_ns == mtime, "mismo plan: mismo recibo original y ningún nuevo job, evento o cambio de bytes")
        reused = session.result("source.estimate_size", revision)
        target.write_bytes(b"x" * len(artifacts[0]["bytes"]))
        corrupt = session.result("source.estimate_size", revision)
        target.write_bytes(artifacts[0]["bytes"])
        step("estimate-rehashes-existing-bytes", reused["existing_bytes"] == str(len(artifacts[0]["bytes"])) and reused["peak_extra_bytes"] == "0" and corrupt["existing_bytes"] == "0", "sólo descuenta bytes con tamaño y SHA reales; mismo tamaño corrupto no es existente verificado")
        session.close()
        session = connect()
        step("completed-job-survives-restart", session.result("source.download", execute(download)) == receipt, "otro worker recupera plan/resultado desde SQLite y sirve su estado real")
        server.mode = "cut"
        interrupted = plan(1)
        error("source.download", execute(interrupted), "source_network_interrupted")
        with sqlite3.connect(db) as connection:
            failed_id, attempt, phase = connection.execute("SELECT id,attempt,phase FROM ref_job WHERE phase='FAILED'").fetchone()
            offset = connection.execute("SELECT offset FROM ref_checkpoint WHERE job_id=?", (failed_id,)).fetchone()[0]
        partial = work / "staging" / failed_id / "data.bin.partial"
        step("interruption-durable-offset", phase == "FAILED" and 0 < offset == partial.stat().st_size < len(artifacts[1]["bytes"]), "offset persistido después de fsync de bytes reales, error conservado y partial sin publicar")
        server.mode = "normal"
        resumed = session.result("source.download", execute(plan(1)), timeout=30)
        ranged = [r for r in server.requests if r["path"] == "/artifact/1" and r["range"]]
        with sqlite3.connect(db) as connection:
            attempt = connection.execute("SELECT attempt FROM ref_job WHERE id=?", (failed_id,)).fetchone()[0]
        step("retry-new-plan-resumes-same-job", resumed["job_id"] == failed_id and attempt == 2 and ranged[-1]["range"] == f"bytes={offset}-", "nuevo preflight/plan incrementa intento; Range/If-Range real y mismo job; no reejecuta un plan fallido")
        server.mode = "slow"
        pending = plan(2)
        request_id = session.send("source.download", execute(pending))
        session.wait_progress(request_id)
        cancel = session.result("core.cancel", {"request_id": request_id})
        reply = session.response(request_id)
        with sqlite3.connect(db) as connection:
            cancelled_id = connection.execute("SELECT id FROM ref_job WHERE phase='CANCELLED'").fetchone()[0]
        step("cancel-removes-only-owned-partials", cancel["cancelled"] and reply["error"]["data"]["details"]["reason"] == "operation_cancelled" and not list((work / "staging" / cancelled_id).rglob("*.partial")), "cancelación cooperativa, libera recursos y elimina sólo temporales enumerados del job")
        server.mode = "normal"
        failed_commit = plan(3)
        with sqlite3.connect(db) as connection:
            connection.execute("CREATE TRIGGER p025_fail_outbox BEFORE INSERT ON ref_outbox BEGIN SELECT RAISE(ABORT,'fixture fail'); END")
        before = rows()
        error("source.download", execute(failed_commit), "reference_io_failed")
        step("accept-outbox-failure-atomic", rows() == before, "trigger SQLite real revierte aceptación, job/journal/outbox/locks/reserva y binding del plan juntos")
        with sqlite3.connect(db) as connection:
            connection.execute("DROP TRIGGER p025_fail_outbox")
            connection.execute("CREATE TRIGGER p025_fail_completion BEFORE INSERT ON ref_outbox WHEN (SELECT phase FROM ref_event WHERE id=NEW.event_id)='COMPLETED' BEGIN SELECT RAISE(ABORT,'fixture fail'); END")
        error("source.download", execute(failed_commit), "reference_execution_persistence_failed")
        with sqlite3.connect(db) as connection:
            uncertain = connection.execute("SELECT id FROM ref_job WHERE phase='VERIFYING'").fetchone()[0]
            held = connection.execute("SELECT (SELECT count(*) FROM ref_lock WHERE job_id=?)+(SELECT count(*) FROM ref_reservation WHERE job_id=?)", (uncertain, uncertain)).fetchone()[0]
            terminal = connection.execute("SELECT count(*) FROM ref_event WHERE job_id=? AND phase='COMPLETED'", (uncertain,)).fetchone()[0]
            connection.execute("DROP TRIGGER p025_fail_completion")
        step("completion-outbox-failure-retains-resources", held == 2 and terminal == 0, "commit final fallido conserva VERIFYING/journal/locks/reserva; no anuncia COMPLETED ni libera a ciegas")
        session.close()
        session = connect()
        requests_before = len([r for r in server.requests if r["path"] == "/artifact/3"])
        recovered_receipt = session.result("source.download", execute(plan(3)), timeout=30)
        step("recovery-reuses-verified-publication", recovered_receipt["job_id"] == uncertain and requests_before == len([r for r in server.requests if r["path"] == "/artifact/3"]), "reinicio registra interrupción; nuevo plan verifica bytes publicados y completa mismo job sin otra transferencia")
        server.mode = "slow"
        killed_plan = plan(4)
        request_id = session.send("source.download", execute(killed_plan))
        session.wait_progress(request_id)
        deadline = time.monotonic() + 5
        killed_job = None
        while time.monotonic() < deadline:
            with sqlite3.connect(db) as connection:
                row = connection.execute("SELECT j.id,c.offset FROM ref_job j JOIN ref_checkpoint c ON c.job_id=j.id WHERE j.phase='RUNNING' AND c.offset>0").fetchone()
            if row:
                killed_job, durable = row
                break
            time.sleep(0.01)
        assert killed_job
        session.crash()
        session = None
        partial = work / "staging" / killed_job / "data.bin.partial"
        step("process-killed-with-durable-partial", partial.exists() and partial.stat().st_size >= durable, "muerte real sólo del worker propio después de comprobar un checkpoint de bytes")
        server.mode = "normal"
        session = connect()
        with sqlite3.connect(db) as connection:
            recovered = connection.execute("SELECT phase FROM ref_job WHERE id=?", (killed_job,)).fetchone()[0]
            held = connection.execute("SELECT (SELECT count(*) FROM ref_lock WHERE job_id=?)+(SELECT count(*) FROM ref_reservation WHERE job_id=?)", (killed_job, killed_job)).fetchone()[0]
        step("recovery-does-not-use-a-timeout", recovered == "FAILED" and held == 0 and partial.exists(), "otra generación con lock exclusivo recupera journal interrumpido; conserva partial y registra fallo antes de liberar")
        finished = session.result("source.download", execute(plan(4)), timeout=30)
        step("post-crash-new-plan-success", finished["job_id"] == killed_job, "nuevo plan/intento retoma bytes fijados y completa el mismo job sin duplicar resultado")
        cancel_plan = plan(5)
        session.close()
        session = None
        profile_path = work / "profile.json"
        original_profile = profile_path.read_bytes()
        profile_path.write_text(json.dumps({"source_id": str(uuid.uuid7()), "storage_location_id": config["storage_location_id"]}), encoding="utf-8")
        before = rows()
        hello=json.dumps({"jsonrpc":"2.0","id":str(uuid.uuid7()),"method":"core.handshake","params":{"protocol_version":"1.0","adapter_id":manifest["id"],"nonce":uuid.uuid4().hex}}).encode()+b"\n"
        rejected = subprocess.run(executable(package, manifest), cwd=work, input=hello, capture_output=True, timeout=15)
        profile_path.write_bytes(original_profile)
        denied=json.loads(rejected.stdout)
        step("work-profile-bound-to-ledger", rejected.returncode == 0 and denied["error"]["data"]["details"]["reason"]=="reference_profile_changed" and rows() == before, "IDs configurables fuera del paquete, pero la apertura de sesión rechaza otro perfil antes de recuperar jobs")
        server.mode = "slow"
        session = Session(executable(package, manifest), manifest, cwd=work, docs=package / "docs")
        outcomes = common(session, valid_method="source.search", valid_params=search, cancel_method="source.download", cancel_params=execute(cancel_plan))
        session.close()
        session = None
        outcomes.append({"id": "shutdown-stdout-contract", "result": "PASS"})
        (run / "conformance.json").write_text(json.dumps(outcomes, indent=2), encoding="utf-8")
        step("reusable-conformance-source", len(outcomes) == 8, "mismo runner común que Rust: handshake/capabilities/schema/error, heartbeat/progreso/cancel y cierre limpio")
        registry = run / "registry"
        registry.mkdir()
        archive_path = registry / "reference-source.zip"
        with zipfile.ZipFile(archive_path, "w", compression=zipfile.ZIP_DEFLATED) as archive:
            for path in sorted(package.rglob("*")):
                if path.is_file():
                    archive.write(path, path.relative_to(package).as_posix())
        inspected = subprocess.run([str(REPO / "target/debug/larm-daemon.exe"), "inspect-adapter-registry", "--folder", str(registry)], capture_output=True, timeout=60)
        assert inspected.returncode == 0, inspected.stderr.decode("utf-8", "replace")
        discovered = json.loads(inspected.stdout)["packages"]
        step("core-discovers-portable-source-package", len(discovered) == 1 and discovered[0]["package_sha256"] == manifest["package_sha256"] and discovered[0]["adapter_id"] == manifest["id"], "CLI de registro P024 descubre ZIP autónomo y verifica sus hashes sin cambios especiales del Core ni promoción")
        step("immutable-package-and-no-auth-header", builder.package_hash(package) == manifest["package_sha256"] and all(r["authorization"] is None for r in server.requests), "paquete no mutado por Python/SQLite, sin headers de autenticación ni herencia de secretos")
        success = True
        return 0
    finally:
        if session:
            session.close()
        server.shutdown()
        server.server_close()
        thread.join(timeout=2)
        record = {"result": "PASS" if success else "FAIL", "scope": "source RPC con Python autónomo, catálogo local, HTTP fixture y ledger privado; sin admisión/API download de Core ni P026", "steps": steps, "run": str(run.relative_to(REPO)), "package_hash": manifest["package_sha256"] if manifest else None, "host": platform.platform(), "generated_at": datetime.now(timezone.utc).isoformat()}
        (run / "receipt.json").write_text(json.dumps(record, ensure_ascii=False, indent=2), encoding="utf-8")
        (REPO / "docs/reports/P025_runs/source-real.json").write_text(json.dumps(record, ensure_ascii=False, indent=2), encoding="utf-8")


if __name__ == "__main__":
    sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    raise SystemExit(main())
