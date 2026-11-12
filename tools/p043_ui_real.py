#!/usr/bin/env python3
"""UI del bundle real → proxy QA → daemon/SQLite reales en perfil desechable.

Sólo el proxy retiene el token. El navegador no recibe credenciales. La
instalación inicial INSTALLED/UNTRUSTED es fixture, nunca cualificación P026.
CTRL+C cierra exclusivamente este daemon y revoca su principal de prueba.
"""
import functools
import http.server
import json
import platform
import shutil
import sqlite3
import subprocess
import sys
import threading
import urllib.error
import urllib.request
import re
import uuid
from datetime import datetime, timezone
from pathlib import Path

from p043_sources_real import EXE, REPO, cli, sha
from p023_real import clean_env, start, stop


def main():
    run = REPO / "docs/reports/P043_runs/ui-real" / uuid.uuid4().hex[:12]
    run.mkdir(parents=True)
    data = run / "data"
    cli(data, "db-info")
    db = data / "state/larm.sqlite"
    latest = json.loads((REPO / "docs/reports/P043_runs/sources-real.json").read_text(encoding="utf-8"))
    previous = REPO / latest["run"] / "data/adapters/fixture.source/1.0.0"
    package = data / "adapters/fixture.source/1.0.0"
    shutil.copytree(previous, package)
    manifest = json.loads((package / "manifest.json").read_text(encoding="utf-8"))
    manifest["executable"]["args"] = ["--manifest", str(package / "manifest.json")]
    (package / "manifest.json").write_text(json.dumps(manifest), encoding="utf-8")
    installation = str(uuid.uuid7())
    stamp = datetime.now(timezone.utc).isoformat(timespec="milliseconds").replace("+00:00", "Z")
    with sqlite3.connect(db) as connection:
        connection.execute("INSERT INTO adapter_installation(id,created_at,updated_at,row_version,metadata,adapter_id,version,family,package_hash,publisher,trust,manifest,state) VALUES (?,?,?,1,'{}',?,?,?,?,?,'UNTRUSTED',?,'INSTALLED')", (installation, stamp, stamp, manifest["id"], manifest["version"], "source", manifest["package_sha256"], manifest["publisher"], json.dumps(manifest)))
    descriptor = run / "support.json"
    descriptor.write_text(json.dumps({"search_applicable": True, "supported_filters": ["source.search.q"], "limitations": ["fixture_only"]}), encoding="utf-8")
    cli(data, "register-source-adapter", "--installation", installation, "--source-type", "HTTP", "--descriptor", descriptor)
    name = f"p043-ui-{run.name}"
    principal = cli(data, "enroll-principal", "--name", name, "--kind", "OPERATOR", "--scopes", "read,models:write")
    token, daemon, server = principal["token"], None, None
    records = []
    nonce = uuid.uuid4().hex
    script = """<script>
window.__TAURI_INTERNALS__={invoke:async function(cmd,args){
 if(cmd==='smoke_enabled')return false;
 if(cmd!=='api_request')throw new Error('Comando ajeno a QA');
 const response=await fetch('/qa-api-NONCE',{method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify(args.request)});
 if(!response.ok)throw new Error('Fallo en el puente QA');return response.json();
}};
</script>""".replace("NONCE", nonce)

    class Handler(http.server.SimpleHTTPRequestHandler):
        def log_message(self, *_args):
            pass

        def do_GET(self):
            if self.path == "/qa.html":
                raw = (REPO / "apps/desktop/dist/index.html").read_text(encoding="utf-8").replace("<head>", "<head>" + script)
                raw = raw.replace("<body>", '<body><p style="margin:0;padding:0.5rem 1rem;background:#ffecb0;color:#111418">QA real · perfil de prueba aislado</p>')
                payload = raw.encode("utf-8")
                self.send_response(200)
                self.send_header("Content-Type", "text/html; charset=utf-8")
                self.send_header("Content-Length", str(len(payload)))
                self.end_headers()
                self.wfile.write(payload)
            else:
                super().do_GET()

        def do_POST(self):
            if self.path == f"/qa-finish-{nonce}" and self.headers.get("Origin") == f"http://127.0.0.1:{server.server_port}":
                self.send_response(204)
                self.end_headers()
                threading.Thread(target=server.shutdown, daemon=True).start()
                return
            if self.path != f"/qa-api-{nonce}" or self.headers.get("Origin") != f"http://127.0.0.1:{server.server_port}":
                self.send_error(403)
                return
            length = int(self.headers.get("Content-Length", "0"))
            if not 0 < length <= 65536:
                self.send_error(400)
                return
            request = json.loads(self.rfile.read(length))
            method, path = request["method"], request["path"]
            resource = path.split("?")[0]
            allowed = method == "GET" and resource in ("/api/v1/health/live", "/api/v1/health/ready", "/api/v1/adapters", "/api/v1/sources")
            allowed |= method == "POST" and resource == "/api/v1/sources"
            allowed |= method in ("PATCH", "DELETE") and resource.startswith("/api/v1/sources/") and len(resource.split("/")) == 5
            if not allowed or ".." in path or "#" in path:
                self.send_error(403)
                return
            body = json.dumps(request["body"]).encode() if "body" in request else None
            upstream = urllib.request.Request(base.removesuffix("/api/v1") + path, method=method, data=body)
            upstream.add_header("authorization", f"Bearer {token}")
            if body is not None:
                upstream.add_header("content-type", "application/json")
            for key, value in request.get("headers", {}).items():
                if key.lower() in ("idempotency-key", "if-match", "x-correlation-id"):
                    upstream.add_header(key, value)
            try:
                with urllib.request.urlopen(upstream, timeout=30) as response:
                    status, raw = response.status, response.read()
            except urllib.error.HTTPError as error:
                status, raw = error.code, error.read()
            if method != "GET":
                with sqlite3.connect(db) as connection:
                    rows = connection.execute("SELECT name,row_version,enabled FROM model_source ORDER BY id").fetchall()
                records.append({"method": method, "path": path, "status": status, "rows_after": rows})
                (run / "requests.json").write_text(json.dumps(records, indent=2), encoding="utf-8")
            payload = json.dumps({"status": status, "body": json.loads(raw) if raw else None}).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)

    try:
        daemon, base = start(data)
        server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), functools.partial(Handler, directory=str(REPO / "apps/desktop/dist")))
        print(f"P043_UI_REAL http://127.0.0.1:{server.server_port}/qa.html#/fuentes", flush=True)
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        if server:
            server.server_close()
        if daemon and daemon.poll() is None:
            stop(daemon)
            daemon.wait(timeout=15)
        revoked = subprocess.run([str(EXE), "--data-root", str(data), "revoke-principal", "--name", name], capture_output=True, env=clean_env(), timeout=30)
        with sqlite3.connect(db) as connection:
            events = connection.execute("SELECT resource_version FROM event WHERE resource_type='model_source' ORDER BY event_id").fetchall()
            remaining = connection.execute("SELECT count(*) FROM model_source").fetchone()[0]
            active = connection.execute("SELECT state,trust FROM adapter_installation WHERE id=?", (installation,)).fetchone()
        passed = ([r["status"] for r in records] == [201, 200, 204]
                  and records[0]["rows_after"] == [("Fuente QA real", 1, 0)]
                  and records[1]["rows_after"] == [("Fuente QA real editada", 2, 0)]
                  and not records[2]["rows_after"] and remaining == 0
                  and events == [(1,), (2,), (3,)] and active == ("INSTALLED", "UNTRUSTED"))
        paths = ["apps/desktop/src/sources.tsx", "apps/desktop/src/App.tsx", "apps/desktop/src/styles.css", "apps/desktop/dist/index.html", "target/debug/larm-daemon.exe"]
        record = {"result": "PASS" if passed else "FAIL", "scope": "bundle web real → puente QA sin token en navegador → daemon/SQLite reales; no WebView nativo ni upstream remoto", "requests": records, "events": events, "remaining": remaining, "installation_state": active, "run": str(run.relative_to(REPO)), "source_sha256": {p: sha(REPO / p) for p in paths}, "host": platform.platform(), "generated_at": datetime.now(timezone.utc).isoformat()}
        record["cleanup"] = {"principal_revoked": revoked.returncode == 0, "daemon_exit_code": daemon.returncode if daemon else None}
        if revoked.returncode != 0 or (daemon and daemon.returncode != 0):
            record["result"] = "FAIL"
            passed = False
        (run / "receipt.json").write_text(json.dumps(record, ensure_ascii=False, indent=2), encoding="utf-8")
        (REPO / "docs/reports/P043_runs/ui-real.json").write_text(json.dumps(record, ensure_ascii=False, indent=2), encoding="utf-8")
        print(f"P043 UI real: {record['result']}; principal revocado y daemon propio detenido", flush=True)
    return 0 if passed else 1


if __name__ == "__main__":
    sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    if len(sys.argv) == 3 and sys.argv[1] == "--finish":
        url = sys.argv[2].split("#")[0]
        if not re.fullmatch(r"http://127\.0\.0\.1:\d+/qa\.html", url):
            raise SystemExit("Sólo la URL loopback de esta prueba")
        html = urllib.request.urlopen(url, timeout=10).read().decode()
        nonce = re.search(r"/qa-api-([a-f0-9]{32})", html).group(1)
        origin = url.removesuffix("/qa.html")
        request = urllib.request.Request(f"{origin}/qa-finish-{nonce}", method="POST", headers={"Origin": origin})
        with urllib.request.urlopen(request, timeout=10) as response:
            assert response.status == 204
        print("Cierre ordenado solicitado al proceso de QA propio")
        raise SystemExit(0)
    raise SystemExit(main())
