#!/usr/bin/env python3
"""P009 - Prueba real de autenticación, secretos y ACL en Windows (PC IA).

  python tools/p009_real.py serve

Sobre una raíz de datos de prueba (docs/reports/P009_runs/real) y con el daemon
real sirviendo la API en un puerto de prueba:

  1. alta de principales por CLI (operador admin y Broker read+leases:manage);
  2. sin Bearer -> 401; Broker sobre /credentials -> 403; admin -> 200;
  3. POST /credentials: 201 sin secreto en la respuesta; el secreto está en Credential
     Manager (CredReadW) y no aparece en la base (.sqlite/-wal) ni en los logs;
  4. DELETE con If-Match -> 204 y la entrada desaparece de Credential Manager;
  5. rotación desde otro proceso (CLI): el token anterior deja de valer y el nuevo vale;
  6. bootstrap por named pipe: cada conexión entrega un token de escritorio nuevo, el
     anterior queda revocado; la DACL del pipe es protegida y solo admite el SID del
     usuario y SYSTEM;
  7. ningún token aparece en los logs; CTRL_BREAK -> exit 0;
  8. limpieza: se revocan los principales de prueba y no queda ninguna entrada suya.

La comprobación con un segundo usuario de Windows no se puede hacer aquí (no hay otra
cuenta con credenciales): queda BLOCKED_REAL con el procedimiento en el report.
"""
import ctypes
import ctypes.wintypes as wt
import json
import os
import secrets
import shutil
import signal
import socket
import subprocess
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
EXE = REPO / "target/debug/larm-daemon.exe"
ROOT = REPO / "docs/reports/P009_runs/real"

advapi32 = ctypes.WinDLL("advapi32", use_last_error=True)
kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)


class CREDENTIALW(ctypes.Structure):
    _fields_ = [("Flags", wt.DWORD), ("Type", wt.DWORD), ("TargetName", wt.LPWSTR), ("Comment", wt.LPWSTR),
                ("LastWritten", wt.FILETIME), ("CredentialBlobSize", wt.DWORD),
                ("CredentialBlob", ctypes.POINTER(ctypes.c_ubyte)), ("Persist", wt.DWORD),
                ("AttributeCount", wt.DWORD), ("Attributes", ctypes.c_void_p), ("TargetAlias", wt.LPWSTR),
                ("UserName", wt.LPWSTR)]


def cred_read(target: str) -> str | None:
    pcred = ctypes.POINTER(CREDENTIALW)()
    if not advapi32.CredReadW(target, 1, 0, ctypes.byref(pcred)):
        return None
    try:
        blob = bytes(pcred.contents.CredentialBlob[: pcred.contents.CredentialBlobSize])
    finally:
        advapi32.CredFree(pcred)
    for codec in ("utf-16-le", "utf-8"):
        try:
            return blob.decode(codec)
        except UnicodeDecodeError:
            continue
    return blob.hex()


def cred_targets(fragment: str) -> list[str]:
    count = wt.DWORD()
    pcreds = ctypes.POINTER(ctypes.POINTER(CREDENTIALW))()
    if not advapi32.CredEnumerateW(None, 0, ctypes.byref(count), ctypes.byref(pcreds)):
        return []
    try:
        names = [pcreds[i].contents.TargetName for i in range(count.value)]
    finally:
        advapi32.CredFree(pcreds)
    return [n for n in names if n and fragment in n]


def pipe_read_and_dacl(name: str) -> tuple[str, str]:
    """Conecta al pipe, devuelve (línea recibida, SDDL de su DACL)."""
    GENERIC_READ, READ_CONTROL, OPEN_EXISTING = 0x80000000, 0x00020000, 3
    kernel32.CreateFileW.restype = wt.HANDLE
    handle = kernel32.CreateFileW(name, GENERIC_READ | READ_CONTROL, 0, None, OPEN_EXISTING, 0, None)
    if handle in (None, wt.HANDLE(-1).value):
        raise AssertionError(f"no se pudo abrir {name}: error {ctypes.get_last_error()}")
    try:
        psd = ctypes.c_void_p()
        rc = advapi32.GetSecurityInfo(handle, 6, 4, None, None, None, None, ctypes.byref(psd))  # SE_KERNEL_OBJECT, DACL
        if rc != 0:
            raise AssertionError(f"GetSecurityInfo error {rc}")
        sddl = wt.LPWSTR()
        if not advapi32.ConvertSecurityDescriptorToStringSecurityDescriptorW(psd, 1, 4, ctypes.byref(sddl), None):
            raise AssertionError("no se pudo convertir el descriptor")
        dacl = sddl.value
        kernel32.LocalFree(sddl)
        kernel32.LocalFree(psd)
        buf = ctypes.create_string_buffer(8192)
        read = wt.DWORD()
        data = b""
        while kernel32.ReadFile(handle, buf, 8192, ctypes.byref(read), None) and read.value:
            data += buf.raw[: read.value]
            if b"\n" in data:
                break
        return data.decode("utf-8").strip(), dacl
    finally:
        kernel32.CloseHandle(handle)


def current_sid() -> str:
    out = subprocess.run(["whoami", "/user", "/fo", "csv", "/nh"], capture_output=True, text=True, check=True).stdout
    return out.strip().rsplit(",", 1)[-1].strip().strip('"')


def cli(*args: str, stdin: str | None = None) -> dict:
    env = {k: v for k, v in os.environ.items() if not k.startswith("LARM_")}
    p = subprocess.run([str(EXE), "--data-root", str(ROOT), *args], input=stdin, capture_output=True, text=True,
                       encoding="utf-8", env=env, timeout=60, check=False)
    if p.returncode != 0:
        raise AssertionError(f"{args[0]} falló: {p.stderr[-300:]}")
    return json.loads(p.stdout)


def http(method: str, url: str, token: str | None = None, body: dict | None = None,
         headers: dict | None = None) -> tuple[int, dict | None]:
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(url, data=data, method=method)
    req.add_header("content-type", "application/json")
    if token:
        req.add_header("authorization", f"Bearer {token}")
    for k, v in (headers or {}).items():
        req.add_header(k, v)
    try:
        with urllib.request.urlopen(req, timeout=10) as r:
            raw = r.read()
            return r.status, json.loads(raw) if raw else None
    except urllib.error.HTTPError as e:
        raw = e.read()
        return e.code, json.loads(raw) if raw else None


def free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def step(log: list, name: str, ok: bool, detail: str) -> None:
    log.append({"step": name, "result": "PASS" if ok else "FAIL", "detail": detail})
    print(f"  [{'PASS' if ok else 'FAIL'}] {name}: {detail}")
    if not ok:
        raise AssertionError(f"{name}: {detail}")


def serve() -> str:
    shutil.rmtree(ROOT, ignore_errors=True)
    ROOT.mkdir(parents=True)
    log: list = []
    tokens: list[str] = []
    names = ["p009-admin", "p009-broker"]
    proc = None
    try:
        admin = cli("enroll-principal", "--name", "p009-admin", "--kind", "OPERATOR", "--scopes", "admin")
        broker = cli("enroll-principal", "--name", "p009-broker", "--kind", "BROKER", "--scopes", "read,leases:manage")
        tokens += [admin["token"], broker["token"]]
        listed = json.dumps(cli("list-principals"))
        step(log, "alta-cli", all(t not in listed for t in tokens) and "p009-broker" in listed,
             "2 principales; list-principals no muestra tokens")
        try:
            cli("enroll-principal", "--name", "p009-x", "--kind", "BROKER", "--scopes", "read,admin")
            step(log, "broker-limitado", False, "se aceptó un Broker con admin")
        except AssertionError as e:
            step(log, "broker-limitado", "broker_scope_not_allowed" in str(e) or "Principal no válido" in str(e),
                 "Broker con admin rechazado")

        port = free_port()
        base = f"http://127.0.0.1:{port}"
        env = {k: v for k, v in os.environ.items() if not k.startswith("LARM_")} | {"LARM_LOG_FORMAT": "json"}
        proc = subprocess.Popen([str(EXE), "--data-root", str(ROOT), "--port", str(port), "--serve-api"], env=env,
                                creationflags=subprocess.CREATE_NEW_PROCESS_GROUP,
                                stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            try:
                if http("GET", f"{base}/api/v1/credentials", admin["token"])[0] == 200:
                    break
            except OSError:
                time.sleep(0.3)
        status = http("GET", f"{base}/api/v1/credentials", admin["token"])[0]
        step(log, "api-arranca", status == 200, f"GET /credentials admin -> {status}")
        s1 = http("GET", f"{base}/api/v1/credentials")
        s2 = http("GET", f"{base}/api/v1/credentials", broker["token"])
        step(log, "authz", s1[0] == 401 and s2[0] == 403 and s2[1]["code"] == "FORBIDDEN",
             f"sin Bearer {s1[0]} {s1[1]['code']}; Broker {s2[0]} {s2[1]['code']}")

        secret = "hf_" + secrets.token_hex(24)
        body = {"service": "larm/test/p009", "username": "cuenta", "secret_type": "TOKEN", "secret": secret, "scopes": []}
        created = http("POST", f"{base}/api/v1/credentials", admin["token"], body, {"idempotency-key": "p009-alta"})
        stored = cred_read("cuenta.larm/test/p009")
        db_bytes = b"".join(p.read_bytes() for p in (ROOT / "state").glob("larm.sqlite*"))
        step(log, "secreto-en-credential-manager",
             created[0] == 201 and secret not in json.dumps(created[1]) and stored == secret
             and secret.encode() not in db_bytes and secret.encode("utf-16-le") not in db_bytes,
             f"201; respuesta sin secreto; CredReadW coincide; ausente de {len(db_bytes)} bytes de SQLite/WAL")
        cid, version = created[1]["id"], created[1]["row_version"]
        no_if = http("DELETE", f"{base}/api/v1/credentials/{cid}", admin["token"])
        deleted = http("DELETE", f"{base}/api/v1/credentials/{cid}", admin["token"], headers={"if-match": str(version)})
        step(log, "borrado", no_if[0] == 428 and deleted[0] == 204 and cred_read("cuenta.larm/test/p009") is None,
             f"sin If-Match {no_if[0]}; con If-Match {deleted[0]}; entrada eliminada de Credential Manager")

        rotated = cli("rotate-principal", "--name", "p009-admin")
        tokens.append(rotated["token"])
        deadline = time.monotonic() + 8
        old_status = 200
        while time.monotonic() < deadline and old_status == 200:
            old_status = http("GET", f"{base}/api/v1/credentials", admin["token"])[0]
            time.sleep(0.5)
        new_status = http("GET", f"{base}/api/v1/credentials", rotated["token"])[0]
        step(log, "rotacion-externa", old_status == 401 and new_status == 200,
             f"token anterior -> {old_status} antes de 8 s; nuevo -> {new_status}")

        sid = current_sid()
        import hashlib
        pipe = r"\\.\pipe\LARM-" + hashlib.sha256(sid.encode()).hexdigest()[:16] + "-bootstrap"
        line1, dacl = pipe_read_and_dacl(pipe)
        first = json.loads(line1)
        line2, _ = pipe_read_and_dacl(pipe)
        second = json.loads(line2)
        tokens += [first["token"], second["token"]]
        use_old = http("GET", f"{base}/api/v1/credentials", first["token"])[0]
        use_new = http("GET", f"{base}/api/v1/credentials", second["token"])[0]
        step(log, "bootstrap-un-uso",
             first["principal"] == "desktop" and first["token"] != second["token"] and use_old == 401 and use_new == 200,
             f"2 conexiones -> 2 tokens distintos; el primero {use_old}, el segundo {use_new}")
        expected = {f"(A;;FA;;;{sid})", "(A;;FA;;;SY)"}
        aces = {"(" + a for a in dacl.split("(")[1:]}
        step(log, "dacl-pipe", dacl.startswith("D:P") and aces == expected,
             f"DACL {dacl} (protegida; solo el SID del usuario y SYSTEM)")

        os.kill(proc.pid, signal.CTRL_BREAK_EVENT)
        rc = proc.wait(timeout=20)
        proc = None
        logs = "".join(p.read_text(encoding="utf-8", errors="replace") for p in (ROOT / "logs").glob("*.log"))
        leaked = [t[:8] for t in tokens + [secret] if t in logs]
        step(log, "logs-sin-secretos", rc == 0 and not leaked and "credential" in logs,
             f"exit {rc}; {len(tokens)} tokens y el secreto ausentes de {len(logs)} bytes de log")
        names.append("desktop")
    finally:
        if proc is not None and proc.poll() is None:
            proc.kill()
        for name in names + ["desktop"]:
            try:
                cli("revoke-principal", "--name", name)
            except AssertionError:
                pass
        (ROOT / "steps.json").write_text(json.dumps(log, indent=2, ensure_ascii=False), encoding="utf-8")
    remaining = cred_targets("larm/principal/") + cred_targets("larm/test/p009")
    # Solo cuentan las entradas de esta raíz: sus ids están en steps/CLI; filtramos por los usuarios de prueba.
    ours = [t for t in remaining if t.split(".", 1)[0] in {"p009-admin", "p009-broker", "desktop", "cuenta"}]
    if ours:
        raise AssertionError(f"quedaron entradas de prueba en Credential Manager: {ours}")
    return f"{len(log)} pasos PASS; limpieza: 0 entradas de prueba en Credential Manager"


if __name__ == "__main__":
    sys.stdout.reconfigure(encoding="utf-8")
    try:
        print("PASS " + {"serve": serve}[sys.argv[1]]())
        raise SystemExit(0)
    except (AssertionError, KeyError, IndexError, OSError, json.JSONDecodeError, subprocess.TimeoutExpired) as e:
        print(f"FAIL {type(e).__name__}: {e}")
        raise SystemExit(1)
