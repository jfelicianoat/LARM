#!/usr/bin/env python3
"""Fixture de proceso para tests del composition root; servidor hasta EOF stdin."""
import importlib.util
import hashlib
import json
import sys
import threading
import zipfile
import http.client
import socket
import subprocess
import time
import os
from pathlib import Path

from p025_source_real import HTTP
from p026_protocol_real import Proxy
import http.server

REPO = Path(__file__).resolve().parent.parent


class NativeSourceHTTP(HTTP):
    """Inyección de fallos de fixture, fuera de los roots del worker.

    El control no suministra resultados/PASS; sólo cambia la respuesta HTTP real.
    El log observa headers sin conservar valores de autorización.
    """
    def do_GET(self):
        control = json.loads(self.server.control.read_text(encoding="utf-8"))
        if set(control) != {"mode", "probe_mode"} or any(value not in {"normal", "slow", "cut", "auth", "rate", "etag_changed", "corrupt", "range_end", "range_once"} for value in control.values()):
            self.send_error(500)
            return
        self.server.mode, self.server.probe_mode = control["mode"], control["probe_mode"]
        observed = {"path": self.path, "range": self.headers.get("Range"), "if_range": self.headers.get("If-Range"), "authorization_present": self.headers.get("Authorization") is not None, "mode": self.server.mode}
        payload = None
        range_failure = False
        if self.path.startswith("/artifact/") and self.server.mode in {"range_end", "range_once"}:
            index = int(self.path.rsplit("/", 1)[1])
            original = self.server.artifacts[index]["bytes"]
            range_header = self.headers.get("Range")
            offset = int(range_header.removeprefix("bytes=").removesuffix("-")) if range_header else 0
            if range_header:
                if self.server.mode == "range_end":
                    range_failure = offset >= len(original)
                else:
                    with self.server.log_lock:
                        range_failure = index not in self.server.rejected_range_artifacts
                        self.server.rejected_range_artifacts.add(index)
            if range_failure:
                etag = '"' + hashlib.sha256(original).hexdigest() + '"'
                observed.update(status=416,response_etag=etag,range_total=str(len(original)),response_body_sha256=hashlib.sha256(b"").hexdigest())
        if self.path.startswith("/artifact/") and self.server.mode in {"etag_changed", "corrupt"}:
            index = int(self.path.rsplit("/", 1)[1])
            original = self.server.artifacts[index]["bytes"]
            sha = hashlib.sha256(original).hexdigest()
            etag = '"' + ("transport-v2-" if self.server.mode == "etag_changed" else "") + sha + '"'
            payload = bytes([original[0] ^ 0xff]) + original[1:] if self.server.mode == "corrupt" else original
            offset = int(self.headers["Range"].removeprefix("bytes=").removesuffix("-")) if self.headers.get("Range") else 0
            if offset and self.headers.get("If-Range") != etag:
                offset = 0  # HTTP 200: If-Range ya no permite concatenar.
            status = 206 if offset else 200
            observed.update(status=status, response_etag=etag, body_sha256=hashlib.sha256(payload).hexdigest())
        with self.server.log_lock, self.server.request_log.open("a", encoding="utf-8") as output:
            output.write(json.dumps(observed) + "\n")
            output.flush()
            os.fsync(output.fileno())
        if range_failure:
            self.send_response(416)
            self.send_header("ETag", etag)
            self.send_header("Content-Range", f"bytes */{len(original)}")
            self.send_header("Content-Length", "0")
            self.end_headers()
            return
        if payload is not None:
            self.send_response(status)
            self.send_header("ETag", etag)
            self.send_header("Content-Length", str(len(payload) - offset))
            if offset:
                self.send_header("Content-Range", f"bytes {offset}-{len(payload)-1}/{len(payload)}")
            self.end_headers()
            try:
                for at in range(offset, len(payload), 16 << 10):
                    self.wfile.write(payload[at:at + (16 << 10)])
                    self.wfile.flush()
            except OSError:
                pass
            return
        super().do_GET()


class NativeProtocolHTTP(Proxy):
    def handle_http(self, method):
        import io
        self.server.sync_control()
        length = int(self.headers.get("Content-Length", "0"))
        if not 0 <= length <= 32 << 10:
            self.send_error(413)
            return
        body = self.rfile.read(length)
        self.rfile = io.BytesIO(body)
        observed = {"method": method, "path": self.path, "body_sha256": hashlib.sha256(body).hexdigest(), "authorization_present": self.headers.get("Authorization") is not None, "mode": self.server.mode}
        with self.server.log_lock, self.server.request_log.open("a", encoding="utf-8") as output:
            output.write(json.dumps(observed) + "\n")
            output.flush()
            os.fsync(output.fileno())
        super().handle_http(method)


class NativeBenchmarkHTTP(http.server.BaseHTTPRequestHandler):
    """Observador HTTP independiente del worker; sólo hashes y tiempos propios."""
    def log_message(self, *_args): pass
    def do_POST(self): self.handle_http("POST")
    def do_GET(self): self.handle_http("GET")

    def emit(self, event):
        with self.server.log_lock, self.server.request_log.open("a", encoding="utf-8") as output:
            output.write(json.dumps(event) + "\n")
            output.flush()
            os.fsync(output.fileno())

    def handle_http(self, method):
        self.server.sync_control()
        length = int(self.headers.get("Content-Length", "0"))
        if not 0 <= length <= 32 << 10:
            self.send_error(413)
            return
        body = self.rfile.read(length)
        with self.server.log_lock:
            seq = self.server.next_request
            self.server.next_request += 1
        mode = self.server.mode
        if self.server.hold_after is not None and seq >= self.server.hold_after: mode = "held"
        start = time.monotonic_ns()
        self.emit({"kind":"request", "seq":seq, "method":method, "path":self.path, "body_sha256":hashlib.sha256(body).hexdigest(), "authorization_present":self.headers.get("Authorization") is not None, "mode":mode})
        status, content_type, extra = 200, "application/json", {}
        if method != "POST" or self.path != "/echo": status, result = 404, b'{}'
        elif mode == "auth": status, result = 401, b'{}'
        elif mode == "rate": status, result, extra = 429, b'{}', {"Retry-After":"900"}
        elif mode == "redirect": status, result, extra = 302, b'{}', {"Location":f"http://127.0.0.1:{self.server.server_port}/redirect-not-followed"}
        elif mode == "wrong": result = b'{"echo":{"private":"private-benchmark-upstream-marker"}}'
        elif mode == "invalid": result = b'{"echo":'
        elif mode == "large": result = b' ' * (40 << 10)
        else:
            connection = http.client.HTTPConnection("127.0.0.1", self.server.backend_port, timeout=2)
            try:
                connection.request(method, self.path, body=body, headers={"Content-Type":"application/json"})
                response = connection.getresponse()
                status, result = response.status, response.read((1 << 20) + 1)
            except OSError: status, result = 503, b'{}'
            finally: connection.close()
        if mode == "held": self.server.release.wait(timeout=3)
        if mode == "type": content_type = "text/plain"
        self.emit({"kind":"response", "seq":seq, "status":status, "wire_sha256":hashlib.sha256(result).hexdigest(), "duration_ns":str(time.monotonic_ns()-start)})
        try:
            self.send_response(status)
            self.send_header("Content-Type", content_type)
            self.send_header("Content-Length", str(len(result) + (7 if mode == "length" else 0)))
            for key, value in extra.items(): self.send_header(key, value)
            self.end_headers()
            self.wfile.write(result)
        except OSError: pass


def native_protocol(folder, kind="protocol"):
    """Proxy/echo propios; control de fallos sin resultados de conformidad."""
    with socket.socket() as reserve:
        reserve.bind(("127.0.0.1", 0))
        port = reserve.getsockname()[1]
    benchmark = kind == "benchmark"
    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), NativeBenchmarkHTTP if benchmark else NativeProtocolHTTP)
    server.hold_after, server.next_request = None, 0
    server.backend_port, server.requests, server.mode = port, [], "normal"
    server.release, server.received = threading.Event(), threading.Event()
    server.log_lock, manager_lock, done = threading.Lock(), threading.Lock(), threading.Event()
    control, state = folder / f"{kind}-control.json", folder / "backend-state.json"
    server.request_log = folder / f"{kind}-requests.ndjson"
    server.request_log.write_bytes(b"")
    initial = {"mode": "normal", "release": True, "backend_running": True}
    if benchmark: initial["hold_after"] = None
    control.write_text(json.dumps(initial), encoding="utf-8")
    backend, epoch = None, 0
    binary = REPO / "target/debug/larm-reference-echo.exe"

    def sync_control():
        nonlocal backend, epoch
        with manager_lock:
            # Windows puede negar un open durante el reemplazo atómico de este
            # fichero propio. Reintento acotado; control inválido nunca se acepta.
            deadline = time.monotonic() + 1
            while True:
                try:
                    value = json.loads(control.read_text(encoding="utf-8"))
                    break
                except (PermissionError, FileNotFoundError):
                    if time.monotonic() >= deadline: raise
                    time.sleep(0.005)
            keys = {"mode", "release", "backend_running"} | ({"hold_after"} if benchmark else set())
            if set(value) != keys or value["mode"] not in {"normal", "auth", "rate", "redirect", "invalid", "wrong", "large", "held", "type", "length"} or type(value["release"]) is not bool or type(value["backend_running"]) is not bool or (benchmark and value["hold_after"] is not None and (type(value["hold_after"]) is not int or not 0 <= value["hold_after"] <= 10000)):
                raise ValueError("control protocol invalido")
            server.hold_after = value.get("hold_after")
            server.mode = value["mode"]
            if value["release"]: server.release.set()
            else: server.release.clear()
            running = backend is not None and backend.poll() is None
            changed = False
            if not value["backend_running"] and running:
                backend.kill()
                backend.wait(timeout=5)
                changed = True
            elif value["backend_running"] and not running:
                backend = subprocess.Popen([str(binary), "--port", str(port)], stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, creationflags=subprocess.CREATE_NO_WINDOW)
                epoch += 1
                deadline = time.monotonic() + 5
                while True:
                    try:
                        with socket.create_connection(("127.0.0.1", port), timeout=0.1): break
                    except OSError:
                        if time.monotonic() >= deadline: raise RuntimeError("backend protocol no iniciado")
                        time.sleep(0.01)
                changed = True
            if changed or not state.exists():
                temporary = state.with_suffix(".tmp")
                temporary.write_text(json.dumps({"pid": backend.pid if backend else None, "running": backend is not None and backend.poll() is None, "epoch": epoch}), encoding="utf-8")
                # The Rust worker may briefly have the previous state file open
                # without delete sharing. Retry the atomic replacement on Windows.
                deadline = time.monotonic() + 1
                while True:
                    try:
                        os.replace(temporary, state)
                        break
                    except PermissionError:
                        if time.monotonic() >= deadline:
                            raise
                        time.sleep(0.005)

    server.sync_control = sync_control
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    watcher = threading.Thread(target=lambda: monitor(), daemon=True)

    def monitor():
        while not done.wait(0.01): sync_control()

    try:
        sync_control()
        thread.start()
        watcher.start()
        base = f"http://127.0.0.1:{server.server_port}"
        if benchmark:
            module = load("benchmark_factory", REPO / "adapters/benchmark/reference/package.py")
            _manifest, profile, policy, request = module.create(REPO, folder / "package", folder / "work", base)
            extra = {"policy":policy, "request":request}
        else:
            module = load("protocol_factory", REPO / "adapters/protocol/reference/package.py")
            _manifest, profile, entry = module.create(REPO, folder / "package", folder / "work", base)
            extra = {"entry":entry}
        print(json.dumps({"package": str(folder / "package"), "work": str(folder / "work"), "base_url": base, "profile": profile, "control": str(control), "requests": str(server.request_log), "backend_state": str(state), "backend_binary": str(binary)} | extra), flush=True)
        sys.stdin.buffer.read()
    finally:
        done.set()
        server.release.set()
        if watcher.is_alive(): watcher.join(timeout=6)
        if thread.is_alive(): server.shutdown()
        server.server_close()
        if thread.is_alive(): thread.join(timeout=2)
        if backend and backend.poll() is None:
            backend.kill()
            backend.wait(timeout=5)


def load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def main():
    kind, folder = sys.argv[1], Path(sys.argv[2]).resolve(strict=True)
    if kind in ("source", "source-conformance"):
        native = kind == "source-conformance"
        server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), NativeSourceHTTP if native else HTTP)
        server.artifacts = ([{"name": "data.bin", "bytes": bytes([index]) * (2 << 20), "capability": f"vendor.reference.case{index}"} for index in range(6)] if native else [{"name": "data.bin", "bytes": b"self-authored-source-fixture\n" * 4096, "capability": "vendor.reference.data"}])
        extra = {}
        if native:
            server.control, server.request_log = folder / "http-control.json", folder / "http-requests.ndjson"
            server.log_lock = threading.Lock()
            server.rejected_range_artifacts = set()
            server.control.write_text(json.dumps({"mode": "normal", "probe_mode": "normal"}), encoding="utf-8")
            server.request_log.write_bytes(b"")
            expected_files = []
            for index, artifact in enumerate(server.artifacts):
                path = folder / f"expected-{index}.bin"
                path.write_bytes(artifact["bytes"])
                expected_files.append(str(path))
            extra = {"control": str(server.control), "requests": str(server.request_log), "expected_files": expected_files}
        server.mode, server.probe_mode, server.requests = "normal", "normal", []
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            base = f"http://127.0.0.1:{server.server_port}"
            module = load("source_factory", REPO / "adapters/source/reference/package.py")
            package = folder / "data/adapters/reference.catalog/1.0.0"
            manifest, config, catalog = module.create(REPO, package, folder / "work", base, server.artifacts)
            archive_path = folder / "reference-source-1.0.1.zip"
            with zipfile.ZipFile(archive_path, "w", compression=zipfile.ZIP_DEFLATED) as archive:
                for path in sorted(package.rglob("*")):
                    if path.is_file() and path.name != "manifest.json":
                        archive.write(path, path.relative_to(package).as_posix())
                archive.writestr("manifest.json", json.dumps(manifest | {"version": "1.0.1"}))
            request = folder / "update.json"
            request.write_text(json.dumps({"package_path": str(archive_path), "expected_hash": hashlib.sha256(archive_path.read_bytes()).hexdigest(), "approval_id": None}), encoding="utf-8")
            print(json.dumps({"package": str(package), "work": str(folder / "work"), "base_url": base, "config": config, "catalog": catalog, "model": catalog["entries"][0]["model"], "sha256": catalog["entries"][0]["inspection"]["artifacts"][0]["sha256"], "update_request": str(request)} | extra), flush=True)
            sys.stdin.buffer.read()
        finally:
            server.shutdown()
            server.server_close()
            thread.join(timeout=2)
    elif kind in ("protocol-conformance", "benchmark-conformance"):
        native_protocol(folder, kind.split("-")[0])
    elif kind == "gguf-conformance":
        module=load("gguf_factory",REPO / "adapters/transformation/gguf/package.py")
        data=module.build_conformance_safetensors()
        _manifest,profile,entry=module.create(REPO,folder / "package",folder / "work",data)
        expected = folder / "expected-input.bin"
        expected.write_bytes(data)
        print(json.dumps({"package":str(folder / "package"),"work":str(folder / "work"),"profile":profile,"entry":entry,"expected_input":str(expected)}),flush=True)
        sys.stdin.buffer.read()
    elif kind == "onnx-conformance":
        module=load("onnx_factory",REPO / "adapters/transformation/onnx/package.py")
        data=module.build_conformance_safetensors()
        _manifest,profile,entry=module.create(REPO,folder / "package",folder / "work",data)
        expected = folder / "expected-input.bin"
        expected.write_bytes(data)
        print(json.dumps({"package":str(folder / "package"),"work":str(folder / "work"),"profile":profile,"entry":entry,"expected_input":str(expected)}),flush=True)
        sys.stdin.buffer.read()
    elif kind in ("transformation", "transformation-conformance"):
        module=load("transformation_factory",REPO / "adapters/transformation/reference/package.py")
        native = kind == "transformation-conformance"
        data=(bytes(range(256)) * (48 << 10) + b"LARM!") if native else b"LARM-owned-transform-input\n"*4096+b"padding"
        _manifest,profile,entry=module.create(REPO,folder / "package",folder / "work",data)
        extra = {}
        if native:
            expected = folder / "expected-input.bin"
            expected.write_bytes(data)
            extra = {"expected_input":str(expected)}
        print(json.dumps({"package":str(folder / "package"),"work":str(folder / "work"),"profile":profile,"entry":entry} | extra),flush=True)
        sys.stdin.buffer.read()
    elif kind in ("protocol","benchmark"):
        with socket.socket() as reserve:
            reserve.bind(("127.0.0.1",0))
            port = reserve.getsockname()[1]
        backend = subprocess.Popen([str(REPO / "target/debug/larm-reference-echo.exe"),"--port",str(port)],stdin=subprocess.DEVNULL,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL,creationflags=subprocess.CREATE_NO_WINDOW)
        try:
            deadline=time.monotonic()+5
            while True:
                try:
                    with socket.create_connection(("127.0.0.1",port),timeout=0.1): break
                except OSError:
                    if time.monotonic()>=deadline: raise RuntimeError("backend no iniciado")
                    time.sleep(0.01)
            base=f"http://127.0.0.1:{port}"
            if kind=="protocol":
                module=load("protocol_factory",REPO / "adapters/protocol/reference/package.py")
                _manifest,profile,entry=module.create(REPO,folder / "package",folder / "work",base)
                data={"profile":profile,"entry":entry}
            else:
                module=load("benchmark_factory",REPO / "adapters/benchmark/reference/package.py")
                _manifest,profile,policy,request=module.create(REPO,folder / "package",folder / "work",base)
                data={"profile":profile,"policy":policy,"request":request}
            print(json.dumps({"package":str(folder / "package"),"work":str(folder / "work"),"base_url":base}|data),flush=True)
            sys.stdin.buffer.read()
        finally:
            if backend.poll() is None:
                try:
                    connection=http.client.HTTPConnection("127.0.0.1",port,timeout=1)
                    connection.request("POST","/shutdown",body=b""); connection.getresponse().read(); connection.close()
                    backend.wait(timeout=1)
                except (OSError,subprocess.TimeoutExpired): backend.kill(); backend.wait(timeout=5)
    elif kind in ("runtime", "runtime-conformance"):
        module = load("runtime_factory", REPO / "adapters/runtime/reference/package.py")
        package, _manifest, model = module.create(REPO, folder / "package", version_delay_ms=8000 if kind=="runtime-conformance" else 0)
        (folder / "work").mkdir()
        info={"package": str(package), "work": str(folder / "work"), "model_path": str(model)}
        if kind=="runtime-conformance":
            info["observed_version"]=subprocess.run([str(package/"bin/echo.exe"),"--version"],capture_output=True,timeout=5,check=True,creationflags=subprocess.CREATE_NO_WINDOW).stdout.decode().strip()
        print(json.dumps(info), flush=True)
        sys.stdin.buffer.read()
    else:
        raise ValueError("familia desconocida")


if __name__ == "__main__":
    main()
