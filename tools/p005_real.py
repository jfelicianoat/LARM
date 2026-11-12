#!/usr/bin/env python3
"""P005 - Prueba real de la API administrativa sobre el daemon en Windows.

  python tools/p005_real.py serve      API servida en un puerto de prueba (docs/reports/P005_runs/real)
  python tools/p005_real.py bind       sin remote_enabled, un bind no loopback no arranca
  python tools/p005_real.py openapi    docs/api/openapi.json al día y válido (openapi-spec-validator)
"""
import json
import os
import shutil
import signal
import socket
import subprocess
import sys
import time
import urllib.error
import urllib.request
import uuid
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
EXE = REPO / "target/debug/larm-daemon.exe"
ROOT = REPO / "docs/reports/P005_runs"


def clean_env() -> dict:
    return {k: v for k, v in os.environ.items() if not k.startswith("LARM_")}


def cli(root: Path, *args: str) -> dict:
    p = subprocess.run([str(EXE), "--data-root", str(root), *args], capture_output=True, text=True,
                       encoding="utf-8", env=clean_env(), timeout=60, check=False)
    if p.returncode != 0:
        raise AssertionError(f"{args[0]}: {p.stderr[-300:]}")
    return json.loads(p.stdout)


def free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def http(method: str, url: str, token: str | None = None, body: bytes | None = None,
         headers: dict | None = None) -> tuple[int, dict, dict | None]:
    req = urllib.request.Request(url, data=body, method=method)
    if token:
        req.add_header("authorization", f"Bearer {token}")
    for k, v in (headers or {}).items():
        req.add_header(k, v)
    try:
        with urllib.request.urlopen(req, timeout=10) as r:
            raw = r.read()
            return r.status, dict(r.headers), json.loads(raw) if raw else None
    except urllib.error.HTTPError as e:
        raw = e.read()
        return e.code, dict(e.headers), json.loads(raw) if raw else None


def listeners(port: int) -> list[str]:
    out = subprocess.run(["powershell", "-NoProfile", "-Command",
                          f"Get-NetTCPConnection -State Listen -LocalPort {port} | ForEach-Object {{ $_.LocalAddress }}"],
                         capture_output=True, text=True, check=False).stdout
    return sorted({line.strip() for line in out.splitlines() if line.strip()})


def step(log: list, name: str, ok: bool, detail: str) -> None:
    log.append({"step": name, "result": "PASS" if ok else "FAIL", "detail": detail})
    print(f"  [{'PASS' if ok else 'FAIL'}] {name}: {detail}")
    if not ok:
        raise AssertionError(f"{name}: {detail}")


def serve() -> str:
    root = ROOT / "real"
    shutil.rmtree(root, ignore_errors=True)
    root.mkdir(parents=True)
    log: list = []
    proc = sse = None
    try:
        reader = cli(root, "enroll-principal", "--name", "p005-reader", "--kind", "OPERATOR", "--scopes", "read")["token"]
        writer = cli(root, "enroll-principal", "--name", "p005-writer", "--kind", "OPERATOR",
                     "--scopes", "credentials:write,credentials:read")["token"]
        port = free_port()
        base = f"http://127.0.0.1:{port}"
        env = clean_env() | {"LARM_LOG_FORMAT": "json"}
        proc = subprocess.Popen([str(EXE), "--data-root", str(root), "--port", str(port), "--serve-api"], env=env,
                                creationflags=subprocess.CREATE_NEW_PROCESS_GROUP,
                                stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        for _ in range(100):
            try:
                if http("GET", f"{base}/api/v1/health/live")[0] == 200:
                    break
            except OSError:
                time.sleep(0.3)
        status, _, body = http("GET", f"{base}/api/v1/health/live")
        step(log, "live", status == 200 and body == {"status": "LIVE", "schema_version": "1"}, f"{status} {body}")
        addresses = listeners(port)
        step(log, "bind-loopback", addresses == ["127.0.0.1"], f"escucha en {addresses}")
        s1 = http("GET", f"{base}/api/v1/health/ready")
        s2 = http("GET", f"{base}/api/v1/health/ready", reader)
        step(log, "ready", s1[0] == 401 and s2[0] == 200 and s2[2]["status"] == "READY",
             f"sin token {s1[0]}; read {s2[0]} {s2[2]}")
        corr = str(uuid.uuid7()) if hasattr(uuid, "uuid7") else None
        hdrs = {"x-correlation-id": corr} if corr else {}
        st, h, b = http("GET", f"{base}/api/v1/runtimes", reader, headers=hdrs)
        keys = sorted(b) if b else []
        step(log, "envelope", st == 404 and keys == ["code", "correlation_id", "details", "message", "retryable"]
             and b["details"]["reason"] == "operation_not_implemented" and h.get("x-correlation-id") == b["correlation_id"]
             and (corr is None or b["correlation_id"] == corr),
             f"{st} {b['code']}/{b['details']['reason']}; correlación {'propagada' if corr else 'generada'}")
        st, _, b = http("POST", f"{base}/api/v1/credentials", writer, b"x" * (2 * 1024 * 1024),
                        {"content-type": "application/json", "idempotency-key": "grande"})
        step(log, "body-limit", st == 400 and b["details"]["reason"] == "body_too_large", f"2 MiB -> {st} {b['details']['reason']}")
        st, h, _ = http("GET", f"{base}/api/v1/health/live", headers={"origin": "http://evil.example"})
        step(log, "cors-off", "access-control-allow-origin" not in {k.lower() for k in h}, "sin Access-Control-Allow-Origin")

        sse = socket.create_connection(("127.0.0.1", port), timeout=10)
        sse.sendall(f"GET /api/v1/events HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer {reader}\r\n\r\n".encode())
        head = sse.recv(4096).decode("utf-8", "replace")
        body = json.dumps({"service": "larm/test/p005", "username": "u", "secret_type": "TOKEN",
                           "secret": "s3cr3t-p005-" + uuid.uuid4().hex, "scopes": []}).encode()
        created = http("POST", f"{base}/api/v1/credentials", writer, body,
                       {"content-type": "application/json", "idempotency-key": "p005-evento"})
        deadline, received = time.monotonic() + 5, head
        while time.monotonic() < deadline and "credential.created" not in received:
            try:
                received += sse.recv(4096).decode("utf-8", "replace")
            except socket.timeout:
                break
        step(log, "sse-evento-real", "text/event-stream" in head and created[0] == 201 and "event: credential.created" in received
             and created[2]["id"] in received and "s3cr3t-p005" not in received,
             "POST /credentials -> evento credential.created por SSE (sin secreto)")
        st, _, _ = http("DELETE", f"{base}/api/v1/credentials/{created[2]['id']}", writer, headers={"if-match": "1"})

        os.kill(proc.pid, signal.CTRL_BREAK_EVENT)
        started = time.monotonic()
        rc = proc.wait(timeout=20)
        took = time.monotonic() - started
        proc = None
        step(log, "apagado-con-sse-abierto", rc == 0 and took < 10, f"exit {rc} en {took:.1f}s con un cliente SSE conectado")
        logs = "".join(p.read_text(encoding="utf-8", errors="replace") for p in (root / "logs").glob("*.log"))
        lines = [json.loads(ln) for ln in logs.splitlines() if ln.strip()]
        reqs = [ln for ln in lines if ln.get("fields", {}).get("message") == "petición"]
        step(log, "log-peticiones", len(reqs) >= 8 and all("http.status" in r["fields"] for r in reqs)
             and reader not in logs and writer not in logs and "s3cr3t-p005" not in logs,
             f"{len(reqs)} peticiones registradas sin tokens ni cuerpos")
    finally:
        if sse:
            sse.close()
        if proc is not None and proc.poll() is None:
            proc.kill()
        for name in ("p005-reader", "p005-writer", "desktop"):
            try:
                cli(root, "revoke-principal", "--name", name)
            except AssertionError:
                pass
        (root / "steps.json").write_text(json.dumps(log, indent=2, ensure_ascii=False), encoding="utf-8")
    return f"{len(log)} pasos PASS"


def bind() -> str:
    root = ROOT / "bind"
    shutil.rmtree(root, ignore_errors=True)
    root.mkdir(parents=True)
    port = free_port()
    p = subprocess.run([str(EXE), "--data-root", str(root), "--bind", "0.0.0.0", "--port", str(port), "--serve-api"],
                       env=clean_env(), capture_output=True, text=True, encoding="utf-8", timeout=30, check=False)
    logs = "".join(f.read_text(encoding="utf-8", errors="replace") for f in (root / "logs").glob("*.log"))
    if p.returncode != 1 or "no es loopback" not in logs + p.stdout + p.stderr:
        raise AssertionError(f"rc={p.returncode}: {(p.stdout + p.stderr)[-300:]}")
    if listeners(port):
        raise AssertionError("quedó algo escuchando")
    return "bind 0.0.0.0 sin remote_enabled: exit 1 «no es loopback» y nada escuchando"


def openapi() -> str:
    check = subprocess.run([str(EXE), "openapi", "--out", str(REPO / "docs/api/openapi.json"), "--check"],
                           capture_output=True, text=True, encoding="utf-8", check=False)
    if check.returncode != 0:
        raise AssertionError(check.stderr)
    from openapi_spec_validator import validate
    doc = json.loads((REPO / "docs/api/openapi.json").read_text(encoding="utf-8"))
    validate(doc)
    ops = [o for p in doc["paths"].values() for o in p.values()]
    return (f"al día y válido OpenAPI {doc['openapi']}: {len(ops)} operaciones, "
            f"{sum(o['x-larm-implemented'] for o in ops)} implementadas, {len(doc['components']['schemas'])} schemas")


if __name__ == "__main__":
    sys.stdout.reconfigure(encoding="utf-8")
    try:
        print("PASS " + {"serve": serve, "bind": bind, "openapi": openapi}[sys.argv[1]]())
        raise SystemExit(0)
    except (AssertionError, KeyError, IndexError, OSError, json.JSONDecodeError, subprocess.TimeoutExpired, Exception) as e:
        print(f"FAIL {type(e).__name__}: {e}")
        raise SystemExit(1)
