#!/usr/bin/env python3
"""Regresión real de INC-01/04/06 y preparación del escritorio para INC-02/03/05/07.

--ui mantiene el daemon y el escritorio hasta crear el fichero close-ui en
la carpeta de esta ejecución. Cada ejecución usa datos nuevos y revoca su
principal. No carga modelos ni cambia los servicios Broker/Ollama del usuario.
"""
import argparse
import hashlib
import json
import os
import signal
import sqlite3
import subprocess
import sys
import time
import uuid

from p005_real import REPO, EXE, cli, http, free_port, clean_env, step
from p014_real import body


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--ui", action="store_true")
    parser.add_argument("--desktop-smoke", action="store_true")
    args = parser.parse_args()
    out = REPO / "docs/reports/AUDIT_FIXES_2026-10-16_runs"
    run = out / uuid.uuid4().hex[:12]
    data = run / "data"
    folders = [run / "modelos ñoño ™", run / "otra carpeta"]
    for folder in folders:
        folder.mkdir(parents=True)
    config = json.loads((REPO.parent / "Auditoria_LARM_2026-10-15/bridge_config.json").read_text(encoding="utf-8"))
    port = free_port()
    config["adoption"]["gateway_base_url"] = f"http://127.0.0.1:{port}/api/v1/gateway"
    config_path = run / "bridge.json"
    config_path.write_text(json.dumps(config), encoding="utf-8")
    token = cli(data, "enroll-principal", "--name", "audit-regression", "--kind", "OPERATOR", "--scopes", "admin")["token"]
    daemon = subprocess.Popen([str(EXE), "--data-root", str(data), "--port", str(port), "--serve-api", "--bridge-config", str(config_path)],
                              env=clean_env(), stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                              creationflags=subprocess.CREATE_NEW_PROCESS_GROUP)
    desktop = None
    steps = []
    base = f"http://127.0.0.1:{port}/api/v1"
    try:
        for _ in range(200):
            try:
                if http("GET", f"{base}/health/live")[0] == 200:
                    break
            except OSError:
                pass
            assert daemon.poll() is None, "daemon terminó durante el arranque"
            time.sleep(0.2)
        status, headers, result = http("POST", f"{base}/health/live")
        step(steps, "INC-04-metodo", status == 405 and headers.get("allow") == "GET, HEAD" and result["details"]["reason"] == "method_not_allowed",
             "POST health/live: 405 + Allow GET, HEAD + envelope")
        st, _, location = http("POST", f"{base}/storage-locations", token, json.dumps(body("Modelos de prueba", str(folders[0]), 0)).encode(),
                               {"content-type": "application/json", "idempotency-key": "valid"})
        assert st == 201, (st, location)
        capacity = int(location["capacity_bytes"])
        db = data / "state/larm.sqlite"
        def counters():
            with sqlite3.connect(db) as conn:
                return tuple(conn.execute(f"SELECT COUNT(*) FROM {table}").fetchone()[0] for table in ("storage_location", "event"))
        before = counters()
        st, _, error = http("POST", f"{base}/storage-locations", token, json.dumps(body("Imposible", str(folders[1]), capacity + 1)).encode(),
                            {"content-type": "application/json", "idempotency-key": "invalid"})
        step(steps, "INC-01-alta-sin-efectos", st == 400 and error["details"]["reason"] == "reservation_exceeds_capacity" and counters() == before,
             "capacidad + 1: 400, sin fila ni evento")
        st, _, error = http("PATCH", f"{base}/storage-locations/{location['id']}", token,
                            json.dumps(body("Imposible", str(folders[0]), capacity + 1)).encode(),
                            {"content-type": "application/json", "if-match": str(location["row_version"])})
        _, _, unchanged = http("GET", f"{base}/storage-locations/{location['id']}", token)
        step(steps, "INC-01-edicion-sin-efectos", st == 400 and error["details"]["reason"] == "reservation_exceeds_capacity" and counters() == before
             and unchanged["row_version"] == location["row_version"] and unchanged["reserved_bytes"] == "0",
             "edición imposible: 400, sin cambiar revisión, datos, evento ni presupuesto")
        status, _, receipt = http("POST", f"{base}/hardware/refresh", token, b'{"resource_id":null,"profile_id":null}',
                                  {"content-type": "application/json", "idempotency-key": "hardware"})
        assert status == 202, receipt
        for _ in range(240):
            _, _, job = http("GET", f"{base}/jobs/{receipt['job_id']}", token)
            if job["state"] in ("COMPLETED", "FAILED"):
                break
            time.sleep(0.5)
        assert job["state"] == "COMPLETED", job
        status, _, page = http("GET", f"{base}/hardware", token)
        assert status == 200 and page["items"], "sin perfil real"
        profile = max(page["items"], key=lambda p: (p["created_at"], p["id"]))
        status, _, catalog = http("GET", f"{base}/broker/catalog", token)
        assert status == 200 and len(catalog["items"]) == 1, "catálogo del runtime real vacío"
        components = catalog["items"][0]["fingerprint"]["components"]
        hardware = components["hardware_fingerprint"]
        step(steps, "INC-06-hardware-real", hardware["status"] == "VERIFIED" and hardware["value"] == profile["hardware_fingerprint"]
             and profile["id"] in hardware["evidence_refs"][0], "catálogo igual al perfil persistido y trazable a su revisión")
        drivers = components["driver_versions"]
        devices = profile["snapshot"]["devices"]
        if devices and all(d["driver_version"] for d in devices):
            observed = json.loads(drivers["value"])
            expected = {(d["kind"], d["device_id"], d["driver_version"]) for d in devices}
            step(steps, "INC-06-drivers-reales", drivers["status"] == "VERIFIED" and {(d["kind"], d["device_id"], d["driver_version"]) for d in observed} == expected,
                 f"{len(observed)} drivers iguales a la observación del hardware")
        else:
            step(steps, "INC-06-drivers-desconocidos", drivers["status"] == "UNAVAILABLE" and drivers["value"] is None,
                 "versiones incompletas no se publican como verificadas")
        (run / "hardware.json").write_text(json.dumps(profile, indent=2), encoding="utf-8")
        (run / "catalog.json").write_text(json.dumps(catalog, indent=2), encoding="utf-8")
        if args.ui or args.desktop_smoke:
            desktop_exe = REPO / "apps/desktop/src-tauri/target/debug/larm-desktop.exe"
            desktop = subprocess.Popen([str(desktop_exe)], env=clean_env(), stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            time.sleep(3)
            assert desktop.poll() is None, "escritorio no permanece abierto"
            second = subprocess.run([str(desktop_exe)], env=clean_env() | {"WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS": "--disable-gpu"},
                                    capture_output=True, timeout=20)
            message = second.stderr.decode("utf-8", errors="replace")
            step(steps, "INC-07-segunda-instancia", second.returncode == 0 and "ya está abierto" in message and "panicked" not in message and desktop.poll() is None,
                 "segunda instancia con flags distintos: salida 0 y mensaje claro; primera ventana sigue abierta")
            if args.ui:
                print(f"UI_READY {run}", flush=True)
                (out / "latest-ui.json").write_text(json.dumps({"run": str(run), "base": base, "daemon_pid": daemon.pid, "desktop_pid": desktop.pid}), encoding="utf-8")
                deadline = time.monotonic() + 900
                while not (run / "close-ui").exists() and time.monotonic() < deadline:
                    time.sleep(0.5)
        print(f"PASS: {len(steps)} comprobaciones reales", flush=True)
        return 0
    finally:
        if desktop is not None and desktop.poll() is None:
            desktop.terminate()
            desktop.wait(timeout=15)
        if daemon.poll() is None:
            os.kill(daemon.pid, signal.CTRL_BREAK_EVENT)
            try:
                daemon.wait(timeout=30)
            except subprocess.TimeoutExpired:
                daemon.kill()
                daemon.wait(timeout=10)
        for name in ("audit-regression", "desktop"):
            subprocess.run([str(EXE), "--data-root", str(data), "revoke-principal", "--name", name], capture_output=True, env=clean_env())
        record = {"steps": steps, "daemon_sha256": hashlib.sha256(EXE.read_bytes()).hexdigest(), "run": str(run.relative_to(REPO))}
        (run / "steps.json").write_text(json.dumps(record, indent=2, ensure_ascii=False), encoding="utf-8")
        (out / "latest.json").write_text(json.dumps(record, indent=2, ensure_ascii=False), encoding="utf-8")


if __name__ == "__main__":
    sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    raise SystemExit(main())
