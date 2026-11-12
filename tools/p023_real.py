#!/usr/bin/env python3
"""P023 - Prueba real del adaptador declarativo con el daemon en el PC IA.

  python tools/p023_real.py serve

Sobre docs/reports/P023_runs/real (se rehace en cada ejecución):

  1. alta por la API real (Bearer de un principal con adapters:write): 201 con
     adapter_installation UNTRUSTED/INSTALLED; paquete en <data>/adapters con el
     hash recalculado aquí con la regla de P022 (oráculo independiente); fila y
     evento adapter.updated en SQLite; repetición idempotente;
  2. negativos: principal solo-lectura 403; variable fuera de allowlist,
     secreto como valor, referencia fuera de larm/, ejecutable fuera de raíces y
     placeholder desconocido 400 sin fila ni carpeta;
  3. exportar (documento del paquete) sin secretos e importar como otra versión:
     201 UNTRUSTED, nunca hereda confianza;
  4. el worker del paquete creado por el daemon, manejado por un cliente
     JSON-RPC escrito aquí: handshake con nonce, sondeo de versión, plan,
     arranque, health READY, la variable no secreta llega al proceso y nada del
     entorno del worker se hereda, parada SIGNAL con el puerto cerrado; una
     credential_reference sin resolver bloquea sondeo y arranque;
  5. runtime real: Ollama instalado, declarado con entorno propio (puerto,
     OLLAMA_MODELS y USERPROFILE temporales) sin tocar la instancia del usuario.
     Su `--version` sin servidor imprime avisos: el sondeo EXACT_TEXT la deja
     desconocida (no inventa versión). Arranque, health READY con /version igual
     a la que ve un oráculo aparte, y parada.
"""
import hashlib
import json
import os
import shutil
import signal
import socket
import sqlite3
import subprocess
import sys
import time
import urllib.error
import urllib.request
import uuid
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
EXE = REPO / "target/debug/larm-daemon.exe"
FAKE = REPO / "target/debug/examples/fake_runtime.exe"
RUNS = REPO / "docs/reports/P023_runs"
ADAPTER = "acme.fake-runtime"
IDS = "0192aaaa-0000-7000-8000-0000000000bb"


def clean_env() -> dict:
    return {k: v for k, v in os.environ.items() if not k.startswith("LARM_")}


def step(log: list, name: str, ok: bool, detail: str) -> None:
    log.append({"step": name, "result": "PASS" if ok else "FAIL", "detail": detail})
    print(f"  [{'PASS' if ok else 'FAIL'}] {name}: {detail}")
    if not ok:
        raise AssertionError(f"{name}: {detail}")


def http(method: str, url: str, token: str, body=None, headers: dict | None = None) -> tuple[int, dict | None]:
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(url, data=data, method=method)
    req.add_header("authorization", f"Bearer {token}")
    if data is not None:
        req.add_header("content-type", "application/json")
    for k, v in (headers or {}).items():
        req.add_header(k, v)
    try:
        with urllib.request.urlopen(req, timeout=120) as r:
            raw = r.read()
            return r.status, json.loads(raw) if raw else None
    except urllib.error.HTTPError as e:
        raw = e.read()
        return e.code, json.loads(raw) if raw else None


def free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def port_open(port: int) -> bool:
    try:
        with socket.create_connection(("127.0.0.1", port), timeout=0.3):
            return True
    except OSError:
        return False


def start(root: Path) -> tuple[subprocess.Popen, str]:
    port = free_port()
    proc = subprocess.Popen([str(EXE), "--data-root", str(root), "--port", str(port), "--serve-api"],
                            env=clean_env(), creationflags=subprocess.CREATE_NEW_PROCESS_GROUP,
                            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    base = f"http://127.0.0.1:{port}/api/v1"
    for _ in range(100):
        try:
            urllib.request.urlopen(f"{base}/health/live", timeout=2)
            return proc, base
        except OSError:
            time.sleep(0.2)
    raise AssertionError("el daemon no arrancó")


def stop(proc: subprocess.Popen) -> None:
    os.kill(proc.pid, signal.CTRL_BREAK_EVENT)
    try:
        proc.wait(timeout=30)
    except subprocess.TimeoutExpired:
        proc.kill()


def package_hash(directory: Path) -> str:
    """Regla de P022 reimplementada: sha256 de `ruta\\0sha256\\n` ordenado, sin manifest.json."""
    entries = []
    for p in directory.rglob("*"):
        if p.is_file():
            rel = p.relative_to(directory).as_posix().lower()
            if rel != "manifest.json":
                entries.append((rel, hashlib.sha256(p.read_bytes()).hexdigest()))
    entries.sort()
    return hashlib.sha256("".join(f"{r}\0{h}\n" for r, h in entries).encode()).hexdigest()


def document(run: Path, version: str, secret: bool = True) -> dict:
    exe = str(run / "rt" / "fake_runtime.exe")
    methods = ["core.handshake", "core.cancel", "core.heartbeat", "runtime.discover", "runtime.list_versions",
               "runtime.install_plan", "runtime.install", "runtime.detect_backends", "runtime.inspect_model",
               "runtime.check_compatibility", "runtime.estimate_memory", "runtime.launch_plan", "runtime.start",
               "runtime.load", "runtime.health", "runtime.stop"]
    return {
        "schema_version": "1.0",
        "manifest": {
            "id": ADAPTER, "version": version, "family": "runtime", "protocol_version": "1.0",
            "publisher": "operador local (P023 real)",
            "executable": {"relative_path": "bin/larm-generic-adapter.exe", "args": []},
            "platforms": ["windows"], "architectures": ["x86_64"], "methods": methods,
            "permissions": {"read_roots": [str(run / "rt"), str(run / "models")], "write_roots": [],
                            "network_origins": [], "process_allowlist": [], "secret_refs": ["larm/p023/hf"]},
            "capabilities": [], "package_sha256": "0" * 64, "signature": None,
            "installation_strategies": [],
        },
        "version_probe": {"executable": exe, "args": ["--version"], "parse_mode": "EXACT_TEXT", "json_field": None},
        "start": {"executable": exe, "cwd": str(run / "rt"),
                  "args": ["--serve", "--port", "{PORT}", "--model", "{MODEL_PATH}", "--threads", "{THREADS}",
                           "--dump", str(run / "out" / "dump.json")]},
        "stop": {"mode": "SIGNAL", "endpoint": None},
        "health": {"url": "http://127.0.0.1:{PORT}/health", "expected_status": 200, "json_pointer": "/status",
                   "expected_value": "ok"},
        "protocol_profile_id": "openai.chat-completions",
        "formats": ["GGUF"],
        "capabilities": [],
        "environment": {"allowlist": ["LARM_TEST", "HF_TOKEN"],
                        "variables": [{"name": "LARM_TEST", "value": "desde-la-gui"}]
                        + ([{"name": "HF_TOKEN", "credential_reference": "larm/p023/hf"}] if secret else [])},
    }


class Rpc:
    """Cliente mínimo del protocolo v1 (NDJSON JSON-RPC por stdio)."""

    def __init__(self, exe: Path, env: dict):
        self.proc = subprocess.Popen([str(exe)], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                     stderr=subprocess.DEVNULL, env=env, creationflags=0x08000000)

    def call(self, method: str, params: dict) -> dict:
        rid = str(uuid.uuid7())
        self.proc.stdin.write((json.dumps({"jsonrpc": "2.0", "id": rid, "method": method, "params": params}) + "\n").encode())
        self.proc.stdin.flush()
        while True:
            frame = json.loads(self.proc.stdout.readline())
            if frame.get("id") == rid:
                return frame

    def close(self) -> None:
        self.proc.stdin.close()
        try:
            self.proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            self.proc.kill()


def ollama_document(run: Path, exe: Path, port: int, version: str) -> dict:
    doc = document(run, "1.0.0", secret=False)
    doc["manifest"]["id"] = "local.ollama-declarativo"
    doc["manifest"]["permissions"]["read_roots"] = [str(exe.parent), str(run / "ollama")]
    doc["manifest"]["permissions"]["secret_refs"] = []
    doc["version_probe"] = {"executable": str(exe), "args": ["--version"], "parse_mode": "EXACT_TEXT", "json_field": None}
    doc["start"] = {"executable": str(exe), "args": ["serve"], "cwd": str(run / "ollama")}
    doc["health"] = {"url": f"http://127.0.0.1:{port}/api/version", "expected_status": 200,
                     "json_pointer": "/version", "expected_value": version}
    doc["formats"] = ["GGUF"]
    doc["environment"] = {"allowlist": ["OLLAMA_HOST", "OLLAMA_MODELS", "USERPROFILE"], "variables": [
        {"name": "OLLAMA_HOST", "value": f"127.0.0.1:{port}"},
        {"name": "OLLAMA_MODELS", "value": str(run / "ollama" / "models")},
        {"name": "USERPROFILE", "value": str(run / "ollama" / "home")}]}
    return doc


def ollama_oracle_version(exe: Path, run: Path) -> str:
    """Versión vista sin LARM: la línea `client version is X` de `ollama --version`."""
    env = {k: os.environ[k] for k in ("SystemRoot", "SystemDrive", "windir", "TEMP", "TMP") if k in os.environ}
    env.update({"OLLAMA_HOST": "127.0.0.1:1", "USERPROFILE": str(run / "ollama" / "home")})
    out = subprocess.run([str(exe), "--version"], capture_output=True, env=env, timeout=30).stdout.decode()
    for line in out.splitlines():
        if "version is" in line:
            return line.rsplit(" ", 1)[1].strip()
    raise AssertionError(f"el oráculo no encontró la versión de Ollama: {out!r}")


def launch_request(port: int, model: str, env: dict) -> dict:
    return {
        "installation_id": IDS, "variant_id": IDS,
        "launch_profile": {
            "threads": 3, "context": 2048, "batch": 512, "concurrency": 1, "offload_layers": None,
            "backend_id": IDS, "tokenizer_hash": None, "chat_template_hash": None,
            "generation": {"temperature": None, "top_p": None, "seed": None, "max_output_tokens": None},
            "memory_limit_bytes": None, "runtime_arguments": [], "environment": env,
            "adapter_options": {"model_path": model, "port": port},
        },
        "constraints": {"runtime_version_id": None, "backend_id": None, "execution_fingerprint": None,
                        "allow_backend_fallback": False, "max_memory_bytes": None},
        "resource_requirements": None,
    }


def serve() -> str:
    run = RUNS / "real"
    shutil.rmtree(run, ignore_errors=True)
    for d in ("rt", "models", "out"):
        (run / d).mkdir(parents=True)
    shutil.copy(FAKE, run / "rt" / "fake_runtime.exe")
    (run / "models" / "m.gguf").write_bytes(b"GGUF")
    data = run / "data"
    log: list = []
    proc = None
    rpc = None
    try:
        def enroll(name: str, scopes: str) -> str:
            out = subprocess.run([str(EXE), "--data-root", str(data), "enroll-principal", "--name", name,
                                  "--kind", "OPERATOR", "--scopes", scopes],
                                 capture_output=True, env=clean_env(), check=True).stdout
            return json.loads(out)["token"]
        writer = enroll("p023", "read,adapters:write")
        reader = enroll("p023-lector", "read")
        proc, base = start(data)
        url = f"{base}/adapters/declarative"

        # 1. Alta real.
        doc = document(run, "1.0.0", secret=False)
        status, a = http("POST", url, writer, doc, {"idempotency-key": "alta"})
        pkg = data / "adapters" / ADAPTER / "1.0.0"
        manifest = json.loads((pkg / "manifest.json").read_text(encoding="utf-8")) if pkg.exists() else {}
        recomputed = package_hash(pkg) if pkg.exists() else ""
        step(log, "alta-real", status == 201 and a["trust"] == "UNTRUSTED" and a["state"] == "INSTALLED"
             and a["metadata"]["signature_state"] == "CUSTOM" and a["package_hash"] == recomputed
             and manifest.get("package_sha256") == recomputed
             and (pkg / "bin" / "larm-generic-adapter.exe").is_file() and (pkg / "declarative.json").is_file(),
             f"201 UNTRUSTED/INSTALLED; package_hash {recomputed[:12]}… recalculado aquí sobre los ficheros")
        db = sqlite3.connect(data / "state" / "larm.sqlite")
        row = db.execute("SELECT trust, state, package_hash FROM adapter_installation WHERE id = ?", (a["id"],)).fetchone()
        events = db.execute("SELECT event_type, payload FROM event WHERE resource_id = ?", (a["id"],)).fetchall()
        step(log, "fila-y-evento", row == ("UNTRUSTED", "INSTALLED", recomputed) and len(events) == 1
             and events[0][0] == "adapter.updated" and json.loads(events[0][1])["action"] == "created",
             "adapter_installation y adapter.updated en SQLite")
        replay = http("POST", url, writer, doc, {"idempotency-key": "alta"})
        count = db.execute("SELECT count(*) FROM adapter_installation").fetchone()[0]
        step(log, "alta-idempotente", replay == (201, a) and count == 1, "misma clave y cuerpo: misma respuesta y una sola fila")
        status, err = http("POST", url, writer, doc, {"idempotency-key": "otra"})
        step(log, "version-existente", status == 409 and err["details"]["reason"] == "adapter_version_exists",
             "otra clave con la misma versión: 409 adapter_version_exists")

        # 2. Negativos sin efectos.
        status, _ = http("POST", url, reader, document(run, "5.0.0"), {"idempotency-key": "lector"})
        step(log, "scope-insuficiente", status == 403, "principal solo-lectura: 403")
        negatives = []
        d = document(run, "6.0.0"); d["environment"]["variables"].append({"name": "OTRA", "value": "x"}); negatives.append((d, "env_not_allowed"))
        d = document(run, "6.0.1"); d["environment"]["variables"][1] = {"name": "HF_TOKEN", "value": "hf_secreto_real"}; negatives.append((d, "secret_value_not_allowed"))
        d = document(run, "6.0.5"); d["environment"]["variables"][1] = {"name": "HF_TOKEN", "credential_reference": "ai-broker/token"}; negatives.append((d, "credential_reference_invalid"))
        d = document(run, "6.0.2"); d["start"]["executable"] = r"C:\Windows\System32\cmd.exe"; negatives.append((d, "path_outside_roots"))
        d = document(run, "6.0.3"); d["start"]["args"].append("{SHELL}"); negatives.append((d, "placeholder_unknown"))
        d = document(run, "6.0.4"); d["start"]["executable"] = str(run / "rt" / "run.bat"); negatives.append((d, "executable_not_exe"))
        bad = []
        for i, (d, reason) in enumerate(negatives):
            st, e = http("POST", url, writer, d, {"idempotency-key": f"neg{i}"})
            if st != 400 or e["details"]["reason"] != reason:
                bad.append(f"{reason}: {st} {e}")
        versions = sorted(p.name for p in (data / "adapters" / ADAPTER).iterdir())
        count = db.execute("SELECT count(*) FROM adapter_installation").fetchone()[0]
        step(log, "negativos-sin-efectos", not bad and versions == ["1.0.0"] and count == 1,
             bad[0] if bad else "6 documentos rechazados con 400 y su motivo; ni fila ni carpeta")

        # 3. Alta con referencia de credencial, exportar e importar.
        status, c = http("POST", url, writer, document(run, "2.0.0"), {"idempotency-key": "con-secreto"})
        step(log, "alta-con-referencia", status == 201 and c["trust"] == "UNTRUSTED", "entorno con valor no secreto y credential_reference")
        secret_pkg = data / "adapters" / ADAPTER / "2.0.0"
        exported = json.loads((secret_pkg / "declarative.json").read_text(encoding="utf-8"))
        text = json.dumps(exported)
        hf = [v for v in exported["environment"]["variables"] if v["name"] == "HF_TOKEN"]
        step(log, "exportacion-sin-secretos", "hf_secreto" not in text and "larm/p023/hf" in text
             and hf == [{"name": "HF_TOKEN", "credential_reference": "larm/p023/hf"}]
             and {"name": "LARM_TEST", "value": "desde-la-gui"} in exported["environment"]["variables"],
             "el documento exportado lleva el valor no secreto y solo la referencia del secreto")
        exported["manifest"]["version"] = "2.0.1"
        status, b = http("POST", url, writer, exported, {"idempotency-key": "importar"})
        imported = data / "adapters" / ADAPTER / "2.0.1" / "declarative.json"
        same_env = imported.is_file() and json.loads(imported.read_text(encoding="utf-8"))["environment"] == exported["environment"]
        step(log, "importacion-sin-confianza", status == 201 and b["trust"] == "UNTRUSTED" and b["id"] != c["id"] and same_env,
             "importar = alta nueva UNTRUSTED con el mismo entorno")
        db.close()

        # 4. El worker del paquete del daemon, con un cliente JSON-RPC independiente.
        worker_env = {k: os.environ[k] for k in ("SystemRoot", "SystemDrive", "windir", "TEMP", "TMP") if k in os.environ}
        worker_env["LARM_P023_FUGA"] = "no-debe-llegar"
        rpc = Rpc(pkg / "bin" / "larm-generic-adapter.exe", worker_env)
        nonce = uuid.uuid4().hex
        hs = rpc.call("core.handshake", {"protocol_version": "1.0", "adapter_id": ADAPTER, "nonce": nonce})
        step(log, "handshake", hs.get("result", {}).get("nonce") == nonce
             and hs["result"]["adapter_version"] == "1.0.0" and "runtime.start" in hs["result"]["methods"],
             "nonce exacto y métodos del manifiesto")
        page = rpc.call("runtime.list_versions", {"runtime_id": IDS, "channel": "custom", "cursor": None})
        item = page.get("result", {}).get("items", [{}])[0]
        real_hash = hashlib.sha256((run / "rt" / "fake_runtime.exe").read_bytes()).hexdigest()
        step(log, "sondeo-de-version", item.get("version_string") == "fake-runtime 1.2.3" and item.get("channel") == "custom"
             and item.get("artifact_hash") == real_hash, "fake-runtime 1.2.3, canal custom, sha256 real del ejecutable")
        port = free_port()
        model = str(run / "models" / "m.gguf")
        plan = rpc.call("runtime.launch_plan", launch_request(port, model, {"allowlist": ["LARM_TEST"], "variables": []}))["result"]
        handle = rpc.call("runtime.start", {"plan_id": plan["plan_id"], "plan_hash": plan["plan_hash"], "approval_id": None})["result"]
        ref = {"instance_id": handle["instance_id"], "generation": handle["generation"]}
        state = None
        for _ in range(100):
            state = rpc.call("runtime.health", ref)["result"]
            if state["state"] == "READY":
                break
            time.sleep(0.1)
        step(log, "arranque-y-health", state["state"] == "READY" and plan["env_allowlist"] == ["LARM_TEST"],
             f"plan de un solo uso, proceso pid {handle['pid']}, health READY")
        dump = json.loads((run / "out" / "dump.json").read_text(encoding="utf-8"))
        allowed = {"systemroot", "systemdrive", "windir", "number_of_processors", "processor_architecture", "temp", "tmp", "larm_test"}
        extra = [k for k in dump["env"] if k.lower() not in allowed]
        step(log, "variable-llega-al-proceso", dump["env"].get("LARM_TEST") == "desde-la-gui" and not extra
             and "LARM_P023_FUGA" not in dump["env"] and "HF_TOKEN" not in dump["env"]
             and dump["args"][:4] == ["--serve", "--port", str(port), "--model"]
             and Path(dump["args"][4]) == Path(model).resolve() and dump["args"][5:7] == ["--threads", "3"]
             and Path(dump["cwd"]).resolve() == (run / "rt").resolve(),
             "LARM_TEST=desde-la-gui en el proceso; ni la variable del worker ni el secreto; args expandidos")
        stopped = rpc.call("runtime.stop", ref)["result"]
        step(log, "parada", stopped == {"stopped": True} and not port_open(port), "SIGNAL: árbol terminado y puerto cerrado")
        rpc.close()
        # Una referencia sin resolver bloquea sondeo y arranque (P045).
        rpc = Rpc(secret_pkg / "bin" / "larm-generic-adapter.exe", worker_env)
        rpc.call("core.handshake", {"protocol_version": "1.0", "adapter_id": ADAPTER, "nonce": "n"})
        (run / "out" / "dump.json").unlink()
        probe = rpc.call("runtime.list_versions", {"runtime_id": IDS, "channel": "custom", "cursor": None})
        port = free_port()
        plan = rpc.call("runtime.launch_plan", launch_request(port, model, {"allowlist": ["LARM_TEST", "HF_TOKEN"], "variables": []}))["result"]
        started = rpc.call("runtime.start", {"plan_id": plan["plan_id"], "plan_hash": plan["plan_hash"], "approval_id": None})
        step(log, "referencia-sin-resolver-bloquea",
             probe.get("error", {}).get("data", {}).get("details", {}).get("reason") == "credential_not_enrolled"
             and started.get("error", {}).get("data", {}).get("details", {}).get("reason") == "credential_not_enrolled"
             and "larm/p023/hf" in json.dumps(plan) and not (run / "out" / "dump.json").exists() and not port_open(port),
             "sondeo y arranque credential_not_enrolled; el plan solo lleva la referencia; ningún proceso")
        rpc.close()
        rpc = None

        # 5. Runtime real: Ollama aislado.
        exe = Path(shutil.which("ollama") or "")
        step(log, "ollama-instalado", exe.is_file(), f"ollama.exe encontrado ({exe.name})")
        for d in ("models", "home"):
            (run / "ollama" / d).mkdir(parents=True)
        version = ollama_oracle_version(exe, run)
        user_before = port_open(11434)
        oport = free_port()
        status, o = http("POST", url, writer, ollama_document(run, exe, oport, version), {"idempotency-key": "ollama"})
        step(log, "ollama-alta", status == 201 and o["trust"] == "UNTRUSTED", f"declarativo de Ollama {version} registrado")
        opkg = data / "adapters" / "local.ollama-declarativo" / "1.0.0"
        rpc = Rpc(opkg / "bin" / "larm-generic-adapter.exe", worker_env)
        rpc.call("core.handshake", {"protocol_version": "1.0", "adapter_id": "local.ollama-declarativo", "nonce": "o"})
        probe = rpc.call("runtime.list_versions", {"runtime_id": IDS, "channel": "custom", "cursor": None})
        step(log, "ollama-version-desconocida",
             probe.get("error", {}).get("data", {}).get("details", {}).get("reason") == "version_unparseable",
             "--version sin servidor imprime dos avisos: version_unparseable, no «Warning: …» como versión")
        oenv = {"allowlist": ["OLLAMA_HOST", "OLLAMA_MODELS", "USERPROFILE"], "variables": []}
        req = launch_request(oport, model, oenv)
        req["launch_profile"]["adapter_options"] = {}
        plan = rpc.call("runtime.launch_plan", req)["result"]
        handle = rpc.call("runtime.start", {"plan_id": plan["plan_id"], "plan_hash": plan["plan_hash"], "approval_id": None})["result"]
        ref = {"instance_id": handle["instance_id"], "generation": handle["generation"]}
        state = None
        for _ in range(240):
            state = rpc.call("runtime.health", ref)["result"]
            if state["state"] == "READY":
                break
            time.sleep(0.25)
        step(log, "ollama-arranque-y-health", state["state"] == "READY" and plan["port"] == oport,
             f"ollama serve en 127.0.0.1:{oport} con entorno declarado; /api/version = {version} (oráculo)")
        stopped = rpc.call("runtime.stop", ref)["result"]
        time.sleep(0.5)
        step(log, "ollama-parada", stopped == {"stopped": True} and not port_open(oport)
             and port_open(11434) == user_before,
             f"árbol terminado y puerto cerrado; la instancia del usuario sigue {'arriba' if user_before else 'parada'}")
    finally:
        if rpc is not None:
            rpc.close()
        if proc is not None:
            stop(proc)
        for name in ("p023", "p023-lector"):
            subprocess.run([str(EXE), "--data-root", str(data), "revoke-principal", "--name", name], capture_output=True, env=clean_env())
        RUNS.mkdir(parents=True, exist_ok=True)
        (RUNS / "serve-steps.json").write_text(json.dumps(log, indent=2, ensure_ascii=False), encoding="utf-8")
    return f"{len(log)} pasos PASS"


if __name__ == "__main__":
    sys.stdout.reconfigure(encoding="utf-8")
    try:
        print("PASS " + {"serve": serve}[sys.argv[1]]())
        raise SystemExit(0)
    except (AssertionError, KeyError, OSError, ValueError, TypeError, IndexError, subprocess.SubprocessError, sqlite3.Error) as e:
        print(f"FAIL {type(e).__name__}: {e}")
        raise SystemExit(1)
