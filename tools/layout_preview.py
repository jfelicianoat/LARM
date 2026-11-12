#!/usr/bin/env python3
"""Vista aislada del frontend compilado con observaciones reales para QA visual.

El transporte se sustituye por fixtures sólo en /qa.html; no se conecta al
daemon ni contiene credenciales. Sirve el mismo JS/CSS de dist, sin editarlo.
"""
import functools
import http.server
import json
import sqlite3
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
info = json.loads((REPO / "docs/reports/AUDIT_FIXES_2026-10-16_runs/latest-ui.json").read_text(encoding="utf-8"))
run = Path(info["run"])
with sqlite3.connect(run / "data/state/larm.sqlite") as conn:
    conn.row_factory = sqlite3.Row
    rows = [dict(row) for row in conn.execute("SELECT * FROM storage_location")]
for row in rows:
    row["metadata"] = json.loads(row["metadata"])
    row["metadata"]["volume_budget"] = {"usable_bytes": str(max(0, row["free_bytes"] - row["reserved_bytes"])), "shared_with": []}
    for key in ("capacity_bytes", "free_bytes", "reserved_bytes"):
        row[key] = str(row[key])
    row["auto_placement"] = bool(row["auto_placement"])
fixture = {"locations": rows, "hardware": json.loads((run / "hardware.json").read_text(encoding="utf-8"))}
script = """<script>
window.__LARM_LAYOUT_QA = FIXTURE;
window.__TAURI_INTERNALS__ = {invoke: async function(cmd,args) {
  if(cmd === 'smoke_enabled') return false;
  if(cmd !== 'api_request') throw new Error('Comando no disponible en QA');
  const request=args.request;
  if(request.method !== 'GET') throw new Error('La vista QA es de sólo lectura');
  const path=request.path.split('?')[0];
  if(path === '/api/v1/health/live') return {status:200,body:{status:'LIVE',schema_version:'1'}};
  if(path === '/api/v1/health/ready') return {status:200,body:{status:'READY',schema_version:'1'}};
  if(path === '/api/v1/storage-locations') return {status:200,body:{items:window.__LARM_LAYOUT_QA.locations,next_cursor:null}};
  if(path === '/api/v1/hardware') return {status:200,body:{items:[window.__LARM_LAYOUT_QA.hardware],next_cursor:null}};
  return {status:404,body:{code:'NOT_FOUND',message:'Ruta fuera de la vista QA',retryable:false,correlation_id:'qa',details:{reason:'qa_read_only'}}};
}};
</script>""".replace("FIXTURE", json.dumps(fixture).replace("<", "\\u003c"))


class Handler(http.server.SimpleHTTPRequestHandler):
    def do_GET(self):
        if self.path.split("?")[0] == "/qa.html":
            raw = (REPO / "apps/desktop/dist/index.html").read_text(encoding="utf-8").replace("<head>", "<head>" + script).encode()
            self.send_response(200)
            self.send_header("Content-Type", "text/html; charset=utf-8")
            self.send_header("Content-Length", str(len(raw)))
            self.end_headers()
            self.wfile.write(raw)
        else:
            super().do_GET()


if __name__ == "__main__":
    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), functools.partial(Handler, directory=str(REPO / "apps/desktop/dist")))
    print(f"LAYOUT_PREVIEW http://127.0.0.1:{server.server_port}/qa.html#/almacenamiento", flush=True)
    server.serve_forever()
