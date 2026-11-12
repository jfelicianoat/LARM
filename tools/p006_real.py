#!/usr/bin/env python3
"""P006 - Prueba real del shell de escritorio contra el daemon en Windows.

  python tools/p006_real.py online    daemon --serve-api en marcha: la ventana real (WebView2)
                                      hace ping y pide health/live y health/ready a través del
                                      backend Tauri, que obtiene el token por el pipe de
                                      bootstrap. El token no aparece en la salida.
  python tools/p006_real.py offline   sin daemon: el backend informa el fallo y el frontend lo
                                      reporta como daemon inalcanzable (no se inventa estado).

Requiere `npx tauri build --debug --no-bundle` (binario con el frontend embebido).
"""
import json
import os
import shutil
import signal
import socket
import subprocess
import sys
import time
import urllib.request
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
DAEMON = REPO / "target/debug/larm-daemon.exe"
DESKTOP = REPO / "apps/desktop/src-tauri/target/debug/larm-desktop.exe"
ROOT = REPO / "docs/reports/P006_runs"


def clean_env() -> dict:
    return {k: v for k, v in os.environ.items() if not k.startswith("LARM_")}


def smoke() -> dict:
    p = subprocess.run([str(DESKTOP)], env=clean_env() | {"LARM_DESKTOP_SMOKE": "1"}, capture_output=True,
                       text=True, encoding="utf-8", errors="replace", timeout=120, check=False)
    line = next((ln for ln in p.stdout.splitlines() if ln.startswith("LARM_DESKTOP_SMOKE ")), None)
    if p.returncode != 0 or line is None:
        raise AssertionError(f"la ventana no informó (rc={p.returncode}): {(p.stdout + p.stderr)[-400:]}")
    return {"report": json.loads(line.split(" ", 1)[1]), "stdout": p.stdout}


def online() -> str:
    root = ROOT / "online"
    shutil.rmtree(root, ignore_errors=True)
    root.mkdir(parents=True)
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        port = s.getsockname()[1]
    daemon = subprocess.Popen([str(DAEMON), "--data-root", str(root), "--port", str(port), "--serve-api"],
                              env=clean_env(), creationflags=subprocess.CREATE_NEW_PROCESS_GROUP,
                              stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try:
        for _ in range(100):
            try:
                urllib.request.urlopen(f"http://127.0.0.1:{port}/api/v1/health/live", timeout=1)
                break
            except OSError:
                time.sleep(0.2)
        result = smoke()
        report = result["report"]
        expected = {"ping": "pong", "live": {"status": "LIVE", "schema_version": "1"},
                    "ready": {"status": "READY", "schema_version": "1"}}
        if report != expected:
            raise AssertionError(f"informe {report} != {expected}")
        listed = subprocess.run([str(DAEMON), "--data-root", str(root), "list-principals"], env=clean_env(),
                                capture_output=True, text=True, encoding="utf-8", check=True).stdout
        if '"name": "desktop"' not in listed:
            raise AssertionError("el bootstrap no creó el principal desktop")
        # El token del escritorio no aparece en la salida de la ventana ni en los logs.
        logs = "".join(f.read_text(encoding="utf-8", errors="replace") for f in (root / "logs").glob("*.log"))
        if any(len(tok) == 64 and tok in logs for tok in result["stdout"].split()):
            raise AssertionError("un token aparece en los logs")
        os.kill(daemon.pid, signal.CTRL_BREAK_EVENT)
        rc = daemon.wait(timeout=20)
        if rc != 0:
            raise AssertionError(f"daemon exit {rc}")
        return ("ventana real: ping=pong, health/live LIVE y health/ready READY vía backend Tauri + pipe de "
                "bootstrap (principal desktop creado); daemon exit 0")
    finally:
        if daemon.poll() is None:
            daemon.kill()
        subprocess.run([str(DAEMON), "--data-root", str(root), "revoke-principal", "--name", "desktop"],
                       env=clean_env(), capture_output=True, check=False)


def offline() -> str:
    report = smoke()["report"]
    for name in ("live", "ready"):
        if report.get(name, {}).get("error") != "DAEMON_UNREACHABLE":
            raise AssertionError(f"{name}: {report.get(name)}")
    if report.get("ping") != "pong":
        raise AssertionError(f"ping {report.get('ping')}")
    return "sin daemon: ping=pong y live/ready -> DAEMON_UNREACHABLE (pipe de bootstrap no disponible)"


if __name__ == "__main__":
    sys.stdout.reconfigure(encoding="utf-8")
    try:
        print("PASS " + {"online": online, "offline": offline}[sys.argv[1]]())
        raise SystemExit(0)
    except (AssertionError, KeyError, IndexError, OSError, json.JSONDecodeError, subprocess.SubprocessError) as e:
        print(f"FAIL {type(e).__name__}: {e}")
        raise SystemExit(1)
