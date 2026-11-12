#!/usr/bin/env python3
"""P025 parcial: IO real del transporte privado, no RPC source ni P050."""
import hashlib
import http.server
import importlib.util
import json
import os
import socket
import subprocess
import sys
import tempfile
import threading
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(REPO / "adapters/sdk/python"))
from larm_adapter_sdk import DomainError  # noqa: E402
spec = importlib.util.spec_from_file_location("reference_transfer", REPO / "adapters/source/reference/transfer.py")
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
DATA = bytes(range(256)) * 4096
DIGEST = hashlib.sha256(DATA).hexdigest()
ETAG = f'"{DIGEST}"'


class Context:
    def __init__(self, cancel_at=None):
        self.events, self.cancel_at = [], cancel_at

    def check_cancelled(self):
        if self.cancel_at is not None and self.events and self.events[-1][1] >= self.cancel_at:
            raise DomainError("RESOURCE_UNAVAILABLE", "Operación cancelada", "operation_cancelled")

    def progress(self, phase, completed, total, code):
        self.events.append((phase, completed, total, code))


class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_args):
        pass

    def do_GET(self):
        self.server.requests.append({"path": self.path, "range": self.headers.get("Range"), "if_range": self.headers.get("If-Range"), "authorization": self.headers.get("Authorization")})
        mode = self.server.mode
        if mode in ("auth", "rate", "redirect"):
            self.send_response({"auth": 401, "rate": 429, "redirect": 302}[mode])
            if mode == "rate":
                self.send_header("Retry-After", "900")
                self.send_header("X-RateLimit-Limit", "100")
                self.send_header("X-RateLimit-Remaining", "0")
            if mode == "redirect":
                self.send_header("Location", self.server.base + "/redirected")
            self.send_header("Content-Length", "0")
            self.end_headers()
            return
        offset = int(self.headers["Range"].removeprefix("bytes=").removesuffix("-")) if self.headers.get("Range") else 0
        if mode == "ignore-range":
            offset = 0
        self.send_response(206 if offset else 200)
        self.send_header("ETag", '"changed"' if mode == "etag-changed" else ETAG)
        if offset:
            self.send_header("Content-Range", f"bytes {offset + (1 if mode == 'wrong-range' else 0)}-{len(DATA)-1}/{len(DATA)}")
        self.send_header("Content-Length", str(len(DATA) - offset + (1 if mode == "wrong-length" else 0)))
        self.end_headers()
        payload = (b"x" * len(DATA) if mode == "wrong-bytes" else DATA)[offset:]
        if mode == "cut":
            payload = payload[:128 << 10]
        try:
            self.wfile.write(payload)
            self.wfile.flush()
            if mode == "cut":
                self.connection.shutdown(socket.SHUT_RDWR)
                self.connection.close()
        except (BrokenPipeError, ConnectionResetError, OSError):
            pass


class Transfers(unittest.TestCase):
    def setUp(self):
        base = (REPO / "docs/reports/P025_runs").resolve()
        base.mkdir(exist_ok=True)
        self.temp = tempfile.TemporaryDirectory(prefix="transfer-", dir=base)
        self.root = Path(self.temp.name).resolve()
        assert self.root.is_relative_to(base)
        self.work = self.root / "managed"
        self.work.mkdir()
        self.target, self.partial = self.work / "model.bin", self.work / "state/model.part"
        self.server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.server.mode, self.server.requests = "normal", []
        self.server.base = f"http://127.0.0.1:{self.server.server_port}"
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()

    def tearDown(self):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=2)
        assert self.root.is_relative_to((REPO / "docs/reports/P025_runs").resolve())
        self.temp.cleanup()

    def run_transfer(self, **over):
        args = {"url": self.server.base + "/artifact", "target": self.target, "partial": self.partial, "size": len(DATA), "sha256": DIGEST, "etag": ETAG, "read_roots": [self.work], "write_roots": [self.work], "origins": [self.server.base], "ctx": Context(), "hard_budget": len(DATA)}
        return module.transfer(**(args | over))

    def error(self, reason, **over):
        with self.assertRaises(DomainError) as raised:
            self.run_transfer(**over)
        self.assertEqual(raised.exception.data["details"]["reason"], reason)
        return raised.exception.data

    def seed(self, size=128 << 10):
        self.partial.parent.mkdir()
        self.partial.write_bytes(DATA[:size])

    def test_actual_bytes_sha256_progress_and_reuse_without_overwrite(self):
        context = Context()
        result = self.run_transfer(ctx=context)
        self.assertEqual(self.target.read_bytes(), DATA)
        self.assertEqual(hashlib.sha256(self.target.read_bytes()).hexdigest(), DIGEST)
        self.assertEqual(result, {"received_bytes": len(DATA), "resumed_from": 0, "reused": False})
        self.assertFalse(self.partial.exists())
        self.assertEqual(context.events[-1][1], len(DATA))
        before = self.target.stat().st_mtime_ns
        self.assertTrue(self.run_transfer()["reused"])
        self.assertEqual(self.target.stat().st_mtime_ns, before)
        self.assertEqual(len(self.server.requests), 1)
        self.assertIsNone(self.server.requests[0]["authorization"])

    def test_network_interruption_preserves_partial_and_next_attempt_uses_range(self):
        self.server.mode = "cut"
        self.error("source_network_interrupted")
        self.assertFalse(self.target.exists())
        offset = self.partial.stat().st_size
        self.assertGreater(offset, 0)
        self.assertLess(offset, len(DATA))
        self.server.mode = "normal"
        result = self.run_transfer()
        self.assertEqual(result["resumed_from"], offset)
        self.assertEqual(result["received_bytes"], len(DATA) - offset)
        self.assertEqual(self.server.requests[-1]["range"], f"bytes={offset}-")
        self.assertEqual(self.server.requests[-1]["if_range"], ETAG)
        self.assertEqual(self.target.read_bytes(), DATA)

    def test_ignored_range_restarts_and_never_concatenates(self):
        self.seed()
        self.server.mode = "ignore-range"
        result = self.run_transfer()
        self.assertEqual(result["resumed_from"], 0)
        self.assertEqual(self.target.read_bytes(), DATA)

    def test_changed_etag_and_bad_range_do_not_modify_partial(self):
        self.seed()
        before = self.partial.read_bytes()
        for mode, reason in (("etag-changed", "source_revision_changed"), ("wrong-range", "reference_content_range_mismatch"), ("wrong-length", "reference_content_length_mismatch")):
            self.server.mode = mode
            self.error(reason)
            self.assertEqual(self.partial.read_bytes(), before)
            self.assertFalse(self.target.exists())

    def test_wrong_checksum_is_never_published_or_used_to_replace_target(self):
        self.server.mode = "wrong-bytes"
        self.error("reference_checksum_mismatch")
        self.assertTrue(self.partial.exists())
        self.assertFalse(self.target.exists())
        self.target.write_bytes(b"existing data")
        self.error("reference_existing_target_mismatch")
        self.assertEqual(self.target.read_bytes(), b"existing data")

    def test_cancel_keeps_partial_then_new_context_can_resume(self):
        self.error("operation_cancelled", ctx=Context(cancel_at=64 << 10))
        offset = self.partial.stat().st_size
        self.assertGreaterEqual(offset, 64 << 10)
        self.assertFalse(self.target.exists())
        result = self.run_transfer()
        self.assertEqual(result["resumed_from"], offset)
        self.assertEqual(self.target.read_bytes(), DATA)

    def test_auth_rate_and_redirect_use_real_http_errors_without_writes(self):
        for mode, reason in (("auth", "source_auth_required"), ("rate", "source_rate_limited"), ("redirect", "reference_redirect_denied")):
            self.server.mode = mode
            error = self.error(reason)
            if mode == "rate":
                rate = json.loads(error["details"]["original_error"]["message"])
                self.assertEqual(rate["retry_after_seconds"], "900")
                self.assertEqual(rate["remaining"], "0")
                self.assertTrue(error["retryable"])
            self.assertFalse(self.partial.exists())
            self.assertFalse(self.partial.parent.exists())
            self.assertFalse(self.target.exists())
        self.assertEqual([request["path"] for request in self.server.requests], ["/artifact"] * 3)

    def test_budget_origin_and_path_escape_are_denied_before_network_or_file(self):
        for over, reason in (({"hard_budget": len(DATA) - 1}, "reference_budget_exceeded"), ({"url": "http://127.0.0.1:1/foreign"}, "reference_origin_not_allowed"), ({"url": self.server.base + "/x?token=fixture"}, "reference_credentials_in_url"), ({"target": self.root / "outside.bin"}, "reference_path_outside_roots"), ({"read_roots": []}, "reference_path_outside_roots")):
            self.error(reason, **over)
            self.assertFalse(self.target.exists())
            self.assertFalse(self.partial.exists())
            self.assertEqual(self.server.requests, [])
        for value in ("../x", "x/../y", "C:/x", "a\\b", "con.txt", "x.", "/x", "a//b", "a:stream"):
            with self.assertRaises(DomainError, msg=value):
                module.relative(value)

    @unittest.skipUnless(os.name == "nt", "junction Windows")
    def test_real_junction_is_rejected_without_touching_target(self):
        outside = self.root / "external"
        outside.mkdir()
        link = self.work / "junction"
        quote = lambda p: "'" + str(p).replace("'", "''") + "'"
        subprocess.run(["powershell", "-NoProfile", "-Command", f"New-Item -ItemType Junction -Path {quote(link)} -Target {quote(outside)} | Out-Null"], capture_output=True, check=True, timeout=15)
        self.error("reference_reparse_denied", target=link / "escaped.bin")
        self.assertEqual(list(outside.iterdir()), [])
        self.assertEqual(self.server.requests, [])


if __name__ == "__main__":
    unittest.main(verbosity=2)
