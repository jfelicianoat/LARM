#!/usr/bin/env python3
"""P018 - Prueba real del analizador, la limpieza, el borrado, la reconciliación
y la reconstrucción del registro con el daemon en el PC IA.

  python tools/p018_real.py serve

  1. Copia al CAS (models-import-dir --mode copy) de dos modelos que comparten
     un README; se escriben sus manifiestos.
  2. Análisis por la API con un blob huérfano, una descarga parcial de un job
     terminado, otra de un job vivo y un fichero del usuario: categorías y
     recuperable correctos; sin rutas absolutas.
  3. Limpieza: plan (aprobación obligatoria), POST /approvals ligado al hash
     y a los recursos, ejecución 202 y job COMPLETED: se borran solo el blob
     huérfano y la parcial del job terminado.
  4. Borrado de la variante Alfa: delete-plan, aprobación y DELETE con
     If-Match: 204; su gguf borrado, el README compartido y Beta intactos.
  5. Raíz que desaparece (carpeta renombrada): el sondeo la marca MISSING y
     la reconciliación deja sus artefactos MISSING y la variante OFFLINE sin
     borrar nada; al volver, se releen los bytes y se restaura.
  6. Reconstrucción: registry-rebuild en una base de staging desde los
     manifiestos (Beta recuperada); db-restore de esa base en otra carpeta de
     datos y el daemon la sirve.
"""
import hashlib
import json
import os
import shutil
import signal
import socket
import sqlite3
import stat
import struct
import subprocess
import sys
import time
import urllib.error
import urllib.request
import uuid
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
EXE = REPO / "target/debug/larm-daemon.exe"
RUNS = REPO / "docs/reports/P018_runs"
SOURCE = "0192f000-0000-7000-8000-00000000f018"
KIB = 1024


def clean_env() -> dict:
    return {k: v for k, v in os.environ.items() if not k.startswith("LARM_")}


def step(log: list, name: str, ok: bool, detail: str) -> None:
    log.append({"step": name, "result": "PASS" if ok else "FAIL", "detail": detail})
    print(f"  [{'PASS' if ok else 'FAIL'}] {name}: {detail}", flush=True)
    if not ok:
        raise AssertionError(f"{name}: {detail}")


def daemon(data: Path, *args: str) -> tuple[int, object, str]:
    r = subprocess.run([str(EXE), "--data-root", str(data), *args], capture_output=True, text=True,
                       encoding="utf-8", env=clean_env())
    try:
        out = json.loads(r.stdout) if r.stdout.strip() else None
    except ValueError:
        out = None
    return r.returncode, out, r.stderr


def http(method: str, url: str, token: str, body=None, headers: dict | None = None) -> tuple[int, dict | None]:
    data = json.dumps(body).encode() if body is not None else None
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


def gguf(name: str, payload: int) -> bytes:
    k = "general.name"
    return (b"GGUF" + struct.pack("<IQQ", 3, 0, 1) + struct.pack("<Q", len(k)) + k.encode()
            + struct.pack("<I", 8) + struct.pack("<Q", len(name)) + name.encode() + os.urandom(payload))


def wait_job(base: str, token: str, job_id: str) -> dict:
    for _ in range(300):
        _, job = http("GET", f"{base}/jobs/{job_id}", token)
        if job and job["state"] in ("COMPLETED", "FAILED"):
            return job
        time.sleep(0.2)
    raise AssertionError("el job no terminó")


def approve(base: str, token: str, plan: dict, key: str) -> str:
    st, a = http("POST", f"{base}/approvals", token,
                 {"operation_digest": plan["plan_hash"], "resource_ids": plan["resource_ids"], "code_hash": None,
                  "permissions": {}, "approved": True}, {"idempotency-key": key})
    assert st == 201, a
    return a["resource_id"]


def q(db: Path, sql: str, args: tuple = ()) -> list:
    with sqlite3.connect(db) as c:
        return c.execute(sql, args).fetchall()


def serve() -> str:
    run = RUNS / "real"
    shutil.rmtree(run, ignore_errors=True)
    data, cas, src = run / "data", run / "roots" / "cas", run / "fuente"
    cas.mkdir(parents=True)
    db = data / "state/larm.sqlite"
    log: list = []
    proc = None
    try:
        rc, enrolled, err = daemon(data, "enroll-principal", "--name", "p018", "--kind", "OPERATOR",
                                   "--scopes", "read,storage:write,models:write,admin")
        assert rc == 0, err
        token = enrolled["token"]
        proc, base = start(data)
        st, loc = http("POST", f"{base}/storage-locations", token,
                       {"name": "cas", "root_path": str(cas), "reserved_bytes": "0", "priority": 1,
                        "auto_placement": True, "performance_class": "UNKNOWN"}, {"idempotency-key": "cas"})
        assert st == 201, loc
        lid = loc["id"]
        with sqlite3.connect(db) as c:
            c.execute("PRAGMA foreign_keys = OFF")
            c.execute("INSERT INTO model_source (id,created_at,updated_at,row_version,metadata,name,adapter_id,base_url,priority,"
                      "enabled,auth_mode,account_id,policy) VALUES (?,'2026-10-11T00:00:00.000Z','2026-10-11T00:00:00.000Z',1,'{}',"
                      "'carpeta local','local','file:',0,1,'NONE',NULL,'{}')", (SOURCE,))
        for d, name in (("alfa", "Alfa"), ("beta", "Beta")):
            (src / d).mkdir(parents=True)
            (src / d / f"{d}.gguf").write_bytes(gguf(name, 300 * KIB))
            (src / d / "README.md").write_bytes(b"# comun\n")
        rc, imp, err = daemon(data, "models-import-dir", "--dir", str(src), "--location", lid, "--source", SOURCE, "--mode", "copy")
        assert rc == 0, err
        alfa, beta = (v["variant_id"] for v in imp["variants"])
        step(log, "copia-con-manifiestos", imp["blobs_copied"] == 3 and imp["blobs_already_present"] == 1
             and len(list((cas / "manifests").glob("*.json"))) == 2,
             "2 modelos al CAS (3 blobs, el README una vez) y 2 manifiestos junto al CAS")

        orphan = cas / "blobs/sha256/ff" / ("f" * 64)
        orphan.parent.mkdir(parents=True, exist_ok=True)
        orphan.write_bytes(b"huerfano")
        live = str(uuid.uuid7())
        with sqlite3.connect(db) as c:
            c.execute("INSERT INTO operation_journal (id,kind,resource_ids,phase,plan_hash,payload,updated_at) "
                      "VALUES (?,'download','[]','RUNNING','0','{}','t')", (live,))
        for job, size in (("job-terminado", 300), (live, 500)):
            (cas / "staging" / job).mkdir(parents=True)
            (cas / "staging" / job / "modelo.gguf.partial").write_bytes(b"\0" * size)
        (cas / "fotos.jpg").write_bytes(b"\1" * 70)
        st, an = http("GET", f"{base}/storage-locations/{lid}/analysis", token)
        cats = {c["kind"]: c for c in an["categories"]} if st == 200 else {}
        step(log, "analisis", st == 200 and cats["PARTIALS"]["used_bytes"] == "800" and cats["PARTIALS"]["reclaimable_bytes"] == "300"
             and cats["PARTIALS"]["blocker_codes"] == ["job_active"] and cats["ORPHANS"]["reclaimable_bytes"] == "8"
             and "not_owned_by_larm" in cats["ORPHANS"]["blocker_codes"] and cats["MODELS"]["reclaimable_bytes"] == "0"
             and str(run) not in json.dumps(an),
             "parciales 800 B (300 recuperables; la del job vivo bloqueada), blob huérfano recuperable, fichero del usuario "
             "no recuperable, modelos sin copias de más; sin rutas absolutas")

        st, plan = http("POST", f"{base}/storage-locations/{lid}/cleanup-plan", token, {"resource_id": None, "profile_id": None})
        st2, e = http("POST", f"{base}/storage-locations/{lid}/cleanup", token,
                      {"plan_id": plan["plan_id"], "plan_hash": plan["plan_hash"], "approval_id": None})
        ok_without = st2 == 409 and e["code"] == "APPROVAL_REQUIRED"
        approval = approve(base, token, plan, "c1")
        st3, receipt = http("POST", f"{base}/storage-locations/{lid}/cleanup", token,
                            {"plan_id": plan["plan_id"], "plan_hash": plan["plan_hash"], "approval_id": approval})
        job = wait_job(base, token, receipt["job_id"]) if st3 == 202 else {}
        step(log, "limpieza-aprobada", st == 201 and plan["approval_required"] and len(plan["steps"]) == 2 and ok_without
             and st3 == 202 and job.get("state") == "COMPLETED" and not orphan.exists()
             and not (cas / "staging/job-terminado/modelo.gguf.partial").exists()
             and (cas / "staging" / live / "modelo.gguf.partial").exists() and (cas / "fotos.jpg").exists(),
             f"plan de 2 pasos; sin aprobación 409; con aprobación 202 y job COMPLETED ({job.get('result', {}).get('freed_bytes')} B); "
             "intactos la parcial del job vivo y la foto")

        st, dplan = http("POST", f"{base}/models/{alfa}/delete-plan", token, {"resource_id": None, "profile_id": None},
                         {"idempotency-key": "d1"})
        _, variant = http("GET", f"{base}/variants/{alfa}", token)
        approval = approve(base, token, dplan, "d2")
        alfa_blob = q(db, "SELECT l.relative_path FROM artifact_location l JOIN model_artifact a ON a.id = l.artifact_id "
                          "WHERE a.variant_id = ? AND a.filename = 'alfa.gguf'", (alfa,))[0][0]
        st2, _ = http("DELETE", f"{base}/models/{alfa}?plan_hash={dplan['plan_hash']}&approval_id={approval}", token,
                      headers={"if-match": str(variant["row_version"])})
        st3, _ = http("GET", f"{base}/variants/{alfa}", token)
        st4, _ = http("GET", f"{base}/variants/{beta}", token)
        readme_rows = q(db, "SELECT count(*) FROM artifact_location l JOIN model_artifact a ON a.id = l.artifact_id WHERE a.filename = 'README.md'")
        step(log, "borrado-de-variante", st == 201 and sorted(s["phase"] for s in dplan["steps"]) == ["DELETE_FILE", "KEEP_SHARED"]
             and st2 == 204 and st3 == 404 and st4 == 200 and not (cas / alfa_blob).exists() and readme_rows == [(1,)]
             and all((cas / p).exists() for (p,) in q(db, "SELECT relative_path FROM artifact_location")),
             "delete-plan DELETE_FILE + KEEP_SHARED; DELETE 204 con If-Match y aprobación; gguf de Alfa borrado; "
             "README compartido y Beta intactos")

        with sqlite3.connect(db) as c:
            c.execute("UPDATE model_variant SET availability = 'AVAILABLE' WHERE id = ?", (beta,))
        aside = cas.with_name("cas-fuera")
        cas.rename(aside)
        nulls = {"resource_id": None, "profile_id": None}
        _, rec = http("POST", f"{base}/storage-locations/{lid}/probe", token, nulls, {"idempotency-key": "p1"})
        wait_job(base, token, rec["job_id"])
        rc, r1, err = daemon(data, "storage-reconcile")
        states = {s for (s,) in q(db, "SELECT state FROM artifact_location")}
        avail = q(db, "SELECT availability, lifecycle FROM model_variant WHERE id = ?", (beta,))
        aside.rename(cas)
        _, rec = http("POST", f"{base}/storage-locations/{lid}/probe", token, nulls, {"idempotency-key": "p2"})
        wait_job(base, token, rec["job_id"])
        rc2, r2, err2 = daemon(data, "storage-reconcile")
        states2 = {s for (s,) in q(db, "SELECT state FROM artifact_location")}
        avail2 = q(db, "SELECT availability FROM model_variant WHERE id = ?", (beta,))
        step(log, "raiz-desaparece-y-vuelve", rc == 0 and states == {"MISSING"} and avail[0][0] == "OFFLINE"
             and avail[0][1] == "DISCOVERED" and rc2 == 0 and r2["reverified"] == 2 and states2 == {"ONLINE"}
             and avail2 == [("AVAILABLE",)],
             f"carpeta renombrada: MISSING ({r1 and r1['missing']} filas), Beta OFFLINE sin tocar su etapa; de vuelta: "
             "2 ficheros releídos ONLINE y Beta AVAILABLE")

        stop(proc)
        proc = None
        staging = run / "staging.sqlite"
        rc, rb, err = daemon(run / "data-rebuild", "registry-rebuild", "--root", str(cas), "--out", str(staging))
        (run / "rebuild.json").write_text(json.dumps(rb, indent=1, ensure_ascii=False) if rb else err, encoding="utf-8")
        report = rb["roots"][0]["report"] if rb else {}
        restored = run / "data-restaurada"
        rc2, rs, err2 = daemon(restored, "db-restore", "--from", str(staging))
        proc, base = start(restored)
        rc3, enr, _ = daemon(restored, "enroll-principal", "--name", "p018b", "--kind", "OPERATOR", "--scopes", "read")
        st, vars_ = http("GET", f"{base}/variants", enr["token"]) if rc3 == 0 else (0, None)
        step(log, "reconstruccion-y-restore", rc == 0 and len(report.get("recovered", [])) == 1
             and report["recovered"][0][0] == beta and report["incomplete"] == [] and rc2 == 0 and rs["integrity"] == "ok"
             and st == 200 and len(vars_["items"]) == 1,
             "manifiestos → base de staging (Beta recuperada; el de Alfa ya no existe); db-restore verificado en otra "
             "carpeta de datos y el daemon sirve la variante recuperada")
    finally:
        if proc is not None:
            stop(proc)
        for p in run.rglob("*"):
            if p.is_file():
                try:
                    os.chmod(p, stat.S_IWRITE | stat.S_IREAD)
                except OSError:
                    pass
        RUNS.mkdir(parents=True, exist_ok=True)
        (RUNS / "serve-steps.json").write_text(json.dumps(log, indent=2, ensure_ascii=False), encoding="utf-8")
    return f"{len(log)} pasos PASS"


if __name__ == "__main__":
    sys.stdout.reconfigure(encoding="utf-8")
    try:
        print("PASS " + {"serve": serve}[sys.argv[1]]())
        raise SystemExit(0)
    except (AssertionError, KeyError, OSError, ValueError, TypeError, IndexError, StopIteration,
            subprocess.SubprocessError, sqlite3.Error) as e:
        print(f"FAIL {type(e).__name__}: {e}")
        raise SystemExit(1)
