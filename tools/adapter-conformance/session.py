"""Cliente reutilizable NDJSON para conformance de paquetes controlados.

Valida contratos, nonce y cada respuesta. No es un sandbox ni concede confianza:
las pruebas mediante el host protegido conservan P022/P026 como propietarios.
"""
import json
import os
import subprocess
import threading
import time
import uuid
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
import sys
sys.path.insert(0, str(REPO / "adapters/sdk/python"))
from larm_adapter_sdk import Contracts  # noqa: E402


class Session:
    def __init__(self, command, manifest, *, cwd=None, docs=None):
        self.contracts = Contracts(docs or REPO / "docs")
        self.manifest = self.contracts.manifest(manifest)
        self.pending, self.replies, self.progress = {}, {}, {}
        self.failure, self.stderr_bytes, self.closed = None, 0, False
        self.condition, self.write_lock = threading.Condition(), threading.Lock()
        env = {k: v for k, v in os.environ.items() if k.upper() in ("SYSTEMROOT", "SYSTEMDRIVE", "WINDIR", "TEMP", "TMP", "PATH", "PROCESSOR_ARCHITECTURE", "NUMBER_OF_PROCESSORS")}
        env["PYTHONUTF8"] = "1"
        self.process = subprocess.Popen(list(map(str, command)), cwd=cwd, env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, creationflags=subprocess.CREATE_NO_WINDOW if os.name == "nt" else 0)
        self.threads = [threading.Thread(target=self._read, daemon=True), threading.Thread(target=self._stderr, daemon=True)]
        for thread in self.threads:
            thread.start()

    def _read(self):
        try:
            while raw := self.process.stdout.readline((1 << 20) + 2):
                if len(raw.removesuffix(b"\n")) > 1 << 20:
                    raise AssertionError("stdout contiene un frame demasiado grande")
                frame = json.loads(raw)
                self.contracts.frame("progress" if "method" in frame else "response", frame)
                with self.condition:
                    if "method" in frame:
                        identity = frame["params"]["request_id"]
                        if identity not in self.pending:
                            raise AssertionError("progreso de una llamada desconocida")
                        self.progress.setdefault(identity, []).append(frame["params"])
                    else:
                        identity = frame["id"]
                        if identity not in self.pending or identity in self.replies:
                            raise AssertionError("respuesta sin llamada o duplicada")
                        method = self.pending[identity]
                        if "result" in frame:
                            if method not in self.contracts.methods:
                                raise AssertionError("éxito de método desconocido")
                            self.contracts.response(method, frame["result"])
                        self.replies[identity] = frame
                    self.condition.notify_all()
        except Exception as error:
            with self.condition:
                self.failure = type(error).__name__ + ": protocolo inválido en stdout"
                self.condition.notify_all()
        finally:
            with self.condition:
                self.closed = True
                self.condition.notify_all()

    def _stderr(self):
        # No persiste texto del worker ni credenciales: sólo cuenta los bytes.
        while chunk := self.process.stderr.read(8192):
            self.stderr_bytes += len(chunk)
            if self.stderr_bytes > 10 << 20:
                with self.condition:
                    self.failure = "stderr excedió 10 MiB"
                    self.condition.notify_all()

    def send(self, method, params, *, validate=True):
        if validate and method in self.contracts.methods:
            self.contracts.request(method, params)
        identity = str(uuid.uuid7())
        with self.condition:
            self.pending[identity] = method
        raw = json.dumps({"jsonrpc": "2.0", "id": identity, "method": method, "params": params}, ensure_ascii=False, allow_nan=False).encode()
        if len(raw) > 1 << 20:
            raise ValueError("request supera 1 MiB")
        with self.write_lock:
            self.process.stdin.write(raw + b"\n")
            self.process.stdin.flush()
        return identity

    def response(self, identity, timeout=15):
        end = time.monotonic() + timeout
        with self.condition:
            while identity not in self.replies:
                if self.failure:
                    raise AssertionError(self.failure)
                if self.closed:
                    raise AssertionError("worker cerró stdout antes de responder")
                remaining = end - time.monotonic()
                if remaining <= 0:
                    raise TimeoutError("la llamada no terminó")
                self.condition.wait(remaining)
            return self.replies[identity]

    def call(self, method, params, *, validate=True, timeout=15):
        return self.response(self.send(method, params, validate=validate), timeout=timeout)

    def result(self, method, params, **kwargs):
        response = self.call(method, params, **kwargs)
        if "error" in response:
            reason = response["error"].get("data", {}).get("details", {}).get("reason", "rpc_error")
            raise AssertionError(f"{method}: {reason}")
        return response["result"]

    def wait_progress(self, identity, timeout=10):
        end = time.monotonic() + timeout
        with self.condition:
            while not self.progress.get(identity):
                if identity in self.replies or self.closed or self.failure:
                    raise AssertionError("llamada terminó sin progreso esperado")
                remaining = end - time.monotonic()
                if remaining <= 0:
                    raise TimeoutError("sin progreso")
                self.condition.wait(remaining)
            return self.progress[identity][0]

    def handshake(self):
        nonce = uuid.uuid4().hex + uuid.uuid4().hex
        result = self.result("core.handshake", {"protocol_version": "1.0", "adapter_id": self.manifest["id"], "nonce": nonce})
        assert result["nonce"] == nonce and result["protocol_version"].split(".")[0] == "1"
        assert set(result["methods"]) == set(self.manifest["methods"])
        assert set(result["capabilities"]) == set(self.manifest["capabilities"])
        return result

    def close(self):
        try:
            if not self.process.stdin.closed:
                self.process.stdin.close()
            try:
                code = self.process.wait(timeout=7)
            except subprocess.TimeoutExpired:
                if os.name == "nt":
                    subprocess.run(["taskkill", "/PID", str(self.process.pid), "/T", "/F"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=False)
                self.process.kill()
                self.process.wait(timeout=5)
                raise AssertionError("EOF no terminó el worker dentro de la gracia")
            assert code == 0, "worker terminó con error"
            for thread in self.threads:
                thread.join(timeout=2)
                assert not thread.is_alive(), "pipe retenido tras salir el worker"
            assert not self.failure, self.failure
            assert self.stderr_bytes <= 10 << 20
        finally:
            if self.process.poll() is None:
                self.process.kill()
                self.process.wait(timeout=5)
            self.process.stdout.close()
            self.process.stderr.close()

    def crash(self):
        """Fault injection sólo sobre el proceso creado por esta Session.

        No acredita stdout/shutdown normales tras la muerte deliberada.
        """
        assert not self.failure, self.failure
        self.process.kill()
        self.process.wait(timeout=5)
        for thread in self.threads:
            thread.join(timeout=2)
            assert not thread.is_alive()
        if not self.process.stdin.closed:
            try:
                self.process.stdin.close()
            except OSError:
                pass
        self.process.stdout.close()
        self.process.stderr.close()
