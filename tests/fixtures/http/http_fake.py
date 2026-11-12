#!/usr/bin/env python3
"""Servidor HTTP falso (fixture común, P007) para fuentes y descargas, solo en 127.0.0.1.

  python http_fake.py --routes rutas.json [--port 0] [--ready-file f]

Imprime `LISTENING <puerto>` en stdout (y en --ready-file si se da). Cada ruta del JSON:

  {"GET /api/models/x": {"status": 200, "json": {...}}}
  {"GET /files/model.gguf": {"file": "ruta", "etag": "\"abc\""}}            # Range y 206/416, ETag/304
  {"GET /slow": {"status": 200, "body": "ok", "delay_ms": 3000}}
  {"GET /cut": {"file": "ruta", "cut_after_bytes": 1000}}                   # corta la conexión
  {"GET /limited": {"status": 429, "headers": {"Retry-After": "2"}}}
  {"HEAD /files/model.gguf": {"file": "ruta"}}                              # Content-Length sin cuerpo

Registra cada petición (método, ruta, Range, Authorization presente sí/no — nunca el valor)
en el fichero `--log` como NDJSON para que las pruebas observen la conducta del cliente.
"""
import argparse
import json
import os
import socketserver
import sys
import time
from http.server import BaseHTTPRequestHandler
from pathlib import Path


class Handler(BaseHTTPRequestHandler):
    routes: dict = {}
    log_path: Path | None = None

    def log_message(self, *_args) -> None:  # sin ruido en stderr
        pass

    def _record(self) -> None:
        if self.log_path:
            entry = {"method": self.command, "path": self.path, "range": self.headers.get("Range"),
                     "if_none_match": self.headers.get("If-None-Match"),
                     "authorization": self.headers.get("Authorization") is not None}
            with self.log_path.open("a", encoding="utf-8") as f:
                f.write(json.dumps(entry) + "\n")

    def _serve(self, head: bool) -> None:
        self._record()
        route = self.routes.get(f"{self.command} {self.path.split('?')[0]}")
        if route is None:
            self.send_response(404)
            self.send_header("Content-Length", "0")
            self.end_headers()
            return
        time.sleep(route.get("delay_ms", 0) / 1000)
        headers = dict(route.get("headers", {}))
        if "file" in route:
            data = Path(route["file"]).read_bytes()
            etag = route.get("etag")
            if etag:
                headers["ETag"] = etag
                if self.headers.get("If-None-Match") == etag:
                    self.send_response(304)
                    self.send_header("ETag", etag)
                    self.end_headers()
                    return
            status, body = 200, data
            rng = self.headers.get("Range")
            if rng and rng.startswith("bytes="):
                start_s, _, end_s = rng[6:].partition("-")
                start = int(start_s or 0)
                end = int(end_s) if end_s else len(data) - 1
                if start >= len(data):
                    self.send_response(416)
                    self.send_header("Content-Range", f"bytes */{len(data)}")
                    self.send_header("Content-Length", "0")
                    self.end_headers()
                    return
                status, body = 206, data[start:end + 1]
                headers["Content-Range"] = f"bytes {start}-{min(end, len(data) - 1)}/{len(data)}"
            headers["Accept-Ranges"] = "bytes"
        elif "json" in route:
            status, body = route.get("status", 200), json.dumps(route["json"]).encode()
            headers.setdefault("Content-Type", "application/json")
        else:
            status, body = route.get("status", 200), str(route.get("body", "")).encode()
        self.send_response(status)
        for k, v in headers.items():
            self.send_header(k, v)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        if head:
            return
        cut = route.get("cut_after_bytes")
        if cut is not None:
            self.wfile.write(body[:cut])
            self.wfile.flush()
            self.connection.shutdown(2)  # corte a mitad del cuerpo
            return
        self.wfile.write(body)

    def do_GET(self) -> None:  # noqa: N802
        self._serve(head=False)

    def do_HEAD(self) -> None:  # noqa: N802
        self._serve(head=True)


class Server(socketserver.ThreadingTCPServer):
    daemon_threads = True
    allow_reuse_address = False


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--routes", type=Path, required=True)
    ap.add_argument("--port", type=int, default=0)
    ap.add_argument("--log", type=Path)
    ap.add_argument("--ready-file", type=Path)
    args = ap.parse_args()
    Handler.routes = json.loads(args.routes.read_text(encoding="utf-8"))
    Handler.log_path = args.log
    with Server(("127.0.0.1", args.port), Handler) as server:
        port = server.server_address[1]
        print(f"LISTENING {port}", flush=True)
        if args.ready_file:
            args.ready_file.write_text(str(port), encoding="utf-8")
        server.serve_forever()
    return 0


if __name__ == "__main__":
    os.environ.setdefault("PYTHONUNBUFFERED", "1")
    raise SystemExit(main())
