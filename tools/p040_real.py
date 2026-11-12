#!/usr/bin/env python3
"""P040 - Prueba real del registro de modelos con el daemon en el PC IA.

  python tools/p040_real.py serve

Con una base nueva (docs/reports/P040_runs/real/data) y las dos fuentes de la
fixture sembradas con sqlite3 (su API es de P042):

  1. `larm-daemon models-import` con tests/fixtures/models/import-multifuente.json:
     Qwen-X por Hugging Face y por ModelScope (misma identidad upstream) queda
     como UN modelo con dos distribuciones; «qwen-x» sin upstream es otro modelo
     y ambos se anotan como possible_duplicates; audio.diarize se acepta sin
     cualificar y el alias «ocr» se guarda como vision.ocr;
  2. repetir la importación no crea nada (idempotente);
  3. cambiar el contenido de una revisión ya registrada: exit 1, motivo
     revision_key_immutable, sin escribir;
  4. API con el daemon: GET /models (filtro capability con alias),
     /models/{id}, /model-distributions?source_id, /model-revisions?state,
     detalle, 404, filtros no previstos 400 y eventos model.state_changed.
"""
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
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
EXE = REPO / "target/debug/larm-daemon.exe"
RUNS = REPO / "docs/reports/P040_runs"
FIXTURE = REPO / "tests/fixtures/models/import-multifuente.json"
HF, MS = "0192f000-0000-7000-8000-00000000f001", "0192f000-0000-7000-8000-00000000f002"


def clean_env() -> dict:
    return {k: v for k, v in os.environ.items() if not k.startswith("LARM_")}


def step(log: list, name: str, ok: bool, detail: str) -> None:
    log.append({"step": name, "result": "PASS" if ok else "FAIL", "detail": detail})
    print(f"  [{'PASS' if ok else 'FAIL'}] {name}: {detail}", flush=True)
    if not ok:
        raise AssertionError(f"{name}: {detail}")


def daemon(data: Path, *args: str) -> subprocess.CompletedProcess:
    return subprocess.run([str(EXE), "--data-root", str(data), *args], capture_output=True, text=True,
                          encoding="utf-8", env=clean_env())


def http(url: str, token: str) -> tuple[int, dict | None]:
    req = urllib.request.Request(url)
    req.add_header("authorization", f"Bearer {token}")
    try:
        with urllib.request.urlopen(req, timeout=60) as r:
            return r.status, json.loads(r.read())
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


def q(db: Path, sql: str, args: tuple = ()) -> list:
    with sqlite3.connect(db) as c:
        return c.execute(sql, args).fetchall()


def serve() -> str:
    run = RUNS / "real"
    shutil.rmtree(run, ignore_errors=True)
    data = run / "data"
    db = data / "state/larm.sqlite"
    log: list = []
    proc = None
    try:
        enrolled = daemon(data, "enroll-principal", "--name", "p040", "--kind", "OPERATOR", "--scopes", "read")
        assert enrolled.returncode == 0, enrolled.stderr
        token = json.loads(enrolled.stdout)["token"]
        with sqlite3.connect(db) as c:
            c.execute("PRAGMA foreign_keys = OFF")
            for sid, name, url in ((HF, "huggingface", "https://huggingface.co"), (MS, "modelscope", "https://modelscope.cn")):
                c.execute("INSERT INTO model_source (id,created_at,updated_at,row_version,metadata,name,adapter_id,base_url,priority,"
                          "enabled,auth_mode,account_id,policy) VALUES (?,'2026-10-11T00:00:00.000Z','2026-10-11T00:00:00.000Z',1,'{}',?,"
                          "'adapter',?,0,1,'NONE',NULL,'{}')", (sid, name, url))

        first = daemon(data, "models-import", "--file", str(FIXTURE))
        reports = json.loads(first.stdout) if first.returncode == 0 else []
        (run / "import-1.json").write_text(first.stdout or first.stderr, encoding="utf-8")
        qwen, grouped, community = (reports + [{}, {}, {}])[:3]
        models = q(db, "SELECT id, name, capabilities, possible_duplicates FROM canonical_model ORDER BY created_at, id")
        step(log, "importacion-multifuente", first.returncode == 0 and qwen["model_created"] and not grouped["model_created"]
             and grouped["model_id"] == qwen["model_id"] and community["model_created"] and community["model_id"] != qwen["model_id"]
             and len(models) == 2 and q(db, "SELECT count(*) FROM model_distribution WHERE canonical_model_id = ?", (qwen["model_id"],)) == [(2,)]
             and community["possible_duplicates"] == [qwen["model_id"]]
             and json.loads(models[0][3]) == [community["model_id"]]
             and json.loads(models[0][2]) == ["audio.diarize", "text.generate", "tool.call", "vision.ocr"]
             and qwen["unqualified_capabilities"] == ["audio.diarize"],
             "Qwen-X por HF y ModelScope = 1 modelo con 2 distribuciones; «qwen-x» sin upstream = otro modelo, "
             "possible_duplicates en los dos; ocr→vision.ocr; audio.diarize aceptada sin cualificar")

        before = q(db, "SELECT count(*) FROM event")[0][0]
        again = daemon(data, "models-import", "--file", str(FIXTURE))
        rerun = json.loads(again.stdout) if again.returncode == 0 else []
        step(log, "idempotente", again.returncode == 0 and all(not r["model_created"] and r["distributions_created"] == 0
                                                                and r["revisions_created"] == 0 for r in rerun)
             and q(db, "SELECT count(*) FROM event")[0][0] == before and q(db, "SELECT count(*) FROM canonical_model") == [(2,)],
             "repetir la importación: nada creado, ningún evento nuevo")

        changed = json.loads(FIXTURE.read_text(encoding="utf-8"))[:1]
        changed[0]["distributions"][0]["revisions"][0]["manifest_hash"] = "9" * 64
        bad_file = run / "revision-cambiada.json"
        bad_file.write_text(json.dumps(changed), encoding="utf-8")
        bad = daemon(data, "models-import", "--file", str(bad_file))
        step(log, "revision-inmutable", bad.returncode == 1 and "revision_key_immutable" in bad.stderr
             and q(db, "SELECT count(*) FROM model_revision WHERE manifest_hash = ?", ("9" * 64,)) == [(0,)],
             f"misma clave con otro manifest_hash: exit 1, {bad.stderr.strip()[:90]}…")

        proc, base = start(data)
        st, page = http(f"{base}/models", token)
        st2, ocr = http(f"{base}/models?capability=ocr", token)
        st3, detail = http(f"{base}/models/{qwen['model_id']}", token)
        step(log, "api-modelos", st == 200 and len(page["items"]) == 2 and page["next_cursor"] is None and page["snapshot_at"]
             and st2 == 200 and [m["id"] for m in ocr["items"]] == [qwen["model_id"]]
             and st3 == 200 and detail["upstream_identity"] == "hf:Qwen/Qwen-X" and detail["metadata"]["family"] == "qwen" and detail["metadata"]["identity_evidence"]["upstream_identity"] == "hf:Qwen/Qwen-X",
             "GET /models: 2 grupos; capability=ocr (alias) devuelve Qwen-X; detalle con identidad upstream y metadata")

        st, hf = http(f"{base}/model-distributions?source_id={HF}", token)
        st2, ms = http(f"{base}/model-distributions?source_id={MS}", token)
        st3, cur = http(f"{base}/model-revisions?state=CURRENT", token)
        st4, prov = http(f"{base}/model-revisions?state=PROVISIONAL", token)
        rid = cur["items"][0]["id"] if st3 == 200 and cur["items"] else ""
        st5, rev = http(f"{base}/model-revisions/{rid}", token)
        step(log, "api-distribuciones-y-revisiones", st == 200 and len(hf["items"]) == 2 and st2 == 200 and len(ms["items"]) == 1
             and ms["items"][0]["canonical_model_id"] == qwen["model_id"] and st3 == 200 and len(cur["items"]) == 2
             and st4 == 200 and prov["items"][0]["revision_key"].startswith("pending:")
             and st5 == 200 and rev["metadata"].get("lastModified") == "2026-09-30T10:00:00Z",
             "por fuente: HF 2 (Qwen-X y la comunitaria), ModelScope 1 del mismo modelo; 2 revisiones CURRENT, "
             "1 PROVISIONAL pending:; metadata original conservada")

        missing = "0192f000-0000-7000-8000-0000000000ff"
        codes = [http(f"{base}/models/{missing}", token)[0], http(f"{base}/models/qwen", token)[0],
                 http(f"{base}/models?state=CURRENT", token)[0], http(f"{base}/model-revisions?state=ACTIVA", token)[0],
                 http(f"{base}/model-distributions?source_id=hf", token)[0], http(f"{base}/models", "token-falso")[0]]
        events = q(db, "SELECT count(*) FROM event e JOIN event_outbox o ON o.event_record_id = e.id WHERE e.event_type = 'model.state_changed'")[0][0]
        step(log, "negativos-y-eventos", codes == [404, 400, 400, 400, 400, 401] and events == 2 + 1 + 3 + 4,
             f"404 desconocido; 400 id/filtros no previstos; 401 sin token; {events} eventos model.state_changed "
             "(2 modelos + 1 duplicado + 3 distribuciones + 4 revisiones)")
    finally:
        if proc is not None:
            stop(proc)
        daemon(data, "revoke-principal", "--name", "p040")
        RUNS.mkdir(parents=True, exist_ok=True)
        (RUNS / "serve-steps.json").write_text(json.dumps(log, indent=2, ensure_ascii=False), encoding="utf-8")
    return f"{len(log)} pasos PASS"


if __name__ == "__main__":
    sys.stdout.reconfigure(encoding="utf-8")
    try:
        print("PASS " + {"serve": serve}[sys.argv[1]]())
        raise SystemExit(0)
    except (AssertionError, KeyError, OSError, ValueError, TypeError, IndexError, subprocess.SubprocessError, sqlite3.Error) as e:
        print(f"FAIL {type(e).__name__}: {e}")
        raise SystemExit(1)
