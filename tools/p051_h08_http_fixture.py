#!/usr/bin/env python3
"""Owned, unthrottled TCP source for the Core H08 throughput experiment.

The source returns SourceDownloadChunk JSON over HTTP, not a qualified P048
worker. Its monotonic trace measures decoded model bytes independently of the
consumer's limiter. No artificial clock, response delay or bandwidth cap.
"""
from __future__ import annotations

import base64
import json
from pathlib import Path
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


def main() -> int:
    trace = Path(sys.argv[1]).resolve()
    trace.parent.mkdir(parents=True, exist_ok=True)
    trace.touch(exist_ok=False)
    lock = threading.Lock()
    data = b"0123456789"

    def record(item):
        with lock, trace.open("a", encoding="utf-8") as out:
            out.write(json.dumps(item) + "\n")
            out.flush()

    class HTTP(BaseHTTPRequestHandler):
        def log_message(self, *args):
            pass

        def reply(self, value):
            encoded = json.dumps(value).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(encoded)))
            self.send_header("Connection", "close")
            self.end_headers()
            self.wfile.write(encoded)
            self.wfile.flush()
            self.close_connection = True

        def do_GET(self):
            if not self.path.startswith("/mark/"):
                self.send_error(404)
                return
            value = {"kind": "mark", "phase": self.path.removeprefix("/mark/"),
                     "monotonic_ns": str(time.monotonic_ns())}
            record(value)
            self.reply(value)

        def do_POST(self):
            length = int(self.headers.get("Content-Length", "0"))
            if self.path != "/chunk" or not 0 < length <= 16384:
                self.send_error(400)
                return
            request = json.loads(self.rfile.read(length))
            offset, bound = int(request["offset"]), int(request["max_bytes"])
            assert request["relative_path"] == "weights.gguf"
            assert 0 <= offset <= len(data) and 0 < bound <= 524288
            validator = {"kind": "STRONG_ETAG", "value": '"h08-owned-v1"'}
            if offset:
                assert request["validator"] == validator
            payload = data[offset:offset + bound]
            start = time.monotonic_ns()
            self.reply({"offset": str(offset), "data_base64": base64.b64encode(payload).decode(),
                        "eof": offset + len(payload) == len(data),
                        "total_bytes": str(len(data)), "validator": validator})
            record({"kind": "read", "phase": self.headers["X-H08-Phase"],
                    "job_id": request["job_id"], "offset": str(offset),
                    "requested_bytes": str(bound), "bytes": str(len(payload)),
                    "started_ns": str(start), "monotonic_ns": str(time.monotonic_ns())})

    server = ThreadingHTTPServer(("127.0.0.1", 0), HTTP)
    server.daemon_threads = True
    print(json.dumps({"port": server.server_port, "payload": "0123456789", "scope": __doc__}), flush=True)
    try:
        server.serve_forever(poll_interval=.02)
    finally:
        server.server_close()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
