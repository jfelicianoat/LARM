#!/usr/bin/env python3
"""P017 - Prueba real del CAS, la deduplicación y la importación en el PC IA.

  python tools/p017_real.py serve

Ubicaciones creadas por la API del daemon: «descargas» y «cas» en C: y una
carpeta temporal en \\\\localhost\\Temp (otra volume_key), que se borra al
final. La fuente «carpeta local» se siembra con sqlite3 (su API es de P042).
Oráculos independientes: hashlib, os.stat (st_nlink, st_mtime_ns) y el
atributo de solo lectura.

  1. carpeta mixta en «descargas» (GGUF con shards y cabecera real, Q8, mmproj,
     README, SafeTensors con config/tokenizer, un pickle de PyTorch, un .exe,
     un .txt, un .gguf sin magia y una copia duplicada del Q8):
     `models-import-dir --mode adopt` registra 4 variantes EXTERNAL/ADOPTED,
     ignora e informa los desconocidos, marca el pickle como inseguro y no
     toca ni un byte ni una fecha;
  2. reimportar: todo desde la caché de hash, ninguna fila nueva;
  3. `storage-duplicates`: el Q8 duplicado con su ahorro; no se borra nada;
  4. `--mode copy` de una carpeta externa al CAS: blobs de solo lectura con
     su sha256, el mmproj y el README compartidos una sola vez (SHARED), el
     original intacto; repetir no copia;
  5. `runtime-view`: en C: hard links (st_nlink >= 2); en \\\\localhost\\Temp,
     sin enlace posible, referencias separadas en view.json sin copiar blobs.
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
RUNS = REPO / "docs/reports/P017_runs"
SHARE = r"\\localhost\Temp"
SOURCE = "0192f000-0000-7000-8000-00000000f017"
KIB = 1024


def clean_env() -> dict:
    return {k: v for k, v in os.environ.items() if not k.startswith("LARM_")}


def step(log: list, name: str, ok: bool, detail: str) -> None:
    log.append({"step": name, "result": "PASS" if ok else "FAIL", "detail": detail})
    print(f"  [{'PASS' if ok else 'FAIL'}] {name}: {detail}", flush=True)
    if not ok:
        raise AssertionError(f"{name}: {detail}")


def daemon(data: Path, *args: str) -> tuple[int, dict | list | None, str]:
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


def gguf(arch: str, name: str, payload: int) -> bytes:
    b = b"GGUF" + struct.pack("<IQQ", 3, 0, 2)
    for k, v in (("general.architecture", arch), ("general.name", name)):
        b += struct.pack("<Q", len(k)) + k.encode() + struct.pack("<I", 8) + struct.pack("<Q", len(v)) + v.encode()
    return b + os.urandom(payload)


def safetensors(payload: int) -> bytes:
    header = json.dumps({"w": {"dtype": "F16", "shape": [payload // 2], "data_offsets": [0, payload]}}).encode()
    return struct.pack("<Q", len(header)) + header + os.urandom(payload)


def write(path: Path, data: bytes) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(data)


def snapshot(root: Path) -> dict:
    return {str(p.relative_to(root)): (hashlib.sha256(p.read_bytes()).hexdigest(), p.stat().st_mtime_ns)
            for p in sorted(root.rglob("*")) if p.is_file()}


def mixed(dir_: Path) -> None:
    write(dir_ / "qwen/qwen-x-Q4_K_M-00001-of-00002.gguf", gguf("qwen2", "Qwen X", 600 * KIB))
    write(dir_ / "qwen/qwen-x-Q4_K_M-00002-of-00002.gguf", gguf("qwen2", "Qwen X", 400 * KIB))
    write(dir_ / "qwen/qwen-x-Q8_0.gguf", gguf("qwen2", "Qwen X", 900 * KIB))
    write(dir_ / "qwen/mmproj-qwen-x-f16.gguf", gguf("clip", "proj", 100 * KIB))
    write(dir_ / "qwen/README.md", "# Qwen X\n".encode())
    write(dir_ / "st/model.safetensors", safetensors(300 * KIB))
    write(dir_ / "st/config.json", json.dumps({"model_type": "llama"}).encode())
    write(dir_ / "st/tokenizer.json", b'{"model":{"type":"BPE"}}')
    write(dir_ / "pt/pytorch_model.bin", b"\x80\x02" + os.urandom(50 * KIB))
    write(dir_ / "setup.exe", b"MZ\x90\x00" + os.urandom(KIB))
    write(dir_ / "notas.txt", "apuntes".encode())
    write(dir_ / "roto.gguf", b"NOPE")


def serve() -> str:
    run = RUNS / "real"
    shutil.rmtree(run, ignore_errors=True)
    data, roots = run / "data", run / "roots"
    downloads, cas, external = roots / "descargas", roots / "cas", run / "externo"
    for d in (downloads, cas, external):
        d.mkdir(parents=True)
    share_dir = Path(SHARE) / f"larm-p017-{uuid.uuid4().hex[:8]}"
    share_dir.mkdir()
    db = data / "state/larm.sqlite"
    log: list = []
    proc = None
    try:
        rc, enrolled, err = daemon(data, "enroll-principal", "--name", "p017", "--kind", "OPERATOR", "--scopes", "read,storage:write")
        assert rc == 0, err
        proc, base = start(data)
        ids = {}
        for name, root in (("descargas", downloads), ("cas", cas), ("red", share_dir)):
            st, loc = http("POST", f"{base}/storage-locations", enrolled["token"],
                           {"name": name, "root_path": str(root), "reserved_bytes": "0", "priority": 1,
                            "auto_placement": True, "performance_class": "UNKNOWN"}, {"idempotency-key": name})
            assert st == 201, loc
            ids[name] = loc["id"]
        stop(proc)
        proc = None
        with sqlite3.connect(db) as c:
            c.execute("PRAGMA foreign_keys = OFF")
            c.execute("INSERT INTO model_source (id,created_at,updated_at,row_version,metadata,name,adapter_id,base_url,priority,"
                      "enabled,auth_mode,account_id,policy) VALUES (?,'2026-10-11T00:00:00.000Z','2026-10-11T00:00:00.000Z',1,'{}',"
                      "'carpeta local','local','file:',0,1,'NONE',NULL,'{}')", (SOURCE,))

        mixed(downloads / "lote")
        (downloads / "lote/copia").mkdir()
        shutil.copy2(downloads / "lote/qwen/qwen-x-Q8_0.gguf", downloads / "lote/copia/qwen-x-Q8_0.gguf")
        before = snapshot(downloads)
        rc, rep, err = daemon(data, "models-import-dir", "--dir", str(downloads / "lote"), "--location", ids["descargas"],
                              "--source", SOURCE, "--mode", "adopt")
        (run / "adopt.json").write_text(json.dumps(rep, indent=1, ensure_ascii=False) if rep else err, encoding="utf-8")
        assert rc == 0, err
        with sqlite3.connect(db) as c:
            rows = c.execute("SELECT ownership, json_extract(metadata,'$.registration_mode'), relative_path FROM artifact_location").fetchall()
            pickle = c.execute("SELECT json_extract(metadata,'$.unsafe_pickle') FROM model_artifact WHERE filename = 'pytorch_model.bin'").fetchall()
        formats = sorted(v["format"] for v in rep["variants"])
        ignored = {(i["relative"], i["reason"]) for i in rep["ignored"]}
        step(log, "adopcion-carpeta-mixta", formats == ["GGUF", "GGUF", "GGUF", "PyTorch", "SafeTensors"]
             and ignored == {("notas.txt", "unknown_format"), ("setup.exe", "unknown_format"), ("roto.gguf", "gguf_magic_missing")}
             and rows and all(o == "EXTERNAL" and m == "ADOPTED" and p.startswith("lote/") for o, m, p in rows)
             and pickle == [("true",)] and snapshot(downloads) == before,
             f"{len(rep['variants'])} variantes (Q4 con 2 shards, Q8, copia del Q8, SafeTensors, PyTorch); {len(rows)} filas "
             "EXTERNAL/ADOPTED; .exe, .txt y .gguf sin magia ignorados e informados; pickle marcado inseguro; "
             "ni un byte ni una mtime cambiados")

        rc, again, err = daemon(data, "models-import-dir", "--dir", str(downloads / "lote"), "--location", ids["descargas"],
                                "--source", SOURCE, "--mode", "adopt")
        step(log, "reimportacion-con-cache", rc == 0 and again["hashed"] == 0 and again["hashed_from_cache"] == rep["hashed"]
             and again["locations_created"] == 0,
             f"{again and again['hashed_from_cache']} ficheros desde la caché (tamaño+mtime), 0 releídos, 0 filas nuevas")

        rc, dups, err = daemon(data, "storage-duplicates")
        q8 = (downloads / "lote/qwen/qwen-x-Q8_0.gguf").stat().st_size
        step(log, "duplicados", rc == 0 and len(dups) == 1 and len(dups[0]["copies"]) == 2
             and dups[0]["reclaimable_bytes"] == q8 and dups[0]["sha256"] == hashlib.sha256((downloads / "lote/copia/qwen-x-Q8_0.gguf").read_bytes()).hexdigest()
             and (downloads / "lote/copia/qwen-x-Q8_0.gguf").exists(),
             f"Q8 en dos carpetas del mismo volumen: ahorro posible {q8} bytes; nada borrado")

        mixed(external)
        ext_before = snapshot(external)
        rc, copied, err = daemon(data, "models-import-dir", "--dir", str(external), "--location", ids["cas"],
                                 "--source", SOURCE, "--mode", "copy")
        (run / "copy.json").write_text(json.dumps(copied, indent=1, ensure_ascii=False) if copied else err, encoding="utf-8")
        assert rc == 0, err
        with sqlite3.connect(db) as c:
            cas_rows = c.execute("SELECT relative_path, ownership, sha256 FROM artifact_location WHERE storage_location_id = ?",
                                 (ids["cas"],)).fetchall()
        blobs_ok = all(
            p.startswith("blobs/sha256/") and hashlib.sha256((cas / p).read_bytes()).hexdigest() == sha
            and not os.stat(cas / p).st_mode & stat.S_IWRITE
            for p, _, sha in cas_rows)
        shared = sum(1 for _, o, _ in cas_rows if o == "SHARED")
        rc2, again2, _ = daemon(data, "models-import-dir", "--dir", str(external), "--location", ids["cas"],
                                "--source", SOURCE, "--mode", "copy")
        step(log, "copia-al-cas", copied["blobs_copied"] == 9 and copied["blobs_already_present"] == 2 and blobs_ok and shared == 4
             and snapshot(external) == ext_before and rc2 == 0 and again2["blobs_copied"] == 0 and again2["locations_created"] == 0,
             f"9 blobs copiados y verificados de solo lectura; mmproj y README una vez para dos variantes ({shared} filas SHARED); "
             "original intacto; repetir no copia")

        q8_variant = next(v["variant_id"] for v in copied["variants"] if v["format"] == "GGUF" and v["files"] == 3)
        rc, local_view, err = daemon(data, "runtime-view", "--location", ids["cas"], "--variant", q8_variant, "--out", str(run / "vista"))
        links = [os.stat(run / "vista" / e["name"]).st_nlink for e in (local_view or [])]
        rc2, net_view, err2 = daemon(data, "runtime-view", "--location", ids["cas"], "--variant", q8_variant, "--out", str(share_dir / "vista"))
        manifest = json.loads((share_dir / "vista" / "view.json").read_text(encoding="utf-8")) if rc2 == 0 else []
        step(log, "vista-y-fallback", rc == 0 and len(local_view) == 3 and all(e["kind"] == "LINKED" for e in local_view)
             and all(n >= 2 for n in links)
             and rc2 == 0 and all(e["kind"] == "REFERENCE" for e in net_view) and manifest == net_view
             and not any((share_dir / "vista" / e["name"]).exists() for e in net_view),
             f"C:: 3 hard links (st_nlink {links}); \\\\localhost\\Temp: sin enlace posible "
             f"({net_view[0]['reason'] if net_view else '?'}), referencias en view.json y ningún blob copiado")
    finally:
        if proc is not None:
            stop(proc)
        for p in cas.rglob("*"):
            if p.is_file():
                os.chmod(p, stat.S_IWRITE | stat.S_IREAD)
        shutil.rmtree(share_dir, ignore_errors=True)
        daemon(data, "revoke-principal", "--name", "p017")
        RUNS.mkdir(parents=True, exist_ok=True)
        (RUNS / "serve-steps.json").write_text(json.dumps(log, indent=2, ensure_ascii=False), encoding="utf-8")
    step(log, "limpieza", not share_dir.exists(), f"{share_dir} borrada")
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
