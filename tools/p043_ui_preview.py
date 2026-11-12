#!/usr/bin/env python3
"""QA visual aislada del bundle real; fixtures explícitas, sin daemon/secretos."""
import functools
import http.server
import json
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
FIXTURE = {
    "id": "0192aaaa-0000-7000-8000-000000000001", "row_version": 3,
    "name": "Modelos del equipo (prueba visual)",
    "adapter_id": "0192aaaa-0000-7000-8000-000000000002",
    "base_url": "https://example.invalid/repositorios/modelos-del-equipo/coleccion-con-nombre-largo-para-verificar-ajuste-de-lineas",
    "priority": 2, "enabled": False, "auth_mode": "TOKEN", "account_id": None,
    "policy": {"vendor": {"unknown": None}}, "metadata": {},
    "created_at": "2026-10-17T00:00:00.000Z", "updated_at": "2026-10-17T00:00:00.000Z",
}
ADAPTER = {"id": "0192aaaa-0000-7000-8000-000000000003", "adapter_id": "fixture.source.http", "version": "1.0.0", "family": "source", "state": "INSTALLED", "trust": "UNTRUSTED", "metadata": {"source_bindings": [{"id": FIXTURE["adapter_id"], "source_type": "HTTP"}]}}
SCRIPT = """<script>
const fixture = FIXTURE;
const adapter = ADAPTER;
window.__TAURI_INTERNALS__ = {invoke:async function(cmd,args){
  if(cmd==='smoke_enabled') return false;
  if(cmd!=='api_request' || args.request.method!=='GET') throw new Error('QA de sólo lectura');
  const path=args.request.path.split('?')[0];
  if(path.endsWith('/health/live')) return {status:200,body:{status:'LIVE',schema_version:'1'}};
  if(path.endsWith('/health/ready')) return {status:200,body:{status:'READY',schema_version:'1'}};
  const items=path==='/api/v1/sources'?[fixture]:path==='/api/v1/adapters'?[adapter]:[];
  return {status:200,body:{items,next_cursor:null,snapshot_at:'2026-10-17T00:00:00.000Z'}};
}};
</script>""".replace("FIXTURE", json.dumps(FIXTURE)).replace("ADAPTER", json.dumps(ADAPTER))


class Handler(http.server.SimpleHTTPRequestHandler):
    def do_GET(self):
        if self.path.split("?")[0] == "/qa.html":
            raw = (REPO / "apps/desktop/dist/index.html").read_text(encoding="utf-8").replace("<head>", "<head>" + SCRIPT)
            raw = raw.replace("<body>", '<body><p style="margin:0;padding:0.5rem 1rem;background:#ffecb0;color:#111418">Vista QA · datos de prueba · sin conexión al daemon</p>')
            data = raw.encode("utf-8")
            self.send_response(200)
            self.send_header("Content-Type", "text/html; charset=utf-8")
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)
        else:
            super().do_GET()


if __name__ == "__main__":
    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), functools.partial(Handler, directory=str(REPO / "apps/desktop/dist")))
    print(f"P043_UI_PREVIEW http://127.0.0.1:{server.server_port}/qa.html#/fuentes", flush=True)
    server.serve_forever()
