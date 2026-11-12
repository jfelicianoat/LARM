#!/usr/bin/env python3
"""P001 - Prueba real del ciclo de vida del daemon en Windows.

Arranca el daemon en su propio grupo de procesos con una raiz de datos gestionada
(docs/reports/P001_runs/daemon-data/<modo>), espera a que escriba su log, le envia
CTRL_BREAK y exige salida limpia. Oraculo de P001: arranque, gestion de la senal de
consola y codigo 0 al cierre limpio, sin escribir fuera de la ubicacion de prueba.

  daemon_lifecycle.py            binario target/debug/larm-daemon.exe
  daemon_lifecycle.py --npm      script raiz `npm run daemon:dev` (cargo run -p larm-daemon)
"""
import io
import os
import shutil
import signal
import subprocess
import sys
import time
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
EXE = REPO / "target/debug/larm-daemon.exe"


def main(argv: list[str]) -> int:
    sys.stdout = io.TextIOWrapper(sys.stdout.buffer, encoding="utf-8", errors="replace")
    via_npm = "--npm" in argv
    data = REPO / "docs/reports/P001_runs/daemon-data" / ("npm" if via_npm else "exe")
    shutil.rmtree(data, ignore_errors=True)
    data.mkdir(parents=True)
    if via_npm:
        # node + npm-cli.js en vez de npm.cmd: el envoltorio .cmd convierte CTRL_BREAK en la
        # pregunta "terminar el trabajo por lotes" y sale con 0xC000013A, ajeno al daemon.
        node = shutil.which("node")
        npm_cli = Path(node).parent / "node_modules/npm/bin/npm-cli.js" if node else None
        if not npm_cli or not npm_cli.is_file():
            print("FAIL: no se encuentra node/npm-cli.js")
            return 1
        cmd = [node, str(npm_cli), "run", "daemon:dev", "--", "--data-root", str(data)]
        startup = 240.0  # incluye la compilacion incremental de cargo run
    else:
        if not EXE.exists():
            print(f"FAIL: no existe {EXE}; compilar antes (cargo build -p larm-daemon)")
            return 1
        cmd = [str(EXE), "--data-root", str(data)]
        startup = 20.0

    proc = subprocess.Popen(cmd, cwd=REPO, creationflags=subprocess.CREATE_NEW_PROCESS_GROUP,
                            stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
    try:
        deadline = time.monotonic() + startup
        logs = []
        while time.monotonic() < deadline and proc.poll() is None:
            logs = [p for p in data.rglob("*") if p.is_file()]
            if logs:
                break
            time.sleep(0.5)
        if proc.poll() is not None:
            out = proc.stdout.read().decode("utf-8", "replace") if proc.stdout else ""
            print(f"FAIL: el daemon termino antes de la senal (rc={proc.returncode})\n{out[-800:]}")
            return 1
        if not logs:
            print(f"FAIL: el daemon no escribio ningun log en {data} en {startup:.0f}s")
            return 1
        time.sleep(1.0)
        os.kill(proc.pid, signal.CTRL_BREAK_EVENT)
        rc = proc.wait(timeout=30)
        out = proc.stdout.read().decode("utf-8", "replace") if proc.stdout else ""
        # En modo npm la senal llega tambien a node/cmd/cargo, que terminan con
        # STATUS_CONTROL_C_EXIT por su manejador por defecto; el codigo 0 del daemon lo
        # demuestra el modo exe. Aqui el oraculo es el cierre limpio registrado por el daemon.
        wrapper_ctrl_exit = via_npm and rc == 0xC000013A
        if rc != 0 and not wrapper_ctrl_exit:
            print(f"FAIL: exit code {rc} != 0 tras CTRL_BREAK\n{out[-800:]}")
            return 1
        log_text = "".join(p.read_text(encoding="utf-8", errors="replace") for p in logs)
        if "cierre limpio" not in log_text:
            print(f"FAIL: exit 0 pero el log del daemon no registra el cierre limpio\n{log_text[-800:]}")
            return 1
        rel = [p.relative_to(data).as_posix() for p in logs]
        print(f"PASS: {'npm run daemon:dev' if via_npm else 'larm-daemon.exe'} vivo, log en raiz gestionada {rel}; "
              f"CTRL_BREAK -> {'daemon registra cierre limpio; envoltorio npm sale 0xC000013A' if wrapper_ctrl_exit else 'exit 0'} (pid={proc.pid})")
        return 0
    except subprocess.TimeoutExpired:
        print("FAIL: el daemon no cerro en 30 s tras CTRL_BREAK")
        return 1
    finally:
        if proc.poll() is None:
            subprocess.run(["taskkill", "/T", "/F", "/PID", str(proc.pid)], capture_output=True, check=False)


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
