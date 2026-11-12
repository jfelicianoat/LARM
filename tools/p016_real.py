#!/usr/bin/env python3
"""P016 - Prueba real del movimiento de modelos con el daemon en el PC IA.

  python tools/p016_real.py serve

Ubicaciones de prueba: dos carpetas en C: (mismo volumen) y una carpeta
temporal en el recurso existente \\\\localhost\\Temp (otra volume_key; mismo
disco físico), que se borra al final. Las filas del registro de modelos
(variante, artefactos, ubicaciones y leases) se siembran con sqlite3 porque
su API es de P040/P041/P100; todo lo demás va por la API del daemon.

  1. alta de las tres ubicaciones; siembra de un modelo de 64 MiB (LARM_MANAGED)
     y un fichero EXTERNAL;
  2. plan en seco al mismo volumen: RENAME, EXTERNAL conservado, sin rutas
     absolutas; GET /artifact-locations;
  3. ejecución: job hasta COMPLETED, fichero renombrado con el hash original
     (sha256 de Python), origen fuera, registro y evento artifact.moved;
  4. a otro volumen (UNC): COPY con reserva, copia verificada, origen borrado,
     staging retirado, verified_at;
  5. lease ACTIVE sobre la variante: 409 ACTIVE_LEASE y nada se mueve; al
     soltarla, el mismo plan vuelve a C:;
  6. plan obsoleto: la fila cambia tras el plan -> 409 STALE_REVISION;
  7. hash distinto real: el origen se altera (mismo tamaño) tras el plan ->
     INTEGRITY_FAILED, registro intacto, destino y staging limpios;
  8. caída real: TerminateProcess del daemon copiando 1 GiB a la red; al
     rearrancar, la recuperación del journal deja FAILED, origen íntegro,
     staging borrado y ningún lock.
"""
import hashlib
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
RUNS = REPO / "docs/reports/P016_runs"
SHARE = r"\\localhost\Temp"
MIB = 1 << 20


def clean_env() -> dict:
    return {k: v for k, v in os.environ.items() if not k.startswith("LARM_")}


def step(log: list, name: str, ok: bool, detail: str) -> None:
    log.append({"step": name, "result": "PASS" if ok else "FAIL", "detail": detail})
    print(f"  [{'PASS' if ok else 'FAIL'}] {name}: {detail}", flush=True)
    if not ok:
        raise AssertionError(f"{name}: {detail}")


def http(method: str, url: str, token: str, body=None, headers: dict | None = None) -> tuple[int, dict | None]:
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(url, data=data, method=method)
    req.add_header("authorization", f"Bearer {token}")
    if data is not None:
        req.add_header("content-type", "application/json")
    for k, v in (headers or {}).items():
        req.add_header(k, v)
    try:
        with urllib.request.urlopen(req, timeout=300) as r:
            raw = r.read()
            return r.status, json.loads(raw) if raw else None
    except urllib.error.HTTPError as e:
        raw = e.read()
        return e.code, json.loads(raw) if raw else None


def start(root: Path) -> tuple[subprocess.Popen, str]:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        port = s.getsockname()[1]
    proc = subprocess.Popen([str(EXE), "--data-root", str(root), "--port", str(port), "--serve-api"],
                            env=clean_env(), creationflags=subprocess.CREATE_NEW_PROCESS_GROUP,
                            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    base = f"http://127.0.0.1:{port}/api/v1"
    for _ in range(150):
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


def sha(path: Path) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for block in iter(lambda: f.read(8 * MIB), b""):
            h.update(block)
    return h.hexdigest()


def write_random(path: Path, size: int) -> str:
    path.parent.mkdir(parents=True, exist_ok=True)
    h = hashlib.sha256()
    with open(path, "wb") as f:
        left = size
        while left:
            block = os.urandom(min(left, 8 * MIB))
            h.update(block)
            f.write(block)
            left -= len(block)
    return h.hexdigest()


def now() -> str:
    return time.strftime("%Y-%m-%dT%H:%M:%S.000Z", time.gmtime())


def seed(db: Path, statements: list[tuple[str, tuple]]) -> None:
    with sqlite3.connect(db) as c:
        c.execute("PRAGMA foreign_keys = OFF")
        for sql, args in statements:
            c.execute(sql, args)


def q(db: Path, sql: str, args: tuple = ()) -> list:
    with sqlite3.connect(db) as c:
        return c.execute(sql, args).fetchall()


def variant_row(vid: str) -> tuple[str, tuple]:
    n = now()
    return ("INSERT INTO model_variant (id,created_at,updated_at,row_version,metadata,update_available,status,revision_id,format,"
            "quantization,architecture,bundle_hash,lifecycle,availability,ownership,promotion,system1_state,system_role,capabilities,"
            "model_limits) VALUES (?,?,?,1,'{}',0,'AVAILABLE','rev','GGUF','Q4_K_M','llama',NULL,'AVAILABLE','AVAILABLE',"
            "'LARM_MANAGED','NONE',NULL,'GENERAL','[]','{}')", (vid, n, n))


def artifact_row(aid: str, vid: str, name: str) -> tuple[str, tuple]:
    n = now()
    return ("INSERT INTO model_artifact (id,created_at,updated_at,row_version,metadata,variant_id,filename,format,quantization,"
            "expected_size,actual_size,sha256,source_url,required) VALUES (?,?,?,1,'{}',?,?,'GGUF',NULL,NULL,NULL,NULL,NULL,1)",
            (aid, n, n, vid, name))


def location_row(lid: str, aid: str, root: str, rel: str, size: int, digest: str | None, ownership: str) -> tuple[str, tuple]:
    n = now()
    return ("INSERT INTO artifact_location (id,created_at,updated_at,row_version,metadata,artifact_id,storage_location_id,"
            "relative_path,file_size,sha256,state,verified_at,ownership) VALUES (?,?,?,1,'{}',?,?,?,?,?,'ONLINE',NULL,?)",
            (lid, n, n, aid, root, rel, size, digest, ownership))


def wait_job(base: str, token: str, job_id: str, limit: float = 600) -> dict:
    deadline = time.monotonic() + limit
    while time.monotonic() < deadline:
        _, job = http("GET", f"{base}/jobs/{job_id}", token)
        if job and job["state"] in ("COMPLETED", "FAILED"):
            return job
        time.sleep(0.2)
    raise AssertionError("el job no terminó")


def move(base: str, token: str, variant: str, target: str, key: str, plan_hash: str | None) -> tuple[int, dict]:
    return http("POST", f"{base}/models/{variant}/move", token,
                {"target_storage_location_id": target, "plan_hash": plan_hash, "dry_run": plan_hash is None,
                 "approval_id": None}, {"idempotency-key": key})


def serve() -> str:
    run = RUNS / "real"
    shutil.rmtree(run, ignore_errors=True)
    roots = run / "roots"
    origin, same = roots / "origen", roots / "mismo"
    origin.mkdir(parents=True)
    same.mkdir()
    share_dir = Path(SHARE) / f"larm-p016-{uuid.uuid4().hex[:8]}"
    share_dir.mkdir()
    data = run / "data"
    db = data / "state/larm.sqlite"
    log: list = []
    proc = None
    vid, aid, lid = str(uuid.uuid7()), str(uuid.uuid7()), str(uuid.uuid7())
    ext_aid, ext_lid = str(uuid.uuid7()), str(uuid.uuid7())
    try:
        token = json.loads(subprocess.run([str(EXE), "--data-root", str(data), "enroll-principal", "--name", "p016",
                                           "--kind", "OPERATOR", "--scopes", "read,storage:write"],
                                          capture_output=True, env=clean_env(), check=True).stdout)["token"]
        proc, base = start(data)
        ids = {}
        for name, root in (("origen", origin), ("mismo", same), ("red", share_dir)):
            st, loc = http("POST", f"{base}/storage-locations", token,
                           {"name": name, "root_path": str(root), "reserved_bytes": "0", "priority": 1,
                            "auto_placement": True, "performance_class": "UNKNOWN"}, {"idempotency-key": name})
            assert st == 201, loc
            ids[name] = loc
        size = 64 * MIB
        digest = write_random(origin / "modelo" / "pesos.gguf", size)
        (origin / "ext.bin").write_bytes(b"externo")
        seed(db, [variant_row(vid), artifact_row(aid, vid, "pesos.gguf"),
                  location_row(lid, aid, ids["origen"]["id"], "modelo/pesos.gguf", size, digest, "LARM_MANAGED"),
                  artifact_row(ext_aid, vid, "ext.bin"),
                  location_row(ext_lid, ext_aid, ids["origen"]["id"], "ext.bin", 7, None, "EXTERNAL")])
        c_guid = ids["origen"]["volume_key"]
        step(log, "alta-y-siembra", ids["mismo"]["volume_key"] == c_guid and ids["red"]["volume_key"] == SHARE.lower()
             and "volume_relative_path" in ids["origen"]["metadata"],
             f"origen y mismo en C: (GUID {c_guid[:8]}…, ruta dentro del volumen guardada), red en {SHARE.lower()}; "
             f"modelo de 64 MiB sha256 {digest[:12]}… + EXTERNAL")

        st, plan = move(base, token, vid, ids["mismo"]["id"], "p1", None)
        kinds = sorted(s["phase"] for s in plan["steps"])
        _, page = http("GET", f"{base}/artifact-locations", token)
        text = json.dumps(plan)
        step(log, "plan-mismo-volumen", st == 201 and kinds == ["KEEP", "RENAME"] and plan["approval_required"] is False
             and int(plan["storage_estimate"]["peak_extra_bytes"]) == 0 and str(roots) not in text and "C:\\\\" not in text
             and len(page["items"]) == 2 and q(db, "SELECT phase FROM operation_journal WHERE id = ?", (plan["plan_id"],)) == [("PLANNED",)],
             f"201 PLANNED: RENAME + KEEP(external_preserved), 0 bytes nuevos, sin rutas absolutas; hash {plan['plan_hash'][:12]}…")

        st, executed = move(base, token, vid, ids["mismo"]["id"], "p2", plan["plan_hash"])
        job = wait_job(base, token, executed["plan_id"])
        dst = same / "modelo" / "pesos.gguf"
        _, row = http("GET", f"{base}/artifact-locations/{lid}", token)
        events = q(db, "SELECT count(*) FROM event WHERE event_type = 'artifact.moved' AND resource_id = ?", (lid,))[0][0]
        step(log, "mueve-mismo-volumen", st == 201 and job["state"] == "COMPLETED" and job["progress"]["phase"] == "COMPLETED"
             and sha(dst) == digest and not (origin / "modelo" / "pesos.gguf").exists()
             and row["storage_location_id"] == ids["mismo"]["id"] and row["row_version"] == 2 and events == 1
             and (origin / "ext.bin").read_bytes() == b"externo",
             "job COMPLETED; renombrado con el mismo sha256; registro v2 en «mismo»; artifact.moved; EXTERNAL intacto")

        st, plan = move(base, token, vid, ids["red"]["id"], "p3", None)
        assert st == 201, plan
        reserved = []
        st2, executed = move(base, token, vid, ids["red"]["id"], "p4", plan["plan_hash"])
        reserved = q(db, "SELECT total_bytes FROM volume_reservation")
        job = wait_job(base, token, executed["plan_id"])
        dst = share_dir / "modelo" / "pesos.gguf"
        _, row = http("GET", f"{base}/artifact-locations/{lid}", token)
        step(log, "mueve-a-otro-volumen", st == 201 and st2 == 201 and [s["phase"] for s in plan["steps"] if s["phase"] != "KEEP"] == ["COPY"]
             and int(plan["storage_estimate"]["peak_extra_bytes"]) == size and job["state"] == "COMPLETED"
             and job["progress"]["bytes_copied"] == str(size) and sha(dst) == digest and not (same / "modelo" / "pesos.gguf").exists()
             and not (share_dir / "staging").exists() and row["storage_location_id"] == ids["red"]["id"]
             and row["sha256"] == digest and row["verified_at"] and q(db, "SELECT count(*) FROM resource_lock") == [(0,)]
             and q(db, "SELECT count(*) FROM volume_reservation") == [(0,)],
             f"COPY a {SHARE}: reserva {reserved} durante el job; 64 MiB copiados, sha256 releído igual, origen borrado, "
             "staging retirado, verified_at; sin locks ni reservas al final")

        n = now()
        seed(db, [("INSERT INTO runtime_instance (id,created_at,updated_at,row_version,metadata,runtime_installation_id,variant_id,"
                   "backend_id,launch_profile_id,state,pid,process_started_at,executable_hash,generation,execution_fingerprint,"
                   "last_used_at,ownership) VALUES ('inst-p016',?,?,1,'{}','ri','" + vid + "','be','lp','READY',NULL,NULL,NULL,1,NULL,NULL,'LARM_MANAGED')", (n, n)),
                  ("INSERT INTO runtime_lease (id,created_at,updated_at,row_version,metadata,owner_id,request_id,instance_id,generation,"
                   "state,expires_at,endpoint_id,constraints) VALUES ('lease-p016',?,?,1,'{}','broker','r','inst-p016',1,'ACTIVE',NULL,NULL,'{}')", (n, n))])
        _, plan = move(base, token, vid, ids["origen"]["id"], "p5", None)
        st, err = move(base, token, vid, ids["origen"]["id"], "p6", plan["plan_hash"])
        step(log, "lease-activa-bloquea", st == 409 and err["code"] == "ACTIVE_LEASE" and "lease-p016" in err["details"]["resource_ids"]
             and sha(dst) == digest and q(db, "SELECT phase FROM operation_journal WHERE id = ?", (plan["plan_id"],)) == [("PLANNED",)]
             and q(db, "SELECT count(*) FROM resource_lock") == [(0,)],
             "lease ACTIVE sobre la variante: 409 ACTIVE_LEASE desde el lock; el plan sigue PLANNED y nada se movió")
        seed(db, [("UPDATE runtime_lease SET state = 'RELEASED' WHERE id = 'lease-p016'", ())])
        st, executed = move(base, token, vid, ids["origen"]["id"], "p7", plan["plan_hash"])
        job = wait_job(base, token, executed["plan_id"])
        step(log, "lease-soltada-mueve", st == 201 and job["state"] == "COMPLETED" and sha(origin / "modelo" / "pesos.gguf") == digest
             and not dst.exists(), "lease RELEASED: el mismo plan se ejecuta y el modelo vuelve de la red a C:")

        _, plan = move(base, token, vid, ids["red"]["id"], "p8", None)
        seed(db, [("UPDATE artifact_location SET metadata = '{\"tocado\":true}', row_version = row_version + 1 WHERE id = ?", (lid,))])
        st, err = move(base, token, vid, ids["red"]["id"], "p9", plan["plan_hash"])
        step(log, "plan-obsoleto", st == 409 and err["code"] == "STALE_REVISION" and err["details"]["reason"] == "plan_changed"
             and q(db, "SELECT phase FROM operation_journal WHERE id = ?", (plan["plan_id"],)) == [("CANCELLED",)],
             "la fila cambió tras el plan: 409 STALE_REVISION plan_changed; plan CANCELLED")

        _, plan = move(base, token, vid, ids["red"]["id"], "p10", None)
        src = origin / "modelo" / "pesos.gguf"
        with open(src, "r+b") as f:
            f.seek(size // 2)
            byte = f.read(1)
            f.seek(size // 2)
            f.write(bytes([byte[0] ^ 0xFF]))
        version = q(db, "SELECT row_version FROM artifact_location WHERE id = ?", (lid,))[0][0]
        st, executed = move(base, token, vid, ids["red"]["id"], "p11", plan["plan_hash"])
        job = wait_job(base, token, executed["plan_id"])
        step(log, "hash-distinto", st == 201 and job["state"] == "FAILED" and job["error"]["code"] == "INTEGRITY_FAILED"
             and job["error"]["details"]["reason"] == "source_hash_mismatch" and src.exists() and not dst.exists()
             and not (share_dir / "staging").exists()
             and q(db, "SELECT storage_location_id, row_version FROM artifact_location WHERE id = ?", (lid,)) == [(ids["origen"]["id"], version)]
             and q(db, "SELECT count(*) FROM resource_lock") == [(0,)],
             "origen alterado (mismo tamaño) tras el plan: INTEGRITY_FAILED; registro intacto, sin copia ni staging, sin locks")
        with open(src, "r+b") as f:
            f.seek(size // 2)
            f.write(byte)
        assert sha(src) == digest

        # Caída real copiando 1 GiB a la red.
        big = 1024 * MIB
        big_digest = write_random(origin / "modelo" / "grande.gguf", big)
        big_aid, big_lid = str(uuid.uuid7()), str(uuid.uuid7())
        seed(db, [artifact_row(big_aid, vid, "grande.gguf"),
                  location_row(big_lid, big_aid, ids["origen"]["id"], "modelo/grande.gguf", big, big_digest, "LARM_MANAGED")])
        _, plan = move(base, token, vid, ids["red"]["id"], "p12", None)
        st, executed = move(base, token, vid, ids["red"]["id"], "p13", plan["plan_hash"])
        staging = share_dir / "staging" / executed["plan_id"]
        deadline = time.monotonic() + 120
        partial = 0
        while time.monotonic() < deadline:
            files = list(staging.glob("*.partial")) if staging.exists() else []
            partial = sum(f.stat().st_size for f in files)
            if partial > 64 * MIB:
                break
            time.sleep(0.02)
        proc.kill()
        proc.wait(timeout=30)
        proc = None
        phase = q(db, "SELECT phase FROM operation_journal WHERE id = ?", (executed["plan_id"],))
        locks = q(db, "SELECT count(*) FROM resource_lock")[0][0]
        step(log, "caida-copiando", st == 201 and partial > 64 * MIB and phase == [("COPYING",)] and locks > 0,
             f"TerminateProcess con {partial // MIB} MiB copiados: journal COPYING y {locks} locks sobreviven a la caída")
        proc, base = start(data)
        phase = q(db, "SELECT phase, json_extract(payload, '$.error.details.reason') FROM operation_journal WHERE id = ?", (executed["plan_id"],))
        rows = q(db, "SELECT storage_location_id, row_version FROM artifact_location WHERE id IN (?, ?) ORDER BY id", (lid, big_lid))
        step(log, "recuperacion-del-journal", phase == [("FAILED", "job_interrupted")] and not staging.exists()
             and sha(origin / "modelo" / "grande.gguf") == big_digest and sha(origin / "modelo" / "pesos.gguf") == digest
             and not list((share_dir / "modelo").glob("*.gguf")) and all(r[0] == ids["origen"]["id"] for r in rows)
             and q(db, "SELECT count(*) FROM resource_lock") == [(0,)] and q(db, "SELECT count(*) FROM volume_reservation") == [(0,)],
             "al rearrancar: FAILED job_interrupted; staging borrado; los dos orígenes íntegros y registrados; sin locks ni reservas")
    finally:
        if proc is not None:
            stop(proc)
        shutil.rmtree(share_dir, ignore_errors=True)
        for big_file in (origin / "modelo").glob("grande.gguf"):
            big_file.unlink()
        subprocess.run([str(EXE), "--data-root", str(data), "revoke-principal", "--name", "p016"], capture_output=True, env=clean_env())
        RUNS.mkdir(parents=True, exist_ok=True)
        (RUNS / "serve-steps.json").write_text(json.dumps(log, indent=2, ensure_ascii=False), encoding="utf-8")
    step(log, "limpieza", not share_dir.exists(), f"{share_dir} y el fichero de 1 GiB borrados")
    return f"{len(log)} pasos PASS"


if __name__ == "__main__":
    sys.stdout.reconfigure(encoding="utf-8")
    try:
        print("PASS " + {"serve": serve}[sys.argv[1]]())
        raise SystemExit(0)
    except (AssertionError, KeyError, OSError, ValueError, TypeError, subprocess.SubprocessError, sqlite3.Error) as e:
        print(f"FAIL {type(e).__name__}: {e}")
        raise SystemExit(1)
