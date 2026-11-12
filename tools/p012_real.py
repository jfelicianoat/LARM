#!/usr/bin/env python3
"""P012 - Pruebas reales del Storage Profiler y la identidad de volumen en el PC IA.

  python tools/p012_real.py windows   enumeración real frente a oráculos independientes
  python tools/p012_real.py serve     último estado de storage_location con el daemon real

Oráculos de `windows` (sin código de LARM):
  - `mountvol`: todos los Volume GUID del sistema y sus puntos de montaje;
  - `Get-Volume`: sistema de ficheros y tamaño de los volúmenes con letra;
  - `Get-PhysicalDisk`: bus (NVMe) y medio (SSD) del disco;
  - ctypes `GetDiskFreeSpaceExW`: capacidad exacta y espacio libre de C: y de
    `\\\\localhost\\Temp` (un recurso compartido existente; no se crea ninguno);
  - un servidor UNC inexistente queda no disponible y sin datos inventados.

`serve` registra filas de `storage_location` directamente en la base de una raíz de
prueba (la API CRUD es de P014) y comprueba que el sondeo de arranque y un refresco
las actualizan: ONLINE con capacidad real, OFFLINE para un volumen que no existe,
la ruta de red disponible, eventos `storage.state_changed` en el outbox y, en un
segundo sondeo sin cambios, `free_bytes`/`last_seen_at` sin subir `row_version`.
"""
import ctypes
import json
import os
import re
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
RUNS = REPO / "docs/reports/P012_runs"
SHARE = r"\\localhost\Temp"
MISSING_SHARE = r"\\noexiste-larm\modelos"
GHOST = "00000000-1111-4222-8333-444444444444"


def clean_env() -> dict:
    return {k: v for k, v in os.environ.items() if not k.startswith("LARM_")}


def step(log: list, name: str, ok: bool, detail: str) -> None:
    log.append({"step": name, "result": "PASS" if ok else "FAIL", "detail": detail})
    print(f"  [{'PASS' if ok else 'FAIL'}] {name}: {detail}")
    if not ok:
        raise AssertionError(f"{name}: {detail}")


def daemon(*args: str, root: Path | None = None) -> dict:
    cmd = [str(EXE)] + (["--data-root", str(root)] if root else []) + list(args)
    p = subprocess.run(cmd, capture_output=True, env=clean_env(), timeout=180, check=False)
    if p.returncode != 0:
        raise AssertionError(f"{args[0]}: exit {p.returncode}: {p.stderr.decode('utf-8', 'replace')[-400:]}")
    return json.loads(p.stdout.decode("utf-8"))


def powershell(script: str) -> str:
    return subprocess.run(["powershell", "-NoProfile", "-Command", script], capture_output=True, check=True,
                          timeout=120).stdout.decode("utf-8", "replace")


def free_space(root: str) -> tuple[int, int] | None:
    available, total = ctypes.c_uint64(), ctypes.c_uint64()
    ok = ctypes.windll.kernel32.GetDiskFreeSpaceExW(ctypes.c_wchar_p(root), ctypes.byref(available), ctypes.byref(total), None)
    return (total.value, available.value) if ok else None


def mountvol() -> dict[str, list[str]]:
    out = subprocess.run(["mountvol"], capture_output=True, check=False).stdout.decode("cp850", "replace")
    volumes: dict[str, list[str]] = {}
    current = None
    for line in out.splitlines():
        m = re.search(r"\\\\\?\\Volume\{([0-9a-fA-F-]{36})\}\\", line)
        if m:
            current = m.group(1).lower()
            volumes[current] = []
        elif current and re.match(r"^\s+[A-Za-z]:\\", line):
            volumes[current].append(line.strip())
        elif current and line.strip().startswith("***"):
            pass
    return volumes


def windows() -> str:
    RUNS.mkdir(parents=True, exist_ok=True)
    log: list = []
    obs = daemon("storage-probe", "--unc", SHARE, "--unc", MISSING_SHARE)
    (RUNS / "storage.json").write_text(json.dumps(obs, indent=1, ensure_ascii=False), encoding="utf-8")
    local = {v["identity"]["volume_guid"]: v for v in obs["volumes"] if v["identity"]["volume_guid"]}
    oracle = mountvol()
    step(log, "guid-frente-a-mountvol", set(local) == set(oracle),
         f"{len(local)} volúmenes; mountvol: {sorted(oracle)}")
    mounts_ok = all(sorted(local[g]["identity"]["mount_points"]) == sorted(m) for g, m in oracle.items())
    step(log, "puntos-de-montaje", mounts_ok,
         "; ".join(f"{g[:8]}… {m or 'sin montar'}" for g, m in oracle.items()))
    sin_letra = [g for g, v in local.items() if v["identity"]["drive_letter"] is None]
    step(log, "volumenes-sin-letra", len(sin_letra) == sum(1 for m in oracle.values() if not any(re.match(r"^[A-Z]:\\$", x) for x in m)),
         f"{len(sin_letra)} volúmenes sin letra identificados por GUID")

    gv = json.loads(powershell("@(Get-Volume | Where-Object DriveLetter | ForEach-Object { [ordered]@{ letter = \"$($_.DriveLetter):\"; "
                               "guid = $_.UniqueId; fs = $_.FileSystem; size = [string]$_.Size } }) | ConvertTo-Json"))
    gv = gv if isinstance(gv, list) else [gv]
    bad = []
    for item in gv:
        guid = re.search(r"\{([0-9a-fA-F-]{36})\}", item["guid"]).group(1).lower()
        v = local.get(guid)
        exact = free_space(item["letter"] + "\\")
        if not v or v["identity"]["drive_letter"] != item["letter"] or v["filesystem"] != item["fs"] \
                or int(v["capacity_bytes"]) != exact[0] or abs(int(v["free_bytes"]) - exact[1]) > 256 * 2**20:
            bad.append(f"{item['letter']}: LARM {v and (v['filesystem'], v['capacity_bytes'], v['free_bytes'])} / "
                       f"Get-Volume {item['fs']} {item['size']} / GetDiskFreeSpaceExW {exact}")
    step(log, "capacidad-y-sistema-de-ficheros", not bad,
         bad[0] if bad else f"{len(gv)} volumen(es) con letra: letra, NTFS y capacidad exacta (enteros de 64 bits) = Get-Volume y GetDiskFreeSpaceExW")

    disks = json.loads(powershell("@(Get-PhysicalDisk | ForEach-Object { [ordered]@{ bus = [string]$_.BusType; media = [string]$_.MediaType } }) | ConvertTo-Json"))
    disks = disks if isinstance(disks, list) else [disks]
    expected = "NVME" if all(d["bus"] == "NVMe" for d in disks) else None
    classes = {v["performance_class"] for v in local.values()}
    step(log, "clase-nvme", expected is not None and classes == {expected},
         f"Get-PhysicalDisk {disks}; LARM {sorted(classes)} (bus NVMe por IOCTL_STORAGE_QUERY_PROPERTY)")

    share = next(v for v in obs["volumes"] if v["identity"]["volume_key"] == SHARE.lower())
    oracle_share = free_space(SHARE + "\\")
    step(log, "unc-disponible", share["online"] and share["identity"]["volume_guid"] is None
         and int(share["capacity_bytes"]) == oracle_share[0] and share["performance_class"] == "NETWORK",
         f"{SHARE}: volume_key {share['identity']['volume_key']}, sin GUID inventado, capacidad {share['capacity_bytes']} = GetDiskFreeSpaceExW")
    missing = next(v for v in obs["volumes"] if v["identity"]["volume_key"] == MISSING_SHARE.lower())
    step(log, "unc-inexistente", not missing["online"] and missing["capacity_bytes"] is None and free_space(MISSING_SHARE + "\\") is None,
         f"{MISSING_SHARE}: no disponible, sin capacidad inventada; error real: {missing['evidence_refs'][-1][:80]}")

    again = daemon("storage-probe")
    keys = lambda o: sorted(v["identity"]["volume_key"] for v in o["volumes"] if v["identity"]["volume_guid"])  # noqa: E731
    step(log, "identidad-estable", keys(again) == keys(obs), "dos sondeos dan las mismas volume_key")
    step(log, "sin-desconocidos", obs["unknown"] == [], "ningún volumen quedó con identidad o clase desconocida")
    (RUNS / "windows-steps.json").write_text(json.dumps(log, indent=2, ensure_ascii=False), encoding="utf-8")
    return f"{len(log)} comprobaciones frente a oráculos independientes PASS"


def http(url: str, token: str, body: dict | None = None, key: str | None = None) -> tuple[int, dict | None]:
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(url, data=data, method="POST" if data else "GET")
    req.add_header("authorization", f"Bearer {token}")
    if data:
        req.add_header("content-type", "application/json")
    if key:
        req.add_header("idempotency-key", key)
    try:
        with urllib.request.urlopen(req, timeout=15) as r:
            return r.status, json.loads(r.read() or b"null")
    except urllib.error.HTTPError as e:
        return e.code, json.loads(e.read() or b"null")


def insert(db: Path, name: str, key: str, unc: str | None, state: str, root: str) -> str:
    """Fila de prueba cuya raíz es `root` (debe existir si se espera ONLINE: si no, desde P014 es MISSING)."""
    id_ = str(uuid.uuid7())  # UUIDv7, como exige el contrato para los id
    with sqlite3.connect(db) as c:
        c.execute(
            "INSERT INTO storage_location (id, created_at, updated_at, row_version, metadata, name, root_path, canonical_path, "
            "volume_guid, drive_letter, unc_path, filesystem, storage_type, capacity_bytes, free_bytes, reserved_bytes, priority, "
            "auto_placement, state, last_seen_at, volume_key, performance_class) VALUES "
            "(?, '2026-10-11T00:00:00.000Z', '2026-10-11T00:00:00.000Z', 1, '{}', ?, ?, ?, ?, NULL, ?, 'UNKNOWN', 'LOCAL', "
            "0, 0, 0, 0, 0, ?, NULL, ?, 'UNKNOWN')",
            (id_, name, root, root, None if unc else key, unc, state, key))
    return id_


def rows(db: Path) -> dict:
    with sqlite3.connect(db) as c:
        return {r[0]: {"state": r[1], "row_version": r[2], "capacity": r[3], "free": r[4], "class": r[5], "letter": r[6], "last_seen": r[7]}
                for r in c.execute("SELECT name, state, row_version, capacity_bytes, free_bytes, performance_class, drive_letter, "
                                   "last_seen_at FROM storage_location")}


def events(db: Path) -> int:
    with sqlite3.connect(db) as c:
        return c.execute("SELECT COUNT(*) FROM event e JOIN event_outbox o ON o.event_record_id = e.id "
                         "WHERE e.event_type = 'storage.state_changed'").fetchone()[0]


def wait(db: Path, ok, timeout: float = 90) -> dict:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        current = rows(db)
        if ok(current):
            return current
        time.sleep(0.5)
    raise AssertionError(f"las filas no llegaron al estado esperado: {rows(db)}")


def serve() -> str:
    root = RUNS / "real"
    shutil.rmtree(root, ignore_errors=True)
    root.mkdir(parents=True)
    log: list = []
    proc = None
    c_guid = next(g for g, m in mountvol().items() if "C:\\" in m)
    try:
        daemon("db-info", root=root)
        db = root / "state/larm.sqlite"
        roots = root / "roots"
        for name in ("modelos", "cache"):
            (roots / name).mkdir(parents=True)
        insert(db, "modelos", c_guid, None, "UNKNOWN", str(roots / "modelos"))
        insert(db, "cache", c_guid, None, "UNKNOWN", str(roots / "cache"))
        insert(db, "fantasma", GHOST, None, "ONLINE", str(roots / "fantasma"))
        insert(db, "red", SHARE.lower(), SHARE, "UNKNOWN", SHARE)
        admin = daemon("enroll-principal", "--name", "p012-admin", "--kind", "OPERATOR", "--scopes", "admin", root=root)["token"]
        with socket.socket() as s:
            s.bind(("127.0.0.1", 0))
            port = s.getsockname()[1]
        proc = subprocess.Popen([str(EXE), "--data-root", str(root), "--port", str(port), "--serve-api"], env=clean_env(),
                                creationflags=subprocess.CREATE_NEW_PROCESS_GROUP, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        first = wait(db, lambda r: r["modelos"]["state"] == "ONLINE" and r["red"]["state"] == "ONLINE" and r["fantasma"]["state"] == "OFFLINE")
        exact = free_space("C:\\")
        step(log, "arranque-online", first["modelos"]["capacity"] == exact[0] and first["modelos"]["class"] == "NVME"
             and first["modelos"]["letter"] == "C:" and first["cache"]["state"] == "ONLINE" and first["modelos"]["row_version"] == 2,
             f"dos raíces del volumen de C: -> ONLINE, NVME, capacidad {exact[0]} (GetDiskFreeSpaceExW), row_version 2")
        step(log, "volumen-ausente-offline", first["fantasma"]["last_seen"] is None and first["fantasma"]["state"] == "OFFLINE",
             "volume_key que no existe en el equipo -> OFFLINE, sin last_seen_at inventado")
        step(log, "unc-online", first["red"]["class"] == "NETWORK" and first["red"]["capacity"] == free_space(SHARE + "\\")[0],
             f"{SHARE} -> ONLINE, NETWORK, capacidad real")
        step(log, "eventos-outbox", events(db) == 4, f"{events(db)} storage.state_changed con su fila de outbox (uno por cambio)")

        base = f"http://127.0.0.1:{port}/api/v1"
        status, receipt = http(f"{base}/hardware/refresh", admin, {"resource_id": None, "profile_id": None}, "p012-r1")
        deadline = time.monotonic() + 90
        while time.monotonic() < deadline:
            st, job = http(f"{base}/jobs/{receipt['job_id']}", admin)
            if job["state"] in ("COMPLETED", "FAILED"):
                break
            time.sleep(0.5)
        second = rows(db)
        step(log, "segundo-sondeo-sin-version", job["state"] == "COMPLETED"
             and second["modelos"]["row_version"] == 2 and second["modelos"]["last_seen"] > first["modelos"]["last_seen"]
             and events(db) == 4,
             f"refresco {job['state']}: last_seen_at {first['modelos']['last_seen']} -> {second['modelos']['last_seen']}, "
             "row_version 2 y sin eventos nuevos (solo espacio libre y última vez visto)")
        _, page = http(f"{base}/hardware", admin)
        storage = page["items"][-1]["metadata"]["storage"]
        step(log, "observacion-en-el-perfil", any(v["identity"]["volume_key"] == c_guid for v in storage["volumes"])
             and any(v["identity"]["volume_key"] == SHARE.lower() for v in storage["volumes"]),
             f"metadata.storage del perfil con {len(storage['volumes'])} volúmenes, incluida la raíz UNC registrada")
    finally:
        if proc is not None:
            os.kill(proc.pid, signal.CTRL_BREAK_EVENT)
            try:
                proc.wait(timeout=20)
            except subprocess.TimeoutExpired:
                proc.kill()
        try:
            daemon("revoke-principal", "--name", "p012-admin", root=root)
        except AssertionError:
            pass
        (root / "steps.json").write_text(json.dumps(log, indent=2, ensure_ascii=False), encoding="utf-8")
    return f"{len(log)} pasos PASS"


if __name__ == "__main__":
    sys.stdout.reconfigure(encoding="utf-8")
    try:
        print("PASS " + {"windows": windows, "serve": serve}[sys.argv[1]]())
        raise SystemExit(0)
    except (AssertionError, KeyError, OSError, ValueError, StopIteration, TypeError, subprocess.SubprocessError, sqlite3.Error) as e:
        print(f"FAIL {e}")
        raise SystemExit(1)
