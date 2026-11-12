#!/usr/bin/env python3
"""P015 - Prueba real de StorageEstimate y preflight de espacio en el PC IA.

  python tools/p015_real.py preflight

Sobre una raíz de datos de prueba con dos ubicaciones reales en C: (creadas por la API
de P014) y el espacio libre real del volumen (ctypes GetDiskFreeSpaceExW como oráculo):

  1. estimate EXACT: utilizable = libre - mayor reserva de las raíces, requerido =
     pico + max(1 GiB, 5 %), cabe;
  2. espacio insuficiente: una descarga del tamaño del libre no cabe en ninguna y dice cuánto falta;
  3. ESTIMATED/rango: se reserva por la cota superior;
  4. UNKNOWN: bloquea; con presupuesto duro del operador, cabe;
  5. ejemplo del contrato a escala: fuente existente 30 MB + temporal 18 + salida 9 -> pico nuevo 27, existente 30;
  6. desbordamiento: tamaños que no caben en 64 bits -> size_overflow;
  7. admisión real: reserva en volume_reservation; otro trabajo que no cabe -> STORAGE_INSUFFICIENT;
  8. cuatro procesos admiten a la vez el 40 % del utilizable: como mucho dos entran.
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
import tempfile
import time
import urllib.request
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
EXE = REPO / "target/debug/larm-daemon.exe"
RUNS = REPO / "docs/reports/P015_runs"
GIB = 1 << 30
MB = 1_000_000


def clean_env() -> dict:
    return {k: v for k, v in os.environ.items() if not k.startswith("LARM_")}


def step(log: list, name: str, ok: bool, detail: str) -> None:
    log.append({"step": name, "result": "PASS" if ok else "FAIL", "detail": detail})
    print(f"  [{'PASS' if ok else 'FAIL'}] {name}: {detail}")
    if not ok:
        raise AssertionError(f"{name}: {detail}")


def free_c() -> int:
    available, total = ctypes.c_uint64(), ctypes.c_uint64()
    ctypes.windll.kernel32.GetDiskFreeSpaceExW("C:\\", ctypes.byref(available), ctypes.byref(total), None)
    return available.value


def item(name, kind, size, volume="$destination", existing=False):
    return {"name": name, "kind": kind, "volume_key": volume, "size": size, "existing": existing}


def exact(n):
    return {"bytes": n, "upper_bound": n, "confidence": "EXACT"}


def preflight(data: Path, op: dict, location: str | None = None, job: str | None = None) -> tuple[int, dict | str]:
    with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False, encoding="utf-8") as f:
        json.dump(op, f)
        path = f.name
    try:
        args = [str(EXE), "--data-root", str(data), "storage-preflight", "--file", path]
        if location:
            args += ["--location", location, "--job", job]
        p = subprocess.run(args, capture_output=True, env=clean_env(), timeout=180)
        out = p.stdout.decode("utf-8")
        return p.returncode, (json.loads(out) if p.returncode == 0 else p.stderr.decode("utf-8", "replace"))
    finally:
        os.unlink(path)


def create_locations(data: Path, roots: Path) -> dict:
    token = json.loads(subprocess.run([str(EXE), "--data-root", str(data), "enroll-principal", "--name", "p015", "--kind", "OPERATOR",
                                       "--scopes", "read,storage:write"], capture_output=True, env=clean_env(), check=True).stdout)["token"]
    # Mantener ONLINE la ubicación de prueba aunque el volumen del host esté
    # bajo el umbral operativo por defecto. P014 cubre LOW_SPACE con el umbral
    # real; P015 necesita medir capacidad/admisión en un destino utilizable.
    config = data / "p015-test-config.json"
    config.write_text(json.dumps({"storage": {"low_disk_thresholds": {
        "mode": "PERCENT", "warning_basis_points": 1, "critical_basis_points": 0,
    }}}), encoding="utf-8")
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        port = s.getsockname()[1]
    proc = subprocess.Popen([str(EXE), "--data-root", str(data), "--config", str(config), "--port", str(port), "--serve-api"], env=clean_env(),
                            creationflags=subprocess.CREATE_NEW_PROCESS_GROUP, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    ids = {}
    try:
        base = f"http://127.0.0.1:{port}/api/v1"
        for _ in range(100):
            try:
                urllib.request.urlopen(f"{base}/health/live", timeout=2)
                break
            except OSError:
                time.sleep(0.2)
        for name, reserved in (("modelos", 10 * GIB), ("cache", 1 * GIB)):
            (roots / name).mkdir(parents=True, exist_ok=True)
            body = json.dumps({"name": name, "root_path": str(roots / name), "reserved_bytes": str(reserved), "priority": 1,
                               "auto_placement": True, "performance_class": "UNKNOWN"}).encode()
            req = urllib.request.Request(f"{base}/storage-locations", data=body, method="POST",
                                         headers={"authorization": f"Bearer {token}", "content-type": "application/json",
                                                  "idempotency-key": f"p015-{name}"})
            with urllib.request.urlopen(req, timeout=60) as r:
                ids[name] = json.loads(r.read())["id"]
    finally:
        os.kill(proc.pid, signal.CTRL_BREAK_EVENT)
        proc.wait(timeout=30)
        subprocess.run([str(EXE), "--data-root", str(data), "revoke-principal", "--name", "p015"], capture_output=True, env=clean_env())
    return ids


def option(result: dict, location_id: str) -> dict:
    return next(o for o in result["options"] if o["location_id"] == location_id)


def run() -> str:
    run_dir = RUNS / "real"
    shutil.rmtree(run_dir, ignore_errors=True)
    data, roots = run_dir / "data", run_dir / "roots"
    roots.mkdir(parents=True)
    log: list = []
    ids = create_locations(data, roots)
    db = data / "state/larm.sqlite"

    code, r = preflight(data, {"phases": [{"name": "descargar", "items": [item("m.gguf", "DOWNLOAD", exact(1 * GIB))]}]})
    free = free_c()
    m = option(r, ids["modelos"])
    pv = m["estimate"]["per_volume"][0]
    usable, required = int(pv["usable_free_bytes"]), int(pv["peak_extra_bytes"]) + int(pv["margin_bytes"])
    step(log, "exact-cabe", code == 0 and m["eligible"] and m["estimate"]["confidence"] == "EXACT"
         and int(pv["margin_bytes"]) == GIB and abs(usable - (free - 10 * GIB)) < 256 * 2**20,
         f"utilizable {usable} ≈ libre {free} (ctypes) - mayor reserva 10 GiB; requerido {required} = 1 GiB + margen 1 GiB")
    step(log, "ambas-raices-mismo-presupuesto", int(option(r, ids["cache"])["estimate"]["per_volume"][0]["usable_free_bytes"]) == usable,
         "la raíz con reserva de 1 GiB ve el mismo utilizable: el mínimo del volumen es la mayor reserva")

    code, r = preflight(data, {"phases": [{"name": "descargar", "items": [item("enorme.gguf", "DOWNLOAD", exact(free))]}]})
    m = option(r, ids["modelos"])
    step(log, "insuficiente", code == 0 and not m["eligible"] and m["reason"].startswith("insufficient_space")
         and not any(o["eligible"] for o in r["options"]), m["reason"][:120])

    rng = {"bytes": 1 * GIB, "upper_bound": 3 * GIB, "confidence": "ESTIMATED"}
    code, r = preflight(data, {"phases": [{"name": "descargar", "items": [item("rango.gguf", "DOWNLOAD", rng)]}]})
    e = option(r, ids["modelos"])["estimate"]
    step(log, "rango-por-cota-superior", e["confidence"] == "ESTIMATED" and int(e["peak_extra_bytes"]) == 3 * GIB,
         f"ESTIMATED 1..3 GiB reserva {e['peak_extra_bytes']} (la cota)")

    unknown = {"bytes": None, "upper_bound": None, "confidence": "UNKNOWN"}
    op = {"phases": [{"name": "descargar", "items": [item("x.bin", "DOWNLOAD", unknown)]}]}
    _, r = preflight(data, op)
    blocked = option(r, ids["modelos"])
    op["hard_budget_bytes"] = str(2 * GIB)
    _, r = preflight(data, op)
    budget = option(r, ids["modelos"])
    step(log, "unknown-y-presupuesto", not blocked["eligible"] and blocked["reason"] == "size_unknown" and budget["eligible"]
         and int(budget["estimate"]["peak_extra_bytes"]) == 2 * GIB,
         "UNKNOWN sin cota bloquea; con presupuesto duro de 2 GiB cabe y reserva 2 GiB")

    guid = option(r, ids["modelos"])["volume_key"]
    transform = {"phases": [
        {"name": "convertir", "items": [item("fuente", "DOWNLOAD", exact(30 * MB), guid, True), item("temp", "TEMPORARY", exact(18 * MB)),
                                        item("final", "FINAL_ARTIFACT", exact(9 * MB))]},
        {"name": "terminar", "items": [item("fuente", "DOWNLOAD", exact(30 * MB), guid, True), item("final", "FINAL_ARTIFACT", exact(9 * MB))]},
    ]}
    _, r = preflight(data, transform)
    e = option(r, ids["modelos"])["estimate"]
    step(log, "ejemplo-30-18-9", int(e["peak_extra_bytes"]) == 27 * MB and int(e["existing_bytes"]) == 30 * MB
         and int(e["temporary_peak_bytes"]) == 18 * MB and int(e["final_artifact_bytes"]) == 9 * MB,
         "fuente existente 30 MB + temporal 18 + salida 9: total 57, nuevo 27 (contrato 02)")

    big = {"phases": [{"name": "x", "items": [item("a", "DOWNLOAD", exact(9_000_000_000_000_000_000)), item("b", "DOWNLOAD", exact(9_000_000_000_000_000_000))]}]}
    code, err = preflight(data, big)
    step(log, "desbordamiento", code == 1 and "size_overflow" in err, "9e18 + 9e18 bytes: size_overflow, sin truncar")

    share = usable // 2 + GIB
    op = {"phases": [{"name": "descargar", "items": [item("a.gguf", "DOWNLOAD", exact(share))]}]}
    code_a, _ = preflight(data, op, ids["modelos"], "job-a")
    code_b, err_b = preflight(data, op, ids["cache"], "job-b")
    with sqlite3.connect(db) as c:
        rows = c.execute("SELECT job_id, volume_key, total_bytes FROM volume_reservation").fetchall()
    step(log, "admision-real", code_a == 0 and code_b == 1 and "destination_not_eligible" in err_b
         and [r[0] for r in rows] == ["job-a"] and rows[0][1] == guid,
         f"job-a reserva {rows[0][2] if rows else 0} bytes en {guid[:8]}…; job-b (otra raíz del mismo volumen) no cabe")
    with sqlite3.connect(db) as c:
        c.execute("DELETE FROM volume_reservation")

    piece = usable * 40 // 100
    op = {"phases": [{"name": "descargar", "items": [item("p.gguf", "DOWNLOAD", exact(piece))]}]}
    with ThreadPoolExecutor(max_workers=4) as pool:
        codes = list(pool.map(lambda i: preflight(data, op, ids["modelos"], f"concurrente-{i}")[0], range(4)))
    with sqlite3.connect(db) as c:
        reserved = c.execute("SELECT COUNT(*), COALESCE(SUM(total_bytes), 0) FROM volume_reservation").fetchone()
        c.execute("DELETE FROM volume_reservation")
    step(log, "admisiones-concurrentes", codes.count(0) == reserved[0] and 1 <= reserved[0] <= 2 and reserved[1] <= usable,
         f"4 procesos piden el 40 %: {codes.count(0)} admitidos, {reserved[1]} bytes reservados ≤ utilizable {usable}")
    (RUNS / "steps.json").write_text(json.dumps(log, indent=2, ensure_ascii=False), encoding="utf-8")
    return f"{len(log)} pasos PASS"


if __name__ == "__main__":
    sys.stdout.reconfigure(encoding="utf-8")
    try:
        print("PASS " + {"preflight": run}[sys.argv[1]]())
        raise SystemExit(0)
    except (AssertionError, KeyError, OSError, ValueError, StopIteration, TypeError, subprocess.SubprocessError, sqlite3.Error) as e:
        print(f"FAIL {type(e).__name__}: {e}")
        raise SystemExit(1)
