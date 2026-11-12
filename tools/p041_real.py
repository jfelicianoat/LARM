#!/usr/bin/env python3
"""P041 - Prueba real de bundles e identidad canónica con el daemon en el PC IA.

  python tools/p041_real.py serve

Sobre ficheros reales en docs/reports/P041_runs/real/files (escritos y
hasheados con hashlib, el oráculo independiente) y una base nueva con dos
fuentes sembradas con sqlite3 (su API es de P042):

  1. importación de cinco modelos sin identidad upstream:
     - «Qwen-X» (HF) y «espejo» (ModelScope) con los mismos bytes requeridos,
       configuración y tokenizador: UN modelo (IDENTICAL), dos distribuciones;
     - «Nova» con otra arquitectura y otros bytes: otro modelo, UNRELATED, sin
       anotarse como duplicado aunque comparta nombre con un tercero;
     - «Qwen-X-GGUF», cuantización con base_model y hashes padre: otro modelo
       con linaje DERIVED_MODEL;
     - «Qwen-X-Instruct», misma arquitectura y autor, otros pesos: otro modelo,
       PROBABLE_SAME_MODEL, possible_duplicates en los dos;
  2. `bundle-verify` del GGUF multimodal (2 shards + mmproj + README opcional)
     contra los ficheros: VERIFIED; sin un shard INCOMPLETE; un byte cambiado
     CORRUPT; restaurado y sin el README opcional VERIFIED;
  3. GGUF de visión sin mmproj: INCOMPLETE por rol faltante aunque estén los
     ficheros;
  4. API: /variants, /models/{id}/variants, /model-bundles?state,
     /model-artifacts?q, detalle y 404.
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
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
EXE = REPO / "target/debug/larm-daemon.exe"
RUNS = REPO / "docs/reports/P041_runs"
HF, MS = "0192f000-0000-7000-8000-00000000f001", "0192f000-0000-7000-8000-00000000f002"
MIB = 1 << 20


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


def write(dir_: Path, name: str, data: bytes) -> dict:
    path = dir_ / name
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(data)
    return {"sha256": hashlib.sha256(data).hexdigest(), "size": len(data)}


def artifact(name: str, role: str, info: dict, required: bool = True) -> dict:
    return {"filename": name, "role": role, "size_bytes": str(info["size"]), "sha256": info["sha256"], "required": required}


def model(name: str, source: str, repo: str, evidence: dict, variant: dict, upstream: str | None = None) -> dict:
    return {"name": name, "upstream_identity": upstream, "capabilities": variant["capabilities"], "evidence": evidence,
            "distributions": [{"source_id": source, "upstream_namespace": "ns", "upstream_model_id": repo, "license_id": None,
                               "revisions": [{"revision_key": "main-1", "upstream_commit": None, "manifest_hash": "0" * 64,
                                              "state": "CURRENT", "variants": [variant]}]}]}


def variant(fmt: str, quant: str | None, arch: str, caps: list[str], artifacts: list[dict]) -> dict:
    return {"format": fmt, "quantization": quant, "architecture": arch, "capabilities": caps, "artifacts": artifacts}


def serve() -> str:
    run = RUNS / "real"
    shutil.rmtree(run, ignore_errors=True)
    data, files = run / "data", run / "files"
    db = data / "state/larm.sqlite"
    log: list = []
    proc = None
    try:
        enrolled = daemon(data, "enroll-principal", "--name", "p041", "--kind", "OPERATOR", "--scopes", "read")
        assert enrolled.returncode == 0, enrolled.stderr
        token = json.loads(enrolled.stdout)["token"]
        with sqlite3.connect(db) as c:
            c.execute("PRAGMA foreign_keys = OFF")
            for sid in (HF, MS):
                c.execute("INSERT INTO model_source (id,created_at,updated_at,row_version,metadata,name,adapter_id,base_url,priority,"
                          "enabled,auth_mode,account_id,policy) VALUES (?,'2026-10-11T00:00:00.000Z','2026-10-11T00:00:00.000Z',1,'{}',"
                          "'fuente','adapter','https://example.invalid',0,1,'NONE',NULL,'{}')", (sid,))

        st = files / "qwen-st"
        w1, w2 = write(st, "model-00001-of-00002.safetensors", os.urandom(3 * MIB)), write(st, "model-00002-of-00002.safetensors", os.urandom(MIB))
        cfg = write(st, "config.json", json.dumps({"architectures": ["Qwen2ForCausalLM"], "hidden_size": 3584}).encode())
        tok = write(st, "tokenizer.json", json.dumps({"model": {"type": "BPE"}}).encode())
        st_artifacts = [artifact("model-00001-of-00002.safetensors", "weights", w1), artifact("model-00002-of-00002.safetensors", "weights", w2),
                        artifact("config.json", "config", cfg), artifact("tokenizer.json", "tokenizer", tok)]
        qwen_ev = {"architecture": "qwen2", "author": "Qwen", "config_hash": cfg["sha256"], "tokenizer_hash": tok["sha256"]}
        gg = files / "qwen-gguf"
        s1, s2 = write(gg, "qwen-x-q4_k_m-00001-of-00002.gguf", os.urandom(2 * MIB)), write(gg, "qwen-x-q4_k_m-00002-of-00002.gguf", os.urandom(MIB))
        mm = write(gg, "mmproj-qwen-x-f16.gguf", os.urandom(512 * 1024))
        rd = write(gg, "README.md", "# Qwen-X GGUF\n".encode())
        gguf_artifacts = [artifact("qwen-x-q4_k_m-00001-of-00002.gguf", "weights", s1), artifact("qwen-x-q4_k_m-00002-of-00002.gguf", "weights", s2),
                          artifact("mmproj-qwen-x-f16.gguf", "mmproj", mm), artifact("README.md", "other", rd, required=False)]
        blind = files / "ciego"
        solo = write(blind, "solo.gguf", os.urandom(MIB))
        nova_dir = files / "nova"
        nova_w = write(nova_dir, "nova.safetensors", os.urandom(MIB))
        ft_w = write(files / "ft", "model.safetensors", os.urandom(MIB))
        imports = [
            model("Qwen-X", HF, "Qwen-X", qwen_ev, variant("SafeTensors", "BF16", "qwen2", ["text.generate"], st_artifacts)),
            model("espejo de Qwen", MS, "mirror-qwen", qwen_ev, variant("SafeTensors", "BF16", "qwen2", ["text.generate"], st_artifacts)),
            model("Nova", HF, "nova-llama", {"architecture": "llama", "author": "otro"},
                  variant("SafeTensors", "BF16", "llama", ["text.generate"], [artifact("nova.safetensors", "weights", nova_w)])),
            model("Qwen-X-GGUF", HF, "Qwen-X-GGUF", {"architecture": "qwen2", "parent_hashes": [w1["sha256"], w2["sha256"]]},
                  variant("GGUF", "Q4_K_M", "qwen2", ["text.generate", "vision.understand"], gguf_artifacts)),
            model("Qwen-X-Instruct", MS, "qwen-x-instruct", {"architecture": "qwen2", "author": "Qwen", "tokenizer_hash": tok["sha256"]},
                  variant("SafeTensors", "BF16", "qwen2", ["text.generate"], [artifact("model.safetensors", "weights", ft_w)])),
            model("Ciego", MS, "ciego", {"architecture": "otra"},
                  variant("GGUF", "Q8_0", "otra", ["vision.understand"], [artifact("solo.gguf", "weights", solo)])),
        ]
        fixture = run / "import.json"
        fixture.write_text(json.dumps(imports, indent=1), encoding="utf-8")
        out = daemon(data, "models-import", "--file", str(fixture))
        (run / "import-report.json").write_text(out.stdout or out.stderr, encoding="utf-8")
        r = json.loads(out.stdout) if out.returncode == 0 else []
        qwen, mirror, nova, gguf, instruct, ciego = (r + [{}] * 6)[:6]
        grade = lambda rep, mid: next((x["grade"] for x in rep.get("resolutions", []) if x["model_id"] == mid), None)
        step(log, "hashes-identicos", out.returncode == 0 and not mirror["model_created"] and mirror["model_id"] == qwen["model_id"]
             and grade(mirror, qwen["model_id"]) == "IDENTICAL"
             and q(db, "SELECT count(*) FROM model_distribution WHERE canonical_model_id = ?", (qwen["model_id"],)) == [(2,)],
             "otro nombre y otra fuente con los mismos bytes requeridos, config y tokenizador: un modelo, dos distribuciones")
        lineage = json.loads(q(db, "SELECT metadata FROM canonical_model WHERE id = ?", (gguf["model_id"],))[0][0]).get("lineage", [])
        step(log, "cuantizacion-derivada", gguf["model_created"] and grade(gguf, qwen["model_id"]) == "DERIVED_MODEL"
             and lineage and lineage[0]["model_id"] == qwen["model_id"] and gguf["possible_duplicates"] == [],
             "GGUF con hashes padre de los safetensors: modelo propio con linaje DERIVED_MODEL, sin fusión")
        dup_first = json.loads(q(db, "SELECT possible_duplicates FROM canonical_model WHERE id = ?", (qwen["model_id"],))[0][0])
        step(log, "probable-sin-fusion", instruct["model_created"] and grade(instruct, qwen["model_id"]) == "PROBABLE_SAME_MODEL"
             and instruct["possible_duplicates"] == [qwen["model_id"]] and dup_first == [instruct["model_id"]],
             "misma arquitectura, autor y tokenizador, otros pesos: PROBABLE_SAME_MODEL, possible_duplicates en los dos")
        step(log, "otro-modelo", nova["model_created"] and nova["possible_duplicates"] == [] and len({m["model_id"] for m in r}) == 5,
             "Nova (llama) no se agrupa con nada; 6 importaciones = 5 modelos")

        gguf_variant = gguf["variant_ids"][0]
        verify = lambda: json.loads(daemon(data, "bundle-verify", "--variant", gguf_variant, "--dir", str(gg)).stdout or "{}")
        v1 = verify()
        shard = gg / "qwen-x-q4_k_m-00002-of-00002.gguf"
        shard.rename(gg / "aparte.bin")
        v2 = verify()
        (gg / "aparte.bin").rename(shard)
        original = (gg / "mmproj-qwen-x-f16.gguf").read_bytes()
        (gg / "mmproj-qwen-x-f16.gguf").write_bytes(original[:100] + bytes([original[100] ^ 0xFF]) + original[101:])
        v3 = verify()
        (gg / "mmproj-qwen-x-f16.gguf").write_bytes(original)
        (gg / "README.md").unlink()
        v4 = verify()
        step(log, "bundle-en-disco", v1.get("state") == "VERIFIED" and v2.get("state") == "INCOMPLETE"
             and v2.get("missing") == ["qwen-x-q4_k_m-00002-of-00002.gguf"] and v3.get("state") == "CORRUPT"
             and v3.get("mismatched") == ["mmproj-qwen-x-f16.gguf"] and v4.get("state") == "VERIFIED" and v4.get("row_version") == 5,
             "2 shards + mmproj + README: VERIFIED; sin un shard INCOMPLETE; un byte del mmproj CORRUPT; "
             "restaurado y sin el README opcional VERIFIED (CAS v5)")
        vc = json.loads(daemon(data, "bundle-verify", "--variant", ciego["variant_ids"][0], "--dir", str(blind)).stdout or "{}")
        step(log, "rol-faltante", vc.get("state") == "INCOMPLETE" and vc.get("missing_roles") == ["mmproj"] and vc.get("missing") == [],
             "GGUF de visión sin mmproj: ficheros completos pero INCOMPLETE por rol necesario")

        proc, base = start(data)
        s1_, allv = http(f"{base}/variants", token)
        s2_, ofq = http(f"{base}/models/{qwen['model_id']}/variants", token)
        s3_, corrupt = http(f"{base}/model-bundles?state=VERIFIED", token)
        s4_, arts = http(f"{base}/model-artifacts?q=mmproj", token)
        s5_, det = http(f"{base}/variants/{gguf_variant}", token)
        s6_ = http(f"{base}/model-bundles/0192f000-0000-7000-8000-0000000000ff", token)[0]
        s7_ = http(f"{base}/variants?state=LISTO", token)[0]
        step(log, "api", s1_ == 200 and len(allv["items"]) == 6 and s2_ == 200 and len(ofq["items"]) == 2
             and s3_ == 200 and len(corrupt["items"]) == 1 and s4_ == 200 and arts["items"][0]["filename"] == "mmproj-qwen-x-f16.gguf"
             and s5_ == 200 and det["format"] == "GGUF" and det["bundle_hash"] and s6_ == 404 and s7_ == 400,
             "6 variantes; 2 del modelo agrupado; 1 bundle VERIFIED; artefacto por nombre; detalle con bundle_hash; 404 y 400")
    finally:
        if proc is not None:
            stop(proc)
        daemon(data, "revoke-principal", "--name", "p041")
        shutil.rmtree(files, ignore_errors=True)
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
