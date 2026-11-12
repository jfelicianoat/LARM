#!/usr/bin/env python3
"""Serve owned bytes and faults to the original P048 executable on loopback."""
from __future__ import annotations

import hashlib
import json
import os
from pathlib import Path
import re
import sys
import threading
import time
import uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

REPO = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(REPO / "adapters/source/p048"))
from package import create  # noqa: E402


def main() -> int:
    root = Path(sys.argv[1]).resolve()
    oracle = root / "oracle"
    oracle.mkdir(parents=True)
    expected = bytes(range(256)) * 4097
    expected_file = oracle / "expected.bin"
    expected_file.write_bytes(expected)
    checksum = hashlib.sha256(expected).hexdigest()
    control = oracle / "control.json"
    control.write_text('{"mode":"normal"}', encoding="utf-8")
    requests = oracle / "requests.ndjson"
    requests.touch()
    evidence = root / "evidence"
    evidence.mkdir()
    lock = threading.Lock()

    class HTTP(BaseHTTPRequestHandler):
        def log_message(self, *args):
            pass

        def send_response(self, code, message=None):
            observed = {**self.observed, "status": code}
            with lock, requests.open("a", encoding="utf-8") as output:
                output.write(json.dumps(observed) + "\n")
                output.flush()
                os.fsync(output.fileno())
            super().send_response(code, message)

        def do_HEAD(self):
            self.handle_read(True)

        def do_GET(self):
            self.handle_read(False)

        def handle_read(self, head):
            mode = json.loads(control.read_text(encoding="utf-8"))["mode"]
            raw_range = self.headers.get("Range")
            self.observed = {"method": self.command, "path": self.path, "range": raw_range,
                        "if_range": self.headers.get("If-Range"), "mode": mode,
                        "authorization_present": self.headers.get("Authorization") is not None}
            if self.path != "/weights.bin":
                self.send_error(404)
                return
            if mode in ("auth", "rate"):
                self.send_response(401 if mode == "auth" else 429)
                self.send_header("Retry-After", "1")
                self.send_header("Content-Length", "0")
                self.end_headers()
                return
            start, end = 0, len(expected) - 1
            if raw_range:
                match = re.fullmatch(r"bytes=(\d+)-(\d*)", raw_range)
                if not match:
                    self.send_error(400)
                    return
                start = int(match[1])
                end = min(int(match[2]) if match[2] else end, end)
                if start >= len(expected) or start > end:
                    self.send_response(416)
                    self.send_header("Content-Range", f"bytes */{len(expected)}")
                    self.send_header("Content-Length", "0")
                    self.end_headers()
                    return
            self.send_response(206 if raw_range else 200)
            self.send_header("Content-Length", str(end - start + 1))
            self.send_header("ETag", f'"{checksum}"')
            if raw_range:
                self.send_header("Content-Range", f"bytes {start}-{end}/{len(expected)}")
            self.end_headers()
            if head:
                return
            if mode == "held":
                deadline = time.monotonic() + 20
                while json.loads(control.read_text(encoding="utf-8"))["mode"] == "held" and time.monotonic() < deadline:
                    time.sleep(.01)
            try:
                payload = bytes([expected[0] ^ 255]) + expected[1:] if mode == "corrupt" else expected
                self.wfile.write(payload[start:start + 128] if mode == "cut" else payload[start:end + 1])
                self.wfile.flush()
            except OSError:
                pass
            self.close_connection = True

    server = ThreadingHTTPServer(("127.0.0.1", 0), HTTP)
    server.daemon_threads = True
    url = f"http://127.0.0.1:{server.server_port}/weights.bin"
    package = root / "data/adapters/larm.source.p048.http/1.0.0"
    package.parent.mkdir(parents=True)
    work = root / "work"
    result = create(REPO, package, work, "HTTP", url=url, checksum=checksum,
                    binary=REPO / "target/debug/larm-source-p048.exe")
    print(json.dumps({"package": str(package), "work": str(work), "config": {
        "source_id": result["profile"]["source_id"], "storage_id": str(uuid.uuid7()),
        "url": url, "expected_file": str(expected_file), "control": str(control),
        "requests": str(requests), "evidence_root": str(evidence)}}), flush=True)
    try:
        server.serve_forever(poll_interval=.05)
    finally:
        server.server_close()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
