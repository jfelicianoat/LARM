#!/usr/bin/env python3
"""Enrola tokens externos por stdin con la API real de P009.

Entrada JSON: {"ai-broker": "...", "agora": "..."}. Nunca usar argumentos
ni guardar esa entrada en un fichero. Los secretos sólo van a Credential
Manager; .local/integrations.json conserva únicamente sus referencias.
No habilita conexiones remotas ni integra por sí mismo esos servicios.
"""
import json
import getpass
import os
import signal
import subprocess
import sys
import time
from pathlib import Path

from p005_real import cli, free_port, http, clean_env, EXE, REPO


def main() -> int:
    root = REPO / ".local" / "integration-data"
    reference_file = root.parent / "integrations.json"
    if reference_file.exists():
        saved = json.loads(reference_file.read_text(encoding="utf-8"))
        assert set(saved["credentials"]) == {"ai-broker", "agora"}
        assert all(c["backend"] == "WINDOWS_CREDENTIAL_MANAGER" and "secret" not in c for c in saved["credentials"].values())
        print("PASS: las dos referencias ya están enroladas en Credential Manager; se conservan.")
        return 0
    print("READY: entrada confidencial por stdin", flush=True)
    secret_input = getpass.getpass("") if sys.stdin.isatty() else sys.stdin.readline()
    secrets = json.loads(secret_input)
    del secret_input
    if set(secrets) != {"ai-broker", "agora"} or not all(isinstance(v, str) and v.strip() for v in secrets.values()):
        raise RuntimeError("Se requieren los dos tokens no vacíos.")
    principal = cli(root, "enroll-principal", "--name", "integration-enrollment", "--kind", "OPERATOR", "--scopes", "admin")
    port = free_port()
    daemon = subprocess.Popen([str(EXE), "--data-root", str(root), "--port", str(port), "--serve-api"],
                              env=clean_env(), stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                              creationflags=subprocess.CREATE_NEW_PROCESS_GROUP)
    references = {}
    try:
        base = f"http://127.0.0.1:{port}/api/v1"
        for _ in range(150):
            try:
                if http("GET", f"{base}/health/live")[0] == 200:
                    break
            except OSError:
                pass
            if daemon.poll() is not None:
                raise RuntimeError("El daemon de enrolamiento no pudo arrancar.")
            time.sleep(0.2)
        for name in ("ai-broker", "agora"):
            body = {"service": f"larm/integration/{name}", "username": "operator",
                    "secret_type": "TOKEN", "secret": secrets[name], "scopes": []}
            status, _, reference = http("POST", f"{base}/credentials", principal["token"],
                                        json.dumps(body).encode(), {"content-type": "application/json", "idempotency-key": f"enroll-{name}"})
            if status != 201 or "secret" in reference:
                raise RuntimeError(f"No se pudo enrolar {name}; HTTP {status}.")
            references[name] = reference
        reference_file.write_text(json.dumps({"data_root": str(root), "credentials": references}, indent=2) + "\n", encoding="utf-8")
        logs = "".join(p.read_text(encoding="utf-8", errors="replace") for p in root.rglob("*.log"))
        database = (root / "state/larm.sqlite").read_bytes()
        for secret in secrets.values():
            if secret in logs or secret.encode() in database:
                raise RuntimeError("Se detectó texto confidencial fuera de Credential Manager.")
        print("PASS: dos referencias creadas; secretos ausentes de SQLite y logs.")
        print(f"Referencias: {reference_file.relative_to(REPO)}")
        return 0
    finally:
        secrets.clear()
        if daemon.poll() is None:
            os.kill(daemon.pid, signal.CTRL_BREAK_EVENT)
            try:
                daemon.wait(timeout=20)
            except subprocess.TimeoutExpired:
                daemon.kill()
                daemon.wait(timeout=10)
        cli(root, "revoke-principal", "--name", "integration-enrollment")


if __name__ == "__main__":
    sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    try:
        raise SystemExit(main())
    except Exception as exc:
        # No imprimir entrada ni cuerpos HTTP: podrían contener secretos.
        print(f"FAIL: {type(exc).__name__}")
        raise SystemExit(1)
