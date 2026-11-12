#!/usr/bin/env python3
"""P024: actualización ZIP y recuperación con binario/profiler reales en Windows.

Usa raíz/principal propios. Nunca ejecuta los workers copiados ni acredita
conformidad P026; la API POST update aún no está conectada a este coordinador.
"""
import hashlib
import json
import platform
import sqlite3
import subprocess
import sys
import time
import uuid
import zipfile
from datetime import datetime, timezone
from pathlib import Path

from p005_real import REPO, EXE, cli, clean_env, step
from p023_real import document, http, start, stop
from p012_real import mountvol


def sha(path):
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


def package(path, doc, worker, ballast=False):
    payload = {"bin/larm-generic-adapter.exe": worker,
               "declarative.json": json.dumps(doc, ensure_ascii=False).encode()}
    if ballast:
        payload["fixture-data.bin"] = bytes(range(256)) * (256 * 1024)  # 64 MiB, determinista, sin compresión.
    descriptors = "".join(f"{name}\0{hashlib.sha256(data).hexdigest()}\n" for name, data in sorted(payload.items()))
    manifest = doc["manifest"] | {"package_sha256": hashlib.sha256(descriptors.encode()).hexdigest()}
    with zipfile.ZipFile(path, "w", compression=zipfile.ZIP_STORED) as archive:
        for name, data in payload.items():
            archive.writestr(name, data)
        archive.writestr("manifest.json", json.dumps(manifest).encode())
    return {"package_path": str(path), "expected_hash": sha(path), "approval_id": None}, manifest


def state(db):
    with sqlite3.connect(db) as conn:
        return {table: conn.execute(f"SELECT * FROM {table} ORDER BY 1").fetchall()
                for table in ("adapter_installation", "adapter_selection", "operation_journal", "volume_reservation", "resource_lock", "event", "event_outbox", "idempotency")}


def main():
    out = REPO / "docs/reports/P024_runs"
    run = out / "update-real" / uuid.uuid4().hex[:12]
    for folder in ("rt", "models", "out", "packages"):
        (run / folder).mkdir(parents=True)
    data = run / "data"
    db = data / "state/larm.sqlite"
    worker = (REPO / "target/debug/larm-generic-adapter.exe").read_bytes()
    (run / "rt/fake_runtime.exe").write_bytes(worker)
    (run / "models/m.gguf").write_bytes(b"GGUF")
    logs, calls, fixtures = [], [], []
    daemon = copying = None

    def command(*args):
        return [str(EXE), "--data-root", str(data), *map(str, args)]

    def invoke(*args):
        process = subprocess.run(command(*args), capture_output=True, env=clean_env(), timeout=120)
        value = json.loads(process.stdout if process.returncode == 0 else process.stderr)
        calls.append({"args": list(map(str, args)), "exit_code": process.returncode, "observed": value})
        return process.returncode, value

    def request_file(version, ballast=False, protocol=None):
        doc = document(run, version, secret=False)
        if protocol:
            doc["manifest"]["protocol_version"] = protocol
        request, manifest = package(run / "packages" / f"{version}.zip", doc, worker, ballast)
        file = run / f"request-{version}.json"
        file.write_text(json.dumps(request), encoding="utf-8")
        fixtures.append({"request": request, "manifest": manifest, "ballast": ballast})
        return file, request, manifest

    try:
        writer = cli(data, "enroll-principal", "--name", "p024-update-writer", "--kind", "OPERATOR", "--scopes", "read,adapters:write")["token"]
        daemon, base = start(data)
        status, old = http("POST", f"{base}/adapters/declarative", writer, document(run, "1.0.0", secret=False), {"idempotency-key": "source"})
        assert status == 201, old
        stop(daemon)
        daemon = None
        old_dir = data / "adapters" / old["adapter_id"] / "1.0.0"
        old_hashes = {str(p.relative_to(old_dir)): sha(p) for p in old_dir.rglob("*") if p.is_file()}
        file, request, manifest = request_file("2.0.0")
        status, result = invoke("update-adapter", "--installation", old["id"], "--request", file, "--idempotency-key", "update-v2")
        step(logs, "actualizacion-con-profiler-nativo", status == 0 and result["job"]["state"] == "COMPLETED"
             and result["job"]["result"]["promoted"] is False, "CLI real mide volumen, reserva, copia ZIP, verifica y registra candidata; no promueve")
        job_id = result["receipt"]["job_id"]
        observed = state(db)
        journal = [r for r in observed["operation_journal"] if r[0] == job_id][0]
        plan = json.loads(journal[5])["plan"]
        final = Path(plan["destination_path"])
        with zipfile.ZipFile(request["package_path"]) as archive:
            unpacked = sum(i.file_size for i in archive.infolist())
        peak = Path(request["package_path"]).stat().st_size + unpacked
        volume_oracle = mountvol()
        expected_volume = max((len(mount), guid) for guid, mounts in volume_oracle.items()
                              for mount in mounts if data.resolve().is_relative_to(Path(mount).resolve()))[1]
        (run / "volume-oracle.json").write_text(json.dumps(volume_oracle, indent=2), encoding="utf-8")
        contents = "".join(f"{p.relative_to(final).as_posix().lower()}\0{sha(p)}\n"
                           for p in sorted(final.rglob("*")) if p.is_file() and p.name != "manifest.json")
        step(logs, "hashes-presupuesto-y-volumen", hashlib.sha256(contents.encode()).hexdigest() == manifest["package_sha256"]
             and int(plan["required_bytes"]) == peak + max(1 << 30, (peak + 19) // 20)
             and plan["volume_key"] == expected_volume,
             "Python contrasta hash de contenido y pico+margen; GUID del volumen coincide con mountvol como oráculo independiente")
        candidate = [r for r in observed["adapter_installation"] if r[0] == result["job"]["resource_id"]][0]
        step(logs, "registro-sin-conformidad-inventada", candidate[-1] == "INSTALLED" and candidate[10] == "UNTRUSTED"
             and not observed["adapter_selection"] and not observed["volume_reservation"] and not observed["resource_lock"]
             and all(sha(old_dir / relative) == value for relative, value in old_hashes.items()),
             "candidata INSTALLED/UNTRUSTED, sin default ni recursos vivos; origen conserva exactamente sus bytes")
        status, replay = invoke("update-adapter", "--installation", old["id"], "--request", file, "--idempotency-key", "update-v2")
        step(logs, "replay-tras-cerrar-proceso", status == 0 and replay == result and state(db) == observed,
             "otro proceso devuelve receipt original y estado COMPLETED sin duplicar job, instalación ni evento")
        malformed = run / "malformed.json"
        malformed.write_text(json.dumps({"package_path": request["package_path"], "expected_hash": request["expected_hash"]}), encoding="utf-8")
        status, error = invoke("update-adapter", "--installation", old["id"], "--request", malformed, "--idempotency-key", "malformed")
        step(logs, "dto-incompleto-sin-efectos", status != 0 and error["code"] == "VALIDATION_FAILED" and state(db) == observed,
             "approval_id ausente se rechaza sin encolar ni escribir paquetes")
        wrong, _, _ = request_file("bad-protocol", protocol="2.0")
        status, error = invoke("update-adapter", "--installation", old["id"], "--request", wrong, "--idempotency-key", "wrong-protocol")
        after = state(db)
        step(logs, "protocolo-incompatible-con-journal", status != 0 and error["details"]["reason"] == "protocol_version_invalid"
             and len(after["adapter_installation"]) == 2 and not after["volume_reservation"] and not after["resource_lock"]
             and not (data / "adapters" / old["adapter_id"] / "bad-protocol").exists(),
             "job FAILED con causa de protocolo; sin reserva, publicación ni instalación extra")
        big, _, _ = request_file("3.0.0", ballast=True)
        copying = subprocess.Popen(command("update-adapter", "--installation", old["id"], "--request", big, "--idempotency-key", "killed-copy"),
                                   stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=clean_env(), creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0))
        interrupted = None
        deadline = time.monotonic() + 60
        while time.monotonic() < deadline and copying.poll() is None:
            with sqlite3.connect(db) as conn:
                rows = conn.execute("SELECT id,payload FROM operation_journal WHERE kind='adapter.update' AND phase='RUNNING'").fetchall()
            for identity, payload in rows:
                value = json.loads(payload)
                if value["step"] != "EXTRACTING":
                    continue
                workspace = Path(value["plan"]["workspace_path"])
                if any(p.stat().st_size > 0 for p in workspace.rglob("archive.zip") if p.exists()):
                    interrupted = identity, value
                    break
            if interrupted:
                break
            time.sleep(0.01)
        assert interrupted is not None, "no se observó copia real antes de terminar el proceso"
        copying.kill()
        copying.communicate(timeout=15)
        pending = state(db)
        identity, record = interrupted
        step(logs, "muerte-real-durante-io", any(r[2] == identity for r in pending["volume_reservation"])
             and len([r for r in pending["resource_lock"] if r[1] == identity]) == 2
             and any(r[0] == identity and r[3] == "RUNNING" for r in pending["operation_journal"]),
             "proceso Windows terminado durante copia de ZIP no vacío; journal, reserva y locks siguen persistidos")
        status, recovered = invoke("recover-adapter-updates")
        after = state(db)
        recovered_row = [r for r in after["operation_journal"] if r[0] == identity][0]
        step(logs, "recuperacion-real-tras-muerte", status == 0 and recovered["reconciled_jobs"] == 1
             and recovered_row[3] == "FAILED" and json.loads(recovered_row[5])["error"]["details"]["reason"] == "adapter_update_interrupted"
             and not Path(record["plan"]["workspace_path"]).exists()
             and not Path(record["plan"]["destination_path"]).exists()
             and not after["volume_reservation"] and not after["resource_lock"]
             and all(sha(old_dir / relative) == value for relative, value in old_hashes.items()),
             "nuevo proceso prueba exclusión, limpia sólo staging propio, registra fallo y libera recursos; versiones anteriores intactas")
        print(f"PASS: {len(logs)} pasos reales de actualización y recuperación; sin ejecución/cualificación de plugins")
        return 0
    finally:
        for process in (copying, daemon):
            if process is not None and process.poll() is None:
                if process is daemon:
                    stop(process)
                else:
                    process.kill()
                    process.wait(timeout=15)
        subprocess.run(command("revoke-principal", "--name", "p024-update-writer"), capture_output=True, env=clean_env(), timeout=30)
        record = {"scope": "CLI, profiler nativo, NTFS y SQLite; muerte real en copia. HTTP se prueba en p024_api_real.py; no acredita P026",
                  "steps": logs, "calls": calls, "fixtures": fixtures, "run": str(run.relative_to(REPO)),
                  "host": platform.platform(), "python": platform.python_version(), "daemon_sha256": sha(EXE),
                  "generated_at": datetime.now(timezone.utc).isoformat()}
        (run / "steps.json").write_text(json.dumps(record, indent=2, ensure_ascii=False), encoding="utf-8")
        (out / "update-real.json").write_text(json.dumps(record, indent=2, ensure_ascii=False), encoding="utf-8")


if __name__ == "__main__":
    sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    raise SystemExit(main())
