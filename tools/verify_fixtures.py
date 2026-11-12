#!/usr/bin/env python3
"""P007 - Verifica los fixtures comunes contra procesos reales (exit 0 = PASS).

  jsonrpc   tests/fixtures/workers/jsonrpc_fake.py: handshake, heartbeat, progreso,
            cancelación, -32601/-32700, y cada modo de fallo (crash, hang, nonce,
            versión, basura, frame > 1 MiB, inundación de stderr)
  http      tests/fixtures/http/http_fake.py: JSON, Range/206/416, ETag/304, 429 con
            Retry-After, HEAD con Content-Length, corte a mitad del cuerpo y registro
            de peticiones sin valores de Authorization
  sparse    tools/make_sparse.py: 8 GiB lógicos con < 1 MiB en disco; denso exacto
"""
import http.client
import json
import socket
import subprocess
import sys
import tempfile
import time
import uuid
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
WORKER = REPO / "tests/fixtures/workers/jsonrpc_fake.py"
HTTP = REPO / "tests/fixtures/http/http_fake.py"


def rid() -> str:
    return str(uuid.uuid7()) if hasattr(uuid, "uuid7") else str(uuid.uuid4())


class Worker:
    def __init__(self, *args: str):
        self.p = subprocess.Popen([sys.executable, str(WORKER), *args], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                  stderr=subprocess.DEVNULL, text=True, encoding="utf-8", bufsize=1)

    def send(self, method: str, params: dict, ident: str | None = None) -> str:
        ident = ident or rid()
        assert self.p.stdin
        self.p.stdin.write(json.dumps({"jsonrpc": "2.0", "id": ident, "method": method, "params": params}) + "\n")
        self.p.stdin.flush()
        return ident

    def read(self, timeout: float = 5) -> dict | None:
        import threading
        out: list = []
        t = threading.Thread(target=lambda: out.append(self.p.stdout.readline() if self.p.stdout else ""), daemon=True)
        t.start()
        t.join(timeout)
        if not out or not out[0]:
            return None
        try:
            return json.loads(out[0])
        except json.JSONDecodeError:
            return {"raw": out[0].strip()}

    def close(self) -> int:
        if self.p.stdin:
            self.p.stdin.close()
        try:
            return self.p.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.p.kill()
            return -9


def jsonrpc() -> str:
    checks = []
    w = Worker()
    hid = w.send("core.handshake", {"protocol_version": "1.0", "adapter_id": "fake", "nonce": "n-1"})
    r = w.read()
    assert r and r["id"] == hid and r["result"]["nonce"] == "n-1" and "core.cancel" in r["result"]["methods"], r
    w.send("core.heartbeat", {})
    assert (w.read() or {}).get("result", {}).get("alive") is True
    job = w.send("test.work", {"steps": 3, "step_ms": 20})
    frames = [w.read() for _ in range(4)]
    progress = [f for f in frames if f and f.get("method") == "core.progress"]
    assert len(progress) == 3 and frames[-1]["result"] == {"done": 3}, frames
    long = w.send("test.work", {"steps": 100, "step_ms": 100})
    first = w.read()
    assert first and first.get("method") == "core.progress"
    w.send("core.cancel", {"request_id": long})
    # Puede llegar algún core.progress más antes de la cancelación: leer hasta ver ambas respuestas.
    acked = errored = False
    for _ in range(20):
        g = w.read()
        if g is None:
            break
        acked |= g.get("result") == {"cancelled": True}
        errored |= g.get("id") == long and g.get("error", {}).get("data", {}).get("code") == "CANCELLED"
        if acked and errored:
            break
    assert acked and errored, (acked, errored)
    w.send("no.existe", {})
    assert (w.read() or {}).get("error", {}).get("code") == -32601
    assert w.p.stdin
    w.p.stdin.write("{basura\n")
    w.p.stdin.flush()
    assert (w.read() or {}).get("error", {}).get("code") == -32700
    assert w.close() == 0
    checks.append("normal")

    for mode, expect in [("bad-nonce", "nonce"), ("bad-version", "version")]:
        w = Worker("--mode", mode)
        w.send("core.handshake", {"protocol_version": "1.0", "adapter_id": "fake", "nonce": "n-2"})
        r = (w.read() or {})["result"]
        assert (r["nonce"] != "n-2") if expect == "nonce" else (r["protocol_version"] == "2.0"), r
        w.close()
        checks.append(mode)
    w = Worker("--mode", "crash-after", "--n", "1")
    w.send("core.heartbeat", {})
    w.read()
    w.send("core.heartbeat", {})
    assert w.read(2) is None and w.p.wait(timeout=5) == 3
    checks.append("crash-after")
    w = Worker("--mode", "hang")
    w.send("core.heartbeat", {})
    assert w.read(1.5) is None
    w.p.kill()
    checks.append("hang")
    w = Worker("--mode", "no-heartbeat")
    w.send("core.heartbeat", {})
    assert w.read(1.5) is None
    w.p.kill()
    checks.append("no-heartbeat")
    w = Worker("--mode", "garbage")
    w.send("core.heartbeat", {})
    assert (w.read() or {}).get("raw") == "esto no es JSON"
    w.close()
    checks.append("garbage")
    w = Worker("--mode", "oversize")
    w.send("test.work", {})
    r = w.read(10)
    assert r is not None and len(json.dumps(r)) > 1024 * 1024
    w.close()
    checks.append("oversize")
    flood = subprocess.run([sys.executable, str(WORKER), "--mode", "stderr-flood"], input="", capture_output=True, timeout=60)
    assert len(flood.stderr) > 10 * 1024 * 1024 and flood.stdout == b""
    checks.append("stderr-flood")
    return f"{len(checks)} modos verificados: {', '.join(checks)}"


def http_fake() -> str:
    with tempfile.TemporaryDirectory() as tmp:
        t = Path(tmp)
        blob = bytes(range(256)) * 40  # 10240 bytes
        (t / "model.bin").write_bytes(blob)
        routes = {
            "GET /api/models/x": {"json": {"id": "x", "siblings": []}},
            "GET /files/model.bin": {"file": str(t / "model.bin"), "etag": "\"v1\""},
            "HEAD /files/model.bin": {"file": str(t / "model.bin")},
            "GET /cut": {"file": str(t / "model.bin"), "cut_after_bytes": 1000},
            "GET /limited": {"status": 429, "headers": {"Retry-After": "2"}},
        }
        (t / "routes.json").write_text(json.dumps(routes), encoding="utf-8")
        log = t / "requests.ndjson"
        p = subprocess.Popen([sys.executable, str(HTTP), "--routes", str(t / "routes.json"), "--log", str(log)],
                             stdout=subprocess.PIPE, text=True)
        try:
            port = int((p.stdout.readline() if p.stdout else "").split()[1])

            def req(method: str, path: str, headers: dict | None = None):
                c = http.client.HTTPConnection("127.0.0.1", port, timeout=5)
                c.request(method, path, headers=headers or {})
                r = c.getresponse()
                try:
                    body = r.read()
                except http.client.IncompleteRead as e:
                    body = ("CORTE", len(e.partial))
                return r.status, dict(r.getheaders()), body

            s, _, b = req("GET", "/api/models/x")
            assert s == 200 and json.loads(b)["id"] == "x"
            s, h, b = req("GET", "/files/model.bin", {"Range": "bytes=100-199"})
            assert s == 206 and b == blob[100:200] and h["Content-Range"] == "bytes 100-199/10240"
            s, h, _ = req("GET", "/files/model.bin", {"Range": "bytes=99999-"})
            assert s == 416 and h["Content-Range"] == "bytes */10240"
            s, _, _ = req("GET", "/files/model.bin", {"If-None-Match": "\"v1\""})
            assert s == 304
            s, h, b = req("HEAD", "/files/model.bin")
            assert s == 200 and h["Content-Length"] == "10240" and b == b""
            s, h, _ = req("GET", "/limited")
            assert s == 429 and h["Retry-After"] == "2"
            s, _, b = req("GET", "/cut")
            assert s == 200 and isinstance(b, tuple) and b[0] == "CORTE" and b[1] == 1000, b
            req("GET", "/api/models/x", {"Authorization": "Bearer secreto-que-no-se-registra"})
            entries = [json.loads(line) for line in log.read_text(encoding="utf-8").splitlines()]
            assert entries[-1]["authorization"] is True and "secreto" not in log.read_text(encoding="utf-8")
        finally:
            p.kill()
    return "JSON, Range 206, 416, ETag 304, HEAD, 429 Retry-After, corte a 1000 bytes, registro sin secretos"


def sparse() -> str:
    with tempfile.TemporaryDirectory() as tmp:
        big = Path(tmp) / "big.bin"
        out = subprocess.run([sys.executable, str(REPO / "tools/make_sparse.py"), str(big), str(8 << 30)],
                             capture_output=True, text=True, check=False)
        info = json.loads(out.stdout)
        assert out.returncode == 0 and int(info["size_bytes"]) == 8 << 30 and int(info["on_disk_bytes"]) < 1 << 20, info
        small = Path(tmp) / "small.bin"
        dense = json.loads(subprocess.run([sys.executable, str(REPO / "tools/make_sparse.py"), str(small), "4096", "--dense"],
                                          capture_output=True, text=True, check=True).stdout)
        assert small.read_bytes() == (bytes(range(256)) * 16) and dense["sparse"] is False
    return f"8 GiB lógicos con {info['on_disk_bytes']} bytes en disco; 4 KiB denso con contenido conocido"


if __name__ == "__main__":
    sys.stdout.reconfigure(encoding="utf-8")
    try:
        print("PASS " + {"jsonrpc": jsonrpc, "http": http_fake, "sparse": sparse}[sys.argv[1]]())
        raise SystemExit(0)
    except (AssertionError, KeyError, IndexError, OSError, ValueError, subprocess.SubprocessError) as e:
        print(f"FAIL {type(e).__name__}: {e!r}")
        raise SystemExit(1)
