#!/usr/bin/env python3
"""Referencia protocol autónoma con backend echo Rust y fallos HTTP controlados."""
import copy
import hashlib
import http.client
import http.server
import importlib.util
import json
import platform
import socket
import subprocess
import sys
import threading
import time
import uuid
from datetime import datetime, timezone
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(REPO / "tools/adapter-conformance"))
from runner import common, executable  # noqa: E402
from session import Session  # noqa: E402


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


class Proxy(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_args):
        pass
    def do_GET(self):
        self.handle_http("GET")
    def do_POST(self):
        self.handle_http("POST")
    def handle_http(self, method):
        body = self.rfile.read(int(self.headers.get("Content-Length", "0")))
        self.server.requests.append({"method":method,"path":self.path,"body_sha256":hashlib.sha256(body).hexdigest(),"authorization":self.headers.get("Authorization")})
        mode = self.server.mode if method == "POST" else "normal"
        self.server.received.set()
        status, content_type = 200, "application/json"
        extra = {}
        if mode == "auth":
            status, result = 401, b'{}'
        elif mode == "rate":
            status, result, extra = 429, b'{}', {"Retry-After":"900"}
        elif mode == "redirect":
            status, result, extra = 302, b'{}', {"Location":f"http://127.0.0.1:{self.server.server_port}/redirect-not-followed"}
        elif mode == "invalid":
            result = b'{"echo":'
        elif mode == "wrong":
            result = b'{"echo":{"private":"private-upstream-fixture"}}'
        elif mode == "large":
            result = b' ' * (40 << 10)
        else:
            connection = http.client.HTTPConnection("127.0.0.1", self.server.backend_port, timeout=2)
            try:
                connection.request(method, self.path, body=body, headers={"Content-Type":"application/json"})
                response = connection.getresponse()
                status, result = response.status, response.read((1 << 20) + 1)
            except OSError:
                status, result = 503, b'{}'
            finally:
                connection.close()
        if mode == "held":
            self.server.release.wait(timeout=3)
        if mode == "type":
            content_type = "text/plain"
        try:
            self.send_response(status)
            self.send_header("Content-Type", content_type)
            self.send_header("Content-Length", str(len(result) + (7 if mode=="length" else 0)))
            for name, value in extra.items(): self.send_header(name, value)
            self.end_headers()
            self.wfile.write(result)
        except OSError:
            pass


def main():
    run = REPO / "docs/reports/P026_runs/protocol-real" / uuid.uuid4().hex[:12]
    run.mkdir(parents=True)
    with socket.socket() as reserve:
        reserve.bind(("127.0.0.1",0)); backend_port = reserve.getsockname()[1]
    server = http.server.ThreadingHTTPServer(("127.0.0.1",0), Proxy)
    server.backend_port, server.requests, server.mode = backend_port, [], "normal"
    server.release, server.received = threading.Event(), threading.Event()
    thread = threading.Thread(target=server.serve_forever,daemon=True); thread.start()
    factory_spec = importlib.util.spec_from_file_location("protocol_reference_factory", REPO / "adapters/protocol/reference/package.py")
    factory = importlib.util.module_from_spec(factory_spec); factory_spec.loader.exec_module(factory)
    package, work = run / "package", run / "work"
    manifest, profile, entry = factory.create(REPO, package, work, f"http://127.0.0.1:{server.server_port}")
    initial = {p.relative_to(package).as_posix():sha(p) for p in package.rglob("*") if p.is_file()}
    session, backend, success, steps = None, None, False, []

    def step(name, condition, detail):
        assert condition, name
        steps.append({"id":name,"result":"PASS","detail":detail}); print(f"PASS {name}",flush=True)
    def start_backend():
        process = subprocess.Popen([str(REPO / "target/debug/larm-reference-echo.exe"),"--port",str(backend_port)],stdin=subprocess.DEVNULL,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL,creationflags=subprocess.CREATE_NO_WINDOW)
        deadline = time.monotonic()+5
        while time.monotonic()<deadline:
            try:
                with socket.create_connection(("127.0.0.1",backend_port),timeout=0.1): return process
            except OSError: time.sleep(0.01)
        process.kill(); process.wait(timeout=5); raise AssertionError("backend echo no arranca")
    def connect(handshake=True):
        value = Session(executable(package,manifest),manifest,cwd=work,docs=package / "docs")
        if handshake: value.handshake()
        return value
    def request(**over):
        return {"lease_id":profile["lease_id"],"request_id":str(uuid.uuid7()),"capability":entry["id"],"input":{"text":"eco nativo á 😀"},"generation":None,"stream":False} | over
    def rejected(params, reason):
        result = session.call("protocol.invoke",params)
        assert result["error"]["data"]["details"]["reason"] == reason, result
        return result["error"]["data"]
    try:
        backend = start_backend(); session = connect()
        describe = session.result("protocol.describe",{"endpoint_id":profile["endpoint_id"]})
        step("autonomous-protocol-description", describe["capabilities"]==[entry["id"]] and not describe["streaming"] and not describe["tools"] and entry["input_sha256"] in describe["request_schema_id"], "Python/SDK/contratos fijados dentro del paquete, cuatro firmas protocol y descriptor con hash; ninguna inferencia anunciada")
        before = len(server.requests)
        denied = session.call("protocol.describe",{"endpoint_id":str(uuid.uuid7())})
        rejected(request(lease_id=str(uuid.uuid7())),"reference_protocol_lease_not_bound")
        rejected(request(capability="vendor.not_registered"),"capability_descriptor_missing")
        rejected(request(input={"text":4}),"capability_input_invalid")
        rejected(request(stream=True),"reference_protocol_options_not_supported")
        step("bindings-schema-options-before-http", denied["error"]["data"]["details"]["reason"]=="reference_protocol_endpoint_not_bound" and len(server.requests)==before, "endpoint/lease ajenos, capacidad ausente, input inválido y streaming no soportado no llegan a la red")
        params = request()
        answer = session.result("protocol.invoke",params)
        step("actual-rust-http-echo", answer["request_id"]==params["request_id"] and answer["output"]=={"echo":params["input"]} and answer["capability"]==entry["id"], "protocol.invoke hace POST real al proxy que reenvía al ejecutable echo Rust; resultado observado independientemente")
        step("usage-and-unknown-backend-preserved", answer["usage"]["tokens_input"] is None and answer["usage"]["tokens_output"] is None and answer["usage"]["latency_ms"]>=0 and answer["fingerprint"]["components"]["backend_identity"]["status"]=="UNAVAILABLE" and answer["fingerprint"]["components"]["backend_identity"]["value"] is None, "latencia medida, tokens desconocidos y binario remoto no observado no se convierten en cero ni verificados")
        qualify = {"endpoint_id":profile["endpoint_id"],"variant_id":profile["variant_id"],"suite_id":"reference.http.echo.v1","fixture_hash":entry["fixture_sha256"]}
        qualified = session.result("protocol.qualify",qualify)
        step("qualification-executes-pinned-fixture", qualified["qualified"] and all(t["status"]=="PASS" for t in qualified["tests"]) and qualified["fingerprint"]["components"]["scope"]["value"]=="controlled_reference_http_transport", "GET health y fixture HTTP reales; cualifica sólo este transporte, sin promover adapter ni certificar inferencia")
        before = len(server.requests)
        wrong = session.call("protocol.qualify",qualify | {"fixture_hash":"b"*64})
        step("qualification-hash-before-http", wrong["error"]["data"]["details"]["reason"]=="reference_protocol_fixture_not_bound" and len(server.requests)==before, "fixture distinta se rechaza sin enviar datos")
        for mode,reason in (("auth","reference_protocol_auth_required"),("rate","reference_protocol_rate_limited"),("redirect","reference_protocol_redirect_denied"),("invalid","reference_protocol_json_invalid"),("wrong","capability_output_invalid"),("large","reference_protocol_response_budget_exceeded"),("type","reference_protocol_http_reply_invalid"),("length","reference_protocol_reply_length_invalid")):
            server.mode=mode
            error = rejected(request(),reason)
            if mode=="rate": assert json.loads(error["details"]["original_error"]["message"])["retry_after_seconds"]=="900"
            assert "private-upstream-fixture" not in json.dumps(error)
            step("http-failure-"+mode, not any(r["path"]=="/redirect-not-followed" for r in server.requests), "fallo HTTP real normalizado, sin respuesta privada fuera del schema ni redirect seguido")
        server.mode="wrong"
        failed = session.result("protocol.qualify",qualify)
        step("bad-upstream-is-not-qualified", not failed["qualified"] and any(t["status"]=="FAIL" for t in failed["tests"]), "upstream con shape incorrecto no obtiene cualificación")
        server.mode="held"; server.release.clear(); server.received.clear()
        params=request(); transport=session.send("protocol.invoke",params)
        session.wait_progress(transport); assert server.received.wait(timeout=3)
        before=len(server.requests)
        rejected(params,"reference_protocol_request_active")
        cancelled=session.result("protocol.cancel",{"request_id":params["request_id"]})
        server.release.set()
        original=session.response(transport)
        step("semantic-cancel-and-active-duplicate", cancelled["cancelled"] and original["error"]["data"]["details"]["reason"]=="operation_cancelled" and len(server.requests)==before, "ID semántico distinto del UUID RPC: duplicado no sustituye el original ni hace otro POST; protocol.cancel señala la llamada activa")
        step("completed-cancel-is-false", not session.result("protocol.cancel",{"request_id":params["request_id"]})["cancelled"], "la operación cancelada sale del registro activo")
        server.mode="normal"
        step("next-invoke-after-cancel", session.result("protocol.invoke",request())["output"]["echo"]["text"]=="eco nativo á 😀", "cancel no rompe la sesión ni deja recursos activos")
        server.mode="held"; server.release.clear(); server.received.clear()
        timed=request(); transport=session.send("protocol.invoke",timed)
        session.wait_progress(transport); assert server.received.wait(timeout=3)
        timeout=session.response(transport)["error"]["data"]
        step("actual-http-timeout-releases-request", timeout["code"]=="UPSTREAM_UNAVAILABLE" and timeout["retryable"] and timeout["details"]["reason"]=="reference_protocol_network_failed", "respuesta retenida por el proxy supera timeout de IO real; error no afirma éxito")
        server.release.set(); server.mode="normal"
        step("same-request-after-timeout", session.result("protocol.invoke",timed)["output"]=={"echo":timed["input"]}, "la llamada fallida salió del mapa activo; nuevo intento explícito del echo no mutante funciona")
        backend.kill(); backend.wait(timeout=5); backend=None
        unavailable=session.call("protocol.invoke",request())["error"]["data"]
        assert unavailable["code"]=="UPSTREAM_UNAVAILABLE" and unavailable["retryable"]
        assert unavailable["details"]["reason"] in ("reference_protocol_http_error","reference_protocol_network_failed")
        backend=start_backend()
        step("real-backend-crash-and-recovery", session.result("protocol.invoke",request())["output"]["echo"]["text"]=="eco nativo á 😀", "muerte real sólo del proceso echo propio; el worker normaliza indisponibilidad y usa el backend reiniciado")
        session.close(); session=None
        saved=(package / "reference-capabilities.json").read_bytes()
        tampered=json.loads(saved); tampered["entries"][0]["input_sha256"]="b"*64
        (package / "reference-capabilities.json").write_text(json.dumps(tampered),encoding="utf-8")
        bad=subprocess.run(executable(package,manifest),cwd=work,input=b"",capture_output=True,timeout=15)
        (package / "reference-capabilities.json").write_bytes(saved)
        step("tampered-descriptor-no-handshake", bad.returncode==2 and not bad.stdout, "hash de schema alterado impide inicializar el worker; prueba directa controlada, host P022 sigue verificando package hash")
        server.mode="held"; server.release.clear()
        session=connect(handshake=False)
        outcomes=common(session,valid_method="protocol.describe",valid_params={"endpoint_id":profile["endpoint_id"]},cancel_method="protocol.invoke",cancel_params=request())
        server.release.set(); session.close(); session=None
        step("common-conformance-and-eof", len(outcomes)==7, "mismo runner: handshake/capabilities, params/error, progreso/heartbeat, core.cancel y cierre de proceso limpio")
        current={p.relative_to(package).as_posix():sha(p) for p in package.rglob("*") if p.is_file()}
        step("immutable-and-no-auth-header", current==initial and all(r["authorization"] is None for r in server.requests), "paquete/Python/descriptor intactos, sin escrituras de worker ni secretos/headers heredados")
        success=True; return 0
    finally:
        server.release.set()
        if session: session.close()
        if backend and backend.poll() is None:
            try:
                connection=http.client.HTTPConnection("127.0.0.1",backend_port,timeout=2); connection.request("POST","/shutdown",body=b""); connection.getresponse().read(); connection.close(); backend.wait(timeout=5)
            except (OSError,subprocess.TimeoutExpired): backend.kill(); backend.wait(timeout=5)
        server.shutdown(); server.server_close(); thread.join(timeout=2)
        record={"result":"PASS" if success else "FAIL","scope":"protocol reference with autonomous Python and actual Rust echo backend; controlled transport only, no inference/adapter promotion/Core leases","steps":steps,"run":run.relative_to(REPO).as_posix(),"package_sha256":manifest["package_sha256"],"backend_sha256":sha(REPO / "target/debug/larm-reference-echo.exe"),"host":platform.platform(),"generated_at":datetime.now(timezone.utc).isoformat(timespec="milliseconds").replace("+00:00","Z")}
        (run / "receipt.json").write_text(json.dumps(record,ensure_ascii=False,indent=2),encoding="utf-8")
        (REPO / "docs/reports/P026_runs/protocol-real.json").write_text(json.dumps(record,ensure_ascii=False,indent=2),encoding="utf-8")


if __name__=="__main__":
    sys.stdout.reconfigure(encoding="utf-8",errors="replace")
    raise SystemExit(main())
