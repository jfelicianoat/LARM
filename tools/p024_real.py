#!/usr/bin/env python3
"""Pruebas reales de la preparación implementada de P024; no acredita la fase completa."""
import hashlib
import json
import shutil
import sqlite3
import subprocess
import sys
import uuid
import zipfile
from pathlib import Path

from p005_real import REPO, EXE, cli
from p023_real import clean_env, document, http, start, stop, step


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def call(*args):
    process = subprocess.run([str(EXE), *map(str, args)], capture_output=True, env=clean_env(), timeout=60)
    raw = process.stdout if process.returncode == 0 else process.stderr
    return process.returncode, json.loads(raw)


def package(path: Path, doc: dict, worker: bytes, extra=None):
    payload = {"bin/larm-generic-adapter.exe": worker,
               "declarative.json": json.dumps(doc, ensure_ascii=False).encode()}
    descriptors = "".join(f"{name}\0{hashlib.sha256(data).hexdigest()}\n" for name, data in sorted(payload.items()))
    manifest = doc["manifest"] | {"package_sha256": hashlib.sha256(descriptors.encode()).hexdigest()}
    with zipfile.ZipFile(path, "w", compression=zipfile.ZIP_DEFLATED) as archive:
        for name, data in payload.items():
            archive.writestr(name, data)
        archive.writestr("manifest.json", json.dumps(manifest).encode())
        if extra:
            archive.writestr(*extra)
    return digest(path), manifest


def main():
    out = REPO / "docs/reports/P024_runs"
    run = out / "preparation-real" / uuid.uuid4().hex[:12]
    for folder in ("registry", "rt", "models", "out"):
        (run / folder).mkdir(parents=True)
    worker = (REPO / "target/debug/larm-generic-adapter.exe").read_bytes()
    # Ejecutable real en los paquetes. La inspección y el listado no lo ejecutan.
    (run / "rt/fake_runtime.exe").write_bytes(worker)
    (run / "models/m.gguf").write_bytes(b"GGUF")
    logs = []
    daemon = None
    data = run / "data"
    try:
        packages = {}
        for version in ("2.0.0", "1.0.0"):
            doc = document(run, version, secret=False)
            path = run / "registry" / f"{version}.zip"
            archive_hash, manifest = package(path, doc, worker)
            packages[version] = (path, archive_hash, manifest)
        status, result = call("inspect-adapter-registry", "--folder", run / "registry")
        step(logs, "registro-local-real", status == 0 and [p["version"] for p in result["packages"]] == ["1.0.0", "2.0.0"]
             and not (data / "adapters").exists(), "dos ZIP con worker real; orden estable; discovery sin instalación")
        again = call("inspect-adapter-registry", "--folder", run / "registry")
        step(logs, "inspeccion-reproducible", again == (status, result), "dos consultas independientes dan el mismo descriptor")
        for version, (path, archive_hash, manifest) in packages.items():
            status, inspected = call("inspect-adapter-package", "--file", path, "--expected-hash", archive_hash)
            value = inspected["packages"][0]
            with zipfile.ZipFile(path) as archive:
                unpacked = sum(item.file_size for item in archive.infolist())
            peak = path.stat().st_size + unpacked
            step(logs, f"hashes-y-presupuesto-{version}", status == 0 and value["archive_sha256"] == archive_hash
                 and value["package_sha256"] == manifest["package_sha256"] and int(value["unpacked_bytes"]) == unpacked
                 and int(value["required_free_bytes"]) == peak + max(1 << 30, (peak + 19) // 20), "hash ZIP, hash P022 y pico+ margen contrastados con Python zipfile")
        bad = run / "escape.zip"
        archive_hash, _ = package(bad, document(run, "bad", secret=False), worker, ("../escape.exe", b"bad"))
        status, error = call("inspect-adapter-package", "--file", bad, "--expected-hash", archive_hash)
        step(logs, "zip-slip-real", status != 0 and error["details"]["reason"] == "package_path_invalid" and not (run / "escape.exe").exists(), "ZIP hostil rechazado sin extraer nada")
        wrong_protocol = document(run, "bad-protocol", secret=False)
        wrong_protocol["manifest"]["protocol_version"] = "2.0"
        bad = run / "protocol.zip"
        archive_hash, _ = package(bad, wrong_protocol, worker)
        status, error = call("inspect-adapter-package", "--file", bad, "--expected-hash", archive_hash)
        step(logs, "protocolo-incompatible-real", status != 0 and error["details"]["reason"] == "protocol_version_invalid", "major incompatible rechazado antes de instalación o worker")
        writer = cli(data, "enroll-principal", "--name", "p024-writer", "--kind", "OPERATOR", "--scopes", "read,adapters:write")["token"]
        reader = cli(data, "enroll-principal", "--name", "p024-reader", "--kind", "OPERATOR", "--scopes", "read")["token"]
        no_read = cli(data, "enroll-principal", "--name", "p024-no-read", "--kind", "OPERATOR", "--scopes", "adapters:write")["token"]
        daemon, base = start(data)
        created = []
        for index, version in enumerate(("1.0.0", "2.0.0")):
            status, row = http("POST", f"{base}/adapters/declarative", writer, document(run, version, secret=False), {"idempotency-key": f"v{index}"})
            assert status == 201, row
            created.append(row)
        status, first = http("GET", f"{base}/adapters?limit=1", reader)
        _, second = http("GET", f"{base}/adapters?limit=1&cursor={first['next_cursor']}", reader)
        step(logs, "lista-paginada-real", status == 200 and first["items"] == [created[0]] and second["items"] == [created[1]] and second["next_cursor"] is None,
             "listado del daemon real en dos páginas, sin repetición ni saltos")
        _, detail = http("GET", f"{base}/adapters/{created[1]['id']}", reader)
        with sqlite3.connect(data / "state/larm.sqlite") as connection:
            persisted = connection.execute("SELECT manifest,package_hash FROM adapter_installation WHERE id=?", (detail["id"],)).fetchone()
        step(logs, "detalle-y-sqlite-real", detail == created[1] and json.loads(persisted[0]) == detail["manifest"] and persisted[1] == detail["package_hash"], "detalle exacto de la instalación persistida; sin elevar confianza")
        status, _ = http("GET", f"{base}/adapters", no_read)
        changed_filter, bad_cursor = http("GET", f"{base}/adapters?state=INSTALLED&cursor={first['next_cursor']}", reader)
        bad_param, _ = http("GET", f"{base}/adapters?unknown=1", reader)
        step(logs, "autorizacion-y-cursor-real", status == 403 and changed_filter == 400 and bad_cursor["details"]["reason"] == "bad_cursor" and bad_param == 400,
             "sin read: 403; cursor de otro filtro y parámetro desconocido: 400")
        (run / "catalog.json").write_text(json.dumps(first, indent=2), encoding="utf-8")
        print(f"PASS: {len(logs)} pasos de ZIP/registry local y API de lectura")
        return 0
    finally:
        if daemon is not None and daemon.poll() is None:
            stop(daemon)
        for name in ("p024-writer", "p024-reader", "p024-no-read"):
            subprocess.run([str(EXE), "--data-root", str(data), "revoke-principal", "--name", name], capture_output=True, env=clean_env())
        record = {"scope": "P024 preparación implementada: ZIP/registry local y API de lectura; no fase completa", "steps": logs, "run": str(run.relative_to(REPO)), "daemon_sha256": digest(EXE)}
        (run / "steps.json").write_text(json.dumps(record, indent=2, ensure_ascii=False), encoding="utf-8")
        (out / "preparation-real.json").write_text(json.dumps(record, indent=2, ensure_ascii=False), encoding="utf-8")


if __name__ == "__main__":
    sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    raise SystemExit(main())
