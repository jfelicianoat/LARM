#!/usr/bin/env python3
"""P014 - Prueba real del registro de StorageLocation con el daemon en el PC IA.

  python tools/p014_real.py serve

Sobre carpetas de prueba (docs/reports/P014_runs/real/roots en C: y una carpeta
temporal dentro del recurso existente \\\\localhost\\Temp, que se borra al final):

  1. alta con identidad real: GUID de C:, ruta canónica, capacidad exacta (ctypes),
     clase NVME observada; 201 idempotente;
  2. rechazos antes de guardar: carpeta inexistente, fichero, junction (mklink /J),
     ruta relativa, ADS y solapamiento con otra ubicación;
  3. dos raíces del mismo volumen comparten presupuesto: usable = libre - mayor reserva;
     una reserva mayor que el libre deja usable 0 (nunca negativo);
  4. ruta de red: volume_key \\\\localhost\\temp, sin GUID, storage_type NETWORK;
  5. MISSING real: se borra la carpeta y POST /probe la marca; al recrearla vuelve ONLINE;
  6. PATCH sin If-Match 428, con versión vieja 409; cambio de raíz con artefactos 409;
  7. DELETE con artefactos 409 y sin ellos 204, con eventos storage.state_changed;
  8. LOW_SPACE real: segundo arranque con umbrales BYTES por encima del libre de C:.
"""
import ctypes
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
RUNS = REPO / "docs/reports/P014_runs"
SHARE = r"\\localhost\Temp"


def clean_env() -> dict:
    return {k: v for k, v in os.environ.items() if not k.startswith("LARM_")}


def step(log: list, name: str, ok: bool, detail: str) -> None:
    log.append({"step": name, "result": "PASS" if ok else "FAIL", "detail": detail})
    print(f"  [{'PASS' if ok else 'FAIL'}] {name}: {detail}")
    if not ok:
        raise AssertionError(f"{name}: {detail}")


def free_space(root: str) -> tuple[int, int]:
    available, total = ctypes.c_uint64(), ctypes.c_uint64()
    ctypes.windll.kernel32.GetDiskFreeSpaceExW(ctypes.c_wchar_p(root), ctypes.byref(available), ctypes.byref(total), None)
    return total.value, available.value


def c_guid() -> str:
    buf = ctypes.create_unicode_buffer(64)
    ctypes.windll.kernel32.GetVolumeNameForVolumeMountPointW("C:\\", buf, 64)
    return buf.value.split("{")[1].split("}")[0].lower()


def http(method: str, url: str, token: str, body=None, headers: dict | None = None) -> tuple[int, dict | None]:
    data = (body if isinstance(body, bytes) else json.dumps(body).encode()) if body is not None else None
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


def start(root: Path, config: Path | None = None) -> tuple[subprocess.Popen, str]:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        port = s.getsockname()[1]
    args = [str(EXE), "--data-root", str(root), "--port", str(port), "--serve-api"]
    if config:
        args[1:1] = ["--config", str(config)]
    proc = subprocess.Popen(args, env=clean_env(), creationflags=subprocess.CREATE_NEW_PROCESS_GROUP,
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


def body(name: str, root: str, reserved: int = 0, priority: int = 1) -> dict:
    return {"name": name, "root_path": root, "reserved_bytes": str(reserved), "priority": priority,
            "auto_placement": True, "performance_class": "UNKNOWN"}


def wait_job(base: str, token: str, job_id: str) -> dict:
    for _ in range(240):
        _, job = http("GET", f"{base}/jobs/{job_id}", token)
        if job["state"] in ("COMPLETED", "FAILED"):
            return job
        time.sleep(0.5)
    raise AssertionError("el job no terminó")


def serve() -> str:
    run = RUNS / "real"
    shutil.rmtree(run, ignore_errors=True)
    roots = run / "roots"
    (roots / "modelos").mkdir(parents=True)
    (roots / "cache").mkdir()
    (roots / "fichero.txt").write_text("x", encoding="utf-8")
    share_dir = Path(SHARE) / f"larm-p014-{uuid.uuid4().hex[:8]}"
    share_dir.mkdir()
    log: list = []
    proc = None
    data = run / "data"
    try:
        token = json.loads(subprocess.run([str(EXE), "--data-root", str(data), "enroll-principal", "--name", "p014",
                                           "--kind", "OPERATOR", "--scopes", "read,storage:write,admin"],
                                          capture_output=True, env=clean_env(), check=True).stdout)["token"]
        proc, base = start(data)
        guid = c_guid()

        status, a = http("POST", f"{base}/storage-locations", token, body("Modelos", str(roots / "modelos"), 1 << 30, 2), {"idempotency-key": "a"})
        total, _ = free_space(str(roots / "modelos"))
        step(log, "alta-identidad-real", status == 201 and a["volume_guid"] == guid and a["volume_key"] == guid
             and a["drive_letter"] == "C:" and int(a["capacity_bytes"]) == total and a["performance_class"] == "NVME"
             and a["metadata"]["performance_class_source"] == "observed" and a["state"] == "ONLINE" and a["last_seen_at"],
             f"201: GUID {guid[:8]}… (GetVolumeNameForVolumeMountPointW), capacidad {total} exacta, NVME observada")
        replay = http("POST", f"{base}/storage-locations", token, body("Modelos", str(roots / "modelos"), 1 << 30, 2), {"idempotency-key": "a"})
        step(log, "alta-idempotente", replay == (201, a), "misma clave y cuerpo: misma respuesta y una sola fila")

        junction = roots / "enlace"
        subprocess.run(["cmd", "/C", "mklink", "/J", str(junction), str(roots / "cache")], capture_output=True, check=True)
        rejects = [
            ("inexistente", str(roots / "no-existe"), 400, "root_not_found"),
            ("fichero", str(roots / "fichero.txt"), 400, "root_not_directory"),
            ("junction", str(junction), 400, "reparse_point_rejected"),
            ("relativa", "modelos", 400, "root_path_invalid"),
            ("ads", str(roots / "modelos") + ":flujo", 400, "root_path_invalid"),
            ("solapada", str(roots / "modelos" / "dentro"), None, None),
        ]
        (roots / "modelos" / "dentro").mkdir()
        rejects[-1] = ("solapada", str(roots / "modelos" / "dentro"), 409, "root_overlap")
        bad = []
        for i, (what, root, code, reason) in enumerate(rejects):
            st, err = http("POST", f"{base}/storage-locations", token, body(what, root), {"idempotency-key": f"r{i}"})
            if st != code or err["details"]["reason"] != reason:
                bad.append(f"{what}: {st} {err}")
        step(log, "rechazos-antes-de-guardar", not bad, bad[0] if bad else "inexistente, fichero, junction, relativa, ADS y solapada rechazadas sin guardar")

        status, b = http("POST", f"{base}/storage-locations", token, body("Cache", str(roots / "cache"), 4 << 30), {"idempotency-key": "b"})
        _, page = http("GET", f"{base}/storage-locations", token)
        items = {i["name"]: i for i in page["items"]}
        # El libre del volumen es la observación más reciente entre sus raíces.
        free = int(max(items.values(), key=lambda i: i["last_seen_at"] or "")["free_bytes"])
        usable = int(items["Modelos"]["metadata"]["volume_budget"]["usable_bytes"])
        step(log, "presupuesto-compartido", status == 201 and len(items) == 2 and usable == free - (4 << 30)
             and items["Modelos"]["metadata"]["volume_budget"]["shared_with"] == [b["id"]]
             and int(items["Cache"]["metadata"]["volume_budget"]["usable_bytes"]) == usable,
             f"dos raíces en C:: utilizable {usable} = libre {free} - mayor reserva (4 GiB), no la suma")

        status, rejected = http("PATCH", f"{base}/storage-locations/{b['id']}", token, body("Cache", str(roots / "cache"), total + 1), {"if-match": str(b["row_version"])})
        _, cache = http("GET", f"{base}/storage-locations/{b['id']}", token)
        step(log, "reserva-mayor-que-capacidad-rechazada", status == 400 and rejected["details"]["reason"] == "reservation_exceeds_capacity"
             and cache["reserved_bytes"] == b["reserved_bytes"] and cache["row_version"] == b["row_version"],
             "capacidad + 1 byte: 400 sin cambiar la reserva, revisión ni presupuesto")
        status, edited = http("PATCH", f"{base}/storage-locations/{b['id']}", token, body("Cache", str(roots / "cache"), total), {"if-match": str(b["row_version"])})
        _, cache = http("GET", f"{base}/storage-locations/{b['id']}", token)
        step(log, "reserva-valida-mayor-que-el-libre", status == 200 and int(cache["metadata"]["volume_budget"]["usable_bytes"]) == 0,
             "reserva igual a capacidad: válida, utilizable 0 porque supera el libre")

        status, net = http("POST", f"{base}/storage-locations", token, body("Red", str(share_dir)), {"idempotency-key": "n"})
        step(log, "ruta-de-red", status == 201 and net["volume_key"] == SHARE.lower() and net["volume_guid"] is None
             and net["unc_path"] == SHARE and net["storage_type"] == "NETWORK" and net["state"] == "ONLINE",
             f"{share_dir}: volume_key {net and net['volume_key']}, sin GUID inventado")

        shutil.rmtree(roots / "modelos" / "dentro")
        os.rmdir(junction)
        shutil.rmtree(roots / "cache")
        nulls = {"resource_id": None, "profile_id": None}
        _, receipt = http("POST", f"{base}/storage-locations/{b['id']}/probe", token, nulls, {"idempotency-key": "p1"})
        job = wait_job(base, token, receipt["job_id"])
        _, cache = http("GET", f"{base}/storage-locations/{b['id']}", token)
        (roots / "cache").mkdir()
        _, receipt = http("POST", f"{base}/storage-locations/{b['id']}/probe", token, nulls, {"idempotency-key": "p2"})
        wait_job(base, token, receipt["job_id"])
        _, back = http("GET", f"{base}/storage-locations/{b['id']}", token)
        step(log, "missing-real", job["state"] == "COMPLETED" and job["resource_type"] == "storage_location"
             and cache["state"] == "MISSING" and back["state"] == "ONLINE",
             "carpeta borrada -> POST /probe (job) -> MISSING; recreada -> ONLINE")

        _, current = http("GET", f"{base}/storage-locations/{a['id']}", token)
        st1, e1 = http("PATCH", f"{base}/storage-locations/{a['id']}", token, body("Modelos", str(roots / "modelos")))
        st2, e2 = http("PATCH", f"{base}/storage-locations/{a['id']}", token, body("Modelos", str(roots / "modelos")), {"if-match": "1" if current["row_version"] != 1 else "9"})
        step(log, "if-match", st1 == 428 and st2 == 409 and e2["code"] == "STALE_REVISION", f"sin If-Match {st1}; versión vieja {st2}")

        db = data / "state/larm.sqlite"
        with sqlite3.connect(db) as conn:
            conn.execute("PRAGMA foreign_keys = OFF")
            conn.execute("INSERT INTO artifact_location (ownership, id, created_at, updated_at, row_version, metadata, artifact_id, "
                         "storage_location_id, relative_path, file_size, sha256, state, verified_at) VALUES "
                         "('LARM_MANAGED', ?, 't', 't', 1, '{}', 'a1', ?, 'blobs/x', 1, NULL, 'ONLINE', NULL)", (str(uuid.uuid7()), a["id"]))
        (roots / "otra").mkdir()
        st3, e3 = http("PATCH", f"{base}/storage-locations/{a['id']}", token, body("Modelos", str(roots / "otra")), {"if-match": str(current["row_version"])})
        st4, e4 = http("DELETE", f"{base}/storage-locations/{a['id']}", token, headers={"if-match": str(current["row_version"])})
        step(log, "artefactos-protegidos", st3 == 409 and e3["details"]["reason"] == "root_change_requires_migration"
             and st4 == 409 and e4["details"]["reason"] == "location_has_artifacts",
             "con un artefacto: cambiar la raíz 409 y borrar 409; la ubicación sigue")
        with sqlite3.connect(db) as conn:
            conn.execute("DELETE FROM artifact_location")
        st5, _ = http("DELETE", f"{base}/storage-locations/{a['id']}", token, headers={"if-match": str(current["row_version"])})
        st6, _ = http("DELETE", f"{base}/storage-locations/{net['id']}", token, headers={"if-match": str(net["row_version"])})
        with sqlite3.connect(db) as conn:
            actions = [r[0] for r in conn.execute(
                "SELECT json_extract(e.payload, '$.action') FROM event e JOIN event_outbox o ON o.event_record_id = e.id "
                "WHERE e.event_type = 'storage.state_changed' AND e.resource_id = ? ORDER BY e.event_id", (a["id"],))]
        step(log, "borrado-y-eventos", st5 == 204 and st6 == 204 and actions[0] == "created" and actions[-1] == "deleted",
             f"204; eventos de Modelos: {actions}")
        stop(proc)
        proc = None

        config = run / "low-space.json"
        _, c_free = free_space("C:\\")
        config.write_text(json.dumps({"storage": {"low_disk_thresholds": {
            "mode": "BYTES", "warning_bytes": str(c_free * 4), "critical_bytes": str(c_free * 2)}}}), encoding="utf-8")
        proc, base = start(data, config)
        deadline = time.monotonic() + 120
        while time.monotonic() < deadline:
            _, cache = http("GET", f"{base}/storage-locations/{b['id']}", token)
            if cache["state"] == "LOW_SPACE":
                break
            time.sleep(0.5)
        step(log, "low-space-real", cache["state"] == "LOW_SPACE",
             f"umbral crítico {c_free * 2} > libre de C: ({c_free}): el sondeo de arranque marca LOW_SPACE")
    finally:
        if proc is not None:
            stop(proc)
        shutil.rmtree(share_dir, ignore_errors=True)
        subprocess.run([str(EXE), "--data-root", str(data), "revoke-principal", "--name", "p014"], capture_output=True, env=clean_env())
        RUNS.mkdir(parents=True, exist_ok=True)
        (RUNS / "serve-steps.json").write_text(json.dumps(log, indent=2, ensure_ascii=False), encoding="utf-8")
    step(log, "limpieza", not share_dir.exists(), f"{share_dir} borrada")
    return f"{len(log)} pasos PASS"


if __name__ == "__main__":
    sys.stdout.reconfigure(encoding="utf-8")
    try:
        print("PASS " + {"serve": serve}[sys.argv[1]]())
        raise SystemExit(0)
    except (AssertionError, KeyError, OSError, ValueError, TypeError, subprocess.SubprocessError, sqlite3.Error) as e:
        print(f"FAIL {type(e).__name__}: {e}")
        raise SystemExit(1)
