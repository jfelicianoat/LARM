#!/usr/bin/env python3
"""P020 - Prueba real del protocolo de adaptadores v1 con un proceso en el PC IA.

  python tools/p020_real.py stdio

Python hace de host independiente: lanza `demo_adapter.exe` (adaptador
benchmark sobre el SDK) por stdio y habla NDJSON JSON-RPC 2.0. Cada frame se
valida con los JSON Schema generados en docs/adapters/schemas (jsonschema).

  1. manifiesto válido con package_sha256 real del ejecutable; uno con una
     estrategia desconocida hace que el adaptador salga sin escribir en stdout;
  2. petición antes del handshake: -32000 handshake_required;
  3. handshake: nonce exacto, HandshakeResult válido, capacidad desconocida
     conservada;
  4. JSON mal formado: -32700 con id null y la sesión sigue;
  5. método inexistente o de otra familia: -32601 con UNSUPPORTED_CAPABILITY;
  6. benchmark.run con progreso válido, heartbeat con el id en curso, cancel:
     cancelled=true y una única respuesta por id;
  7. todo stdout son frames; cerrar stdin termina el proceso con código 0;
  8. adaptador que anuncia la versión 2.0: el host lo rechaza antes de pedir
     nada más.
"""
import hashlib
import json
import queue
import subprocess
import sys
import threading
import uuid
from pathlib import Path

import jsonschema

REPO = Path(__file__).resolve().parent.parent
EXE = REPO / "target/debug/examples/demo_adapter.exe"
SCHEMAS = REPO / "docs/adapters/schemas"
FIXTURE = REPO / "tests/fixtures/adapters/demo-benchmark.manifest.json"
RUNS = REPO / "docs/reports/P020_runs"


def schema(name: str) -> jsonschema.Draft202012Validator:
    return jsonschema.Draft202012Validator(json.loads((SCHEMAS / f"{name}.schema.json").read_text(encoding="utf-8")))


def step(log: list, name: str, ok: bool, detail: str) -> None:
    log.append({"step": name, "result": "PASS" if ok else "FAIL", "detail": detail})
    print(f"  [{'PASS' if ok else 'FAIL'}] {name}: {detail}", flush=True)
    if not ok:
        raise AssertionError(f"{name}: {detail}")


class Adapter:
    def __init__(self, manifest: Path, *extra: str):
        self.proc = subprocess.Popen([str(EXE), "--manifest", str(manifest), *extra], stdin=subprocess.PIPE,
                                     stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        self.lines: queue.Queue = queue.Queue()
        self.raw: list[bytes] = []
        threading.Thread(target=self._read, daemon=True).start()

    def _read(self) -> None:
        for line in self.proc.stdout:
            self.raw.append(line)
            self.lines.put(line)
        self.lines.put(None)

    def send_raw(self, data: bytes) -> None:
        self.proc.stdin.write(data + b"\n")
        self.proc.stdin.flush()

    def send(self, method: str, params: dict) -> str:
        rid = str(uuid.uuid7())
        self.send_raw(json.dumps({"jsonrpc": "2.0", "id": rid, "method": method, "params": params}).encode())
        return rid

    def frame(self, timeout: float = 15) -> dict:
        line = self.lines.get(timeout=timeout)
        assert line is not None, "el adaptador cerró stdout"
        return json.loads(line)

    def response(self, timeout: float = 15) -> dict:
        while True:
            f = self.frame(timeout)
            if "method" not in f:
                return f

    def close(self) -> int:
        self.proc.stdin.close()
        return self.proc.wait(timeout=30)


def manifest_file(dest: Path, **changes) -> Path:
    m = json.loads(FIXTURE.read_text(encoding="utf-8"))
    m["package_sha256"] = hashlib.sha256(EXE.read_bytes()).hexdigest()
    m.update(changes)
    dest.write_text(json.dumps(m, indent=2), encoding="utf-8")
    return dest


def stdio() -> str:
    RUNS.mkdir(parents=True, exist_ok=True)
    log: list = []
    response_v, result_v, progress_v, manifest_v = schema("frame.response"), schema("HandshakeResult"), schema("frame.progress"), schema("AdapterManifest")
    good = manifest_file(RUNS / "manifest.json")
    bad = manifest_file(RUNS / "manifest-estrategia.json", installation_strategies=["NPM"])
    broken = subprocess.run([str(EXE), "--manifest", str(bad)], capture_output=True, input=b"", timeout=30)
    step(log, "manifiestos", not list(manifest_v.iter_errors(json.loads(good.read_text())))
         and list(manifest_v.iter_errors(json.loads(bad.read_text()))) and broken.returncode == 2
         and broken.stdout == b"" and b"schema_violation" in broken.stderr,
         f"package_sha256 real {json.loads(good.read_text())['package_sha256'][:12]}…; estrategia NPM rechazada por "
         "schema y por el adaptador (exit 2, stdout vacío, motivo en stderr)")

    a = Adapter(good)
    early = a.send("benchmark.describe", {})
    r = a.response()
    step(log, "handshake-obligatorio", r["id"] == early and r["error"]["code"] == -32000
         and r["error"]["data"]["details"]["reason"] == "handshake_required" and not list(response_v.iter_errors(r)),
         "benchmark.describe antes del handshake: -32000 handshake_required")

    nonce = uuid.uuid4().hex + uuid.uuid4().hex
    hid = a.send("core.handshake", {"protocol_version": "1.0", "adapter_id": "larm.demo-benchmark", "nonce": nonce})
    r = a.response()
    hr = r.get("result", {})
    step(log, "handshake", r["id"] == hid and hr.get("nonce") == nonce and not list(result_v.iter_errors(hr))
         and "vendor.demo.experimental" in hr["capabilities"] and hr["adapter_version"] == "0.1.0",
         "nonce exacto, HandshakeResult válido por schema, capacidad desconocida conservada")

    a.send_raw(b'{"jsonrpc": "2.0", "id": ')
    r = a.response()
    alive = a.send("core.heartbeat", {})
    r2 = a.response()
    step(log, "json-mal-formado", r["id"] is None and r["error"]["code"] == -32700 and not list(response_v.iter_errors(r))
         and r2["id"] == alive and r2["result"]["alive"] is True,
         "frame roto: -32700 con id null; la sesión sigue (heartbeat responde)")

    codes = []
    for method in ("runtime.teleport", "runtime.health", "source.search"):
        rid = a.send(method, {})
        r = a.response()
        codes.append((r["id"] == rid, r["error"]["code"], r["error"]["data"]["code"]))
    step(log, "metodos-no-anunciados", all(c == (True, -32601, "UNSUPPORTED_CAPABILITY") for c in codes),
         "inexistente y de otras familias: -32601 con UNSUPPORTED_CAPABILITY, nunca éxito vacío")

    fixture_id = str(uuid.uuid7())
    run = a.send("benchmark.run", {"variant_id":fixture_id,"runtime_version_id":fixture_id,"backend_id":fixture_id,"launch_profile_id":fixture_id,"suite_id":"fixture.transport:500","fixture_hash":"a"*64,"settings_hash":"b"*64})
    progress = []
    while len(progress) < 3:
        f = a.frame()
        if f.get("method") == "core.progress":
            progress.append(f)
    hb = a.send("core.heartbeat", {})
    responses = {}
    while hb not in responses:
        f = a.frame()
        if "method" not in f:
            responses[f["id"]] = f
    running = responses[hb]["result"]["running_request_ids"]
    cancel = a.send("core.cancel", {"request_id": run})
    while cancel not in responses or run not in responses:
        f = a.frame()
        if "method" in f:
            progress.append(f)
            continue
        assert f["id"] not in responses, "una respuesta por id"
        responses[f["id"]] = f
    step(log, "progreso-heartbeat-cancel", all(not list(progress_v.iter_errors(p)) and p["params"]["request_id"] == run for p in progress)
         and running == [run] and responses[cancel]["result"] == {"cancelled": True}
         and responses[run]["error"]["data"]["details"]["reason"] == "request_cancelled",
         f"{len(progress)} progresos válidos con su request_id; heartbeat con el id en curso; cancel → cancelled y la "
         "ejecución termina cancelada, una respuesta por id")

    code = a.close()
    stderr = a.proc.stderr.read()
    every_frame = all(json.loads(l) and l.endswith(b"\n") for l in a.raw)
    (RUNS / "stdout.ndjson").write_bytes(b"".join(a.raw))
    step(log, "stdout-solo-frames", code == 0 and every_frame and len(a.raw) >= 10,
         f"{len(a.raw)} líneas en stdout, todas frames JSON-RPC; stdin cerrado → exit {code}; stderr aparte ({len(stderr)} B)")

    v2 = Adapter(good, "--protocol-version", "2.0")
    nonce = uuid.uuid4().hex
    v2.send("core.handshake", {"protocol_version": "1.0", "adapter_id": "larm.demo-benchmark", "nonce": nonce})
    hr = v2.response()
    rejected = hr.get("error", {}).get("data", {}).get("details", {}).get("reason") == "invalid_adapter_reply"
    code = v2.close()
    step(log, "version-incompatible", bool(rejected) and code == 0,
         "el handler intenta anunciar 2.0: el SDK normaliza el reply inválido y no habilita la sesión")
    (RUNS / "stdio-steps.json").write_text(json.dumps(log, indent=2, ensure_ascii=False), encoding="utf-8")
    return f"{len(log)} pasos PASS"


if __name__ == "__main__":
    sys.stdout.reconfigure(encoding="utf-8")
    try:
        print("PASS " + {"stdio": stdio}[sys.argv[1]]())
        raise SystemExit(0)
    except (AssertionError, KeyError, OSError, ValueError, TypeError, queue.Empty, subprocess.SubprocessError) as e:
        print(f"FAIL {type(e).__name__}: {e}")
        raise SystemExit(1)
