#!/usr/bin/env python3
"""P025 parcial: SDK Python con procesos Windows reales y oráculos canónicos."""
import copy
import json
import os
import queue
import subprocess
import sys
import tempfile
import threading
import unittest
import uuid
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(REPO / "adapters/sdk/python"))
from larm_adapter_sdk import Contracts, ContractViolation  # noqa: E402


class Worker:
    def __init__(self, contracts, *, workers=4, capacity=64, gate_mode=None, capabilities=None):
        self.contracts = contracts
        self.temp = tempfile.TemporaryDirectory(prefix="p025-sdk-", dir=REPO / "docs/reports/P025_runs")
        self.root = Path(self.temp.name)
        table = contracts.methods
        methods = contracts.common + [name for name, item in table.items() if item["family"] == "source"]
        self.manifest = {"id": "fixture.python", "version": "1.0.0", "family": "source", "protocol_version": "1.0", "publisher": "P025 SDK transport fixture", "executable": {"relative_path": "bin/python.exe", "args": []}, "platforms": ["windows"], "architectures": ["x86_64"], "methods": methods, "permissions": {"read_roots": [], "write_roots": [], "network_origins": [], "process_allowlist": [], "secret_refs": []}, "capabilities": ["source.search"], "package_sha256": "a" * 64, "signature": None, "installation_strategies": []}
        if capabilities is not None:
            self.manifest["capabilities"]=capabilities
        manifest = self.root / "manifest.json"
        manifest.write_text(json.dumps(self.manifest), encoding="utf-8")
        self.marker = self.root / "shutdown.txt"
        self.trace = self.root / "negotiation.txt"
        env = {key: value for key, value in os.environ.items() if key.upper() in ("SYSTEMROOT", "WINDIR", "PATH", "TEMP", "TMP")}
        env["PYTHONUTF8"] = "1"
        gate_args=[] if gate_mode is None else [gate_mode,str(self.trace)]
        self.process = subprocess.Popen([sys.executable, str(REPO / "tests/fixtures/adapters/python_sdk_worker.py"), str(manifest), str(self.marker), str(workers), str(capacity),*gate_args], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=env)
        self.lines = queue.Queue()
        def read():
            for raw in self.process.stdout:
                self.lines.put(raw)
            self.lines.put(None)
        self.thread = threading.Thread(target=read, daemon=True)
        self.thread.start()
        self.fixture = json.loads((REPO / "tests/fixtures/source/protocol.json").read_text(encoding="utf-8"))

    def send(self, method, params, identity=None):
        identity = identity or str(uuid.uuid7())
        self.raw(json.dumps({"jsonrpc": "2.0", "id": identity, "method": method, "params": params}).encode())
        return identity

    def raw(self, value):
        self.process.stdin.write(value + b"\n")
        self.process.stdin.flush()

    def receive(self):
        raw = self.lines.get(timeout=15)
        if raw is None:
            raise AssertionError("el worker terminó sin responder")
        frame = json.loads(raw)
        self.contracts.frame("progress" if "method" in frame else "response", frame)
        return frame

    def handshake(self, version="1.0"):
        nonce = uuid.uuid4().hex
        self.send("core.handshake", {"protocol_version": version, "adapter_id": self.manifest["id"], "nonce": nonce})
        response = self.receive()
        if version == "1.0":
            assert response["result"]["nonce"] == nonce
            self.contracts.response("core.handshake", response["result"])
        return response

    def search(self, q="llama", **over):
        return self.send("source.search", self.fixture["requests"]["source.search"] | {"q": q} | over)

    def close(self):
        try:
            if self.process.poll() is None:
                self.process.stdin.close()
                self.process.wait(timeout=15)
            assert self.process.returncode == 0
            stderr = self.process.stderr.read()
            assert not stderr, stderr[:100]
            assert self.marker.read_text(encoding="utf-8") == "shutdown"
        finally:
            if self.process.poll() is None:
                self.process.kill()
                self.process.wait(timeout=10)
            self.thread.join(timeout=2)
            self.process.stdout.close()
            self.process.stderr.close()
            assert self.root.resolve().is_relative_to((REPO / "docs/reports/P025_runs").resolve())
            self.temp.cleanup()


class PythonSDK(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        (REPO / "docs/reports/P025_runs").mkdir(exist_ok=True)
        cls.contracts = Contracts(REPO / "docs")

    def worker(self, **args):
        worker = Worker(self.contracts, **args)
        self.addCleanup(worker.close)
        return worker

    def test_handshake_required_incompatible_then_valid_and_roundtrip(self):
        w = self.worker()
        w.search()
        self.assertEqual(w.receive()["error"]["data"]["details"]["reason"], "handshake_required")
        self.assertEqual(w.handshake("2.0")["error"]["data"]["code"], "PROTOCOL_ERROR")
        w.handshake()
        identity = w.search()
        response = w.receive()
        self.assertEqual(response["id"], identity)
        self.assertEqual(response["result"], w.fixture["responses"]["source.search"])

    def test_malformed_utf8_duplicate_keys_nonfinite_and_oversize_recover(self):
        w = self.worker()
        for raw in (b"{", b"\xff", b'{"jsonrpc":"2.0","jsonrpc":"2.0"}', b'{"n":NaN}', b"x" * ((1 << 20) + 100), b"\r" * ((1 << 20) + 100)):
            w.raw(raw)
            self.assertEqual(w.receive()["error"]["code"], -32700)
        w.raw(b"[" * 2000 + b"]" * 2000)
        self.assertIn(w.receive()["error"]["code"], (-32700, -32600))
        w.raw(b'{"jsonrpc":"2.0","id":123,"method":"core.heartbeat"}')
        invalid = w.receive()
        self.assertIsNone(invalid["id"])
        self.assertEqual(invalid["error"]["code"], -32600)
        w.handshake()
        w.send("core.heartbeat", {})
        self.assertEqual(w.receive()["result"], {"alive": True, "running_request_ids": []})

    def test_invalid_params_unsupported_reply_exception_and_error_do_not_leak(self):
        w = self.worker()
        w.handshake()
        w.search(limit=0)
        self.assertEqual(w.receive()["error"]["code"], -32602)
        w.send("not.announced", {})
        unsupported = w.receive()["error"]
        self.assertEqual(unsupported["code"], -32601)
        self.assertEqual(unsupported["data"]["code"], "UNSUPPORTED_CAPABILITY")
        for value, reason in (("bad-reply", "invalid_adapter_reply"), ("oversized", "frame_too_large"), ("exception", "adapter_callback_failed"), ("invalid-error", "invalid_adapter_error")):
            w.search(value)
            error = w.receive()
            self.assertEqual(error["error"]["data"]["details"]["reason"], reason)
            self.assertNotIn("fixture-value-that-must-not-leak", json.dumps(error))

    def test_cancel_progress_heartbeat_and_duplicate_keep_original_call(self):
        w = self.worker()
        w.handshake()
        slow = w.search("slow")
        self.assertEqual(w.receive()["params"]["request_id"], slow)
        w.send("core.heartbeat", {})
        self.assertIn(slow, w.receive()["result"]["running_request_ids"])
        w.send("source.search", w.fixture["requests"]["source.search"], slow)
        self.assertEqual(w.receive()["error"]["data"]["details"]["reason"], "request_id_in_use")
        w.send("core.heartbeat", {}, slow)
        self.assertEqual(w.receive()["error"]["data"]["details"]["reason"], "request_id_in_use")
        cancel = w.send("core.cancel", {"request_id": slow})
        responses = [w.receive(), w.receive()]
        by_id = {r["id"]: r for r in responses}
        self.assertTrue(by_id[cancel]["result"]["cancelled"])
        self.assertEqual(by_id[slow]["error"]["data"]["details"]["reason"], "operation_cancelled")

    def test_bounded_queue_rejects_without_losing_cancellation(self):
        w = self.worker(workers=1, capacity=0)
        w.handshake()
        identity = w.search("slow")
        w.receive()
        w.search()
        self.assertEqual(w.receive()["error"]["data"]["details"]["reason"], "adapter_queue_full")
        w.send("core.cancel", {"request_id": identity})
        self.assertEqual(len([w.receive(), w.receive()]), 2)

    def test_eof_cancels_active_callback_and_runs_shutdown(self):
        w = self.worker()
        w.handshake()
        identity = w.search("slow")
        w.receive()
        w.process.stdin.close()
        w.process.wait(timeout=15)
        response = w.receive()
        self.assertEqual(response["id"], identity)
        self.assertEqual(response["error"]["data"]["details"]["reason"], "operation_cancelled")

    def test_queued_cancellation_does_not_invoke_the_callback(self):
        w = self.worker(workers=1, capacity=1)
        w.handshake()
        active = w.search("slow")
        w.receive()  # primer callback activo y ocupando el único slot
        queued = w.search("exception")  # lanzaría INTERNAL si llegase al callback
        queued_cancel = w.send("core.cancel", {"request_id": queued})
        self.assertEqual(w.receive(), {"jsonrpc": "2.0", "id": queued_cancel, "result": {"cancelled": True}})
        active_cancel = w.send("core.cancel", {"request_id": active})
        results = {r["id"]: r for r in (w.receive(), w.receive(), w.receive())}
        self.assertTrue(results[active_cancel]["result"]["cancelled"])
        for identity in (active, queued):
            self.assertEqual(results[identity]["error"]["data"]["details"]["reason"], "operation_cancelled")

    def test_manifest_family_and_source_requests_share_normative_schemas(self):
        w = self.worker()
        self.contracts.manifest(w.manifest)
        for method, value in w.fixture["requests"].items():
            self.contracts.request(method, value)
            for key in value:
                bad = copy.deepcopy(value)
                del bad[key]
                with self.assertRaises(ContractViolation, msg=f"{method}.{key}"):
                    self.contracts.request(method, bad)
        bad = copy.deepcopy(w.manifest)
        bad["methods"].append("runtime.start")
        with self.assertRaises(ContractViolation):
            self.contracts.manifest(bad)
        bad = copy.deepcopy(w.manifest)
        bad["methods"].remove("source.search")
        with self.assertRaises(ContractViolation):
            self.contracts.manifest(bad)

    def test_all_p020_documents_keep_the_independent_schema_verdict(self):
        # Reutiliza fixtures compartidas, no reescribe el schema en el test.
        examples = REPO / "docs/adapters/examples"
        names = {"frame.request": "request", "frame.response": "response", "frame.progress": "progress"}
        observed = 0
        for group, expected in (("valid", True), ("invalid", False)):
            for path in (examples / group).glob("*.json"):
                name = path.name.split("--")[0]
                value = json.loads(path.read_text(encoding="utf-8"))
                try:
                    if name in names:
                        self.contracts.frame(names[name], value)
                    else:
                        self.contracts.validate(f"larm.schema.json#/$defs/{name}", value)
                    valid = True
                except ContractViolation:
                    valid = False
                self.assertEqual(valid, expected, path.name)
                observed += 1
        self.assertEqual(observed, 24)


if __name__ == "__main__":
    unittest.main(verbosity=2)
