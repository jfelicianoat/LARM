#!/usr/bin/env python3
"""Selección P024 sobre procesos reales y SQLite/NTFS. No cualifica plugins.

Las filas VALIDATED son fixtures explícitas; P026 sigue pendiente. Se prueban
la herramienta, CAS, journal/outbox, fallos, reinicio y conservación de bytes.
"""
import hashlib
import json
import platform
import sqlite3
import subprocess
import sys
import uuid
from datetime import datetime, timezone

from p005_real import EXE, REPO, clean_env, cli, step

ADAPTER = "acme.selection-fixture"
TS = "2026-10-16T10:00:00.000Z"
IDS = [f"0192aaaa-0000-7000-8000-{i:012x}" for i in range(1, 4)]


def snapshot(db):
    with sqlite3.connect(db) as conn:
        return {
            "selection": conn.execute("SELECT * FROM adapter_selection").fetchall(),
            "journal": conn.execute("SELECT id,phase,payload FROM operation_journal ORDER BY id").fetchall(),
            "events": conn.execute("SELECT event_id,resource_id,resource_version,payload FROM event ORDER BY event_id").fetchall(),
            "outbox": conn.execute("SELECT * FROM event_outbox ORDER BY event_id").fetchall(),
            "reservations": conn.execute("SELECT * FROM volume_reservation").fetchall(),
            "locks": conn.execute("SELECT * FROM resource_lock").fetchall(),
        }


def main():
    out = REPO / "docs/reports/P024_runs"
    run = out / "selection-real" / uuid.uuid4().hex[:12]
    data = run / "data"
    run.mkdir(parents=True)
    logs, calls = [], []
    db = data / "state/larm.sqlite"

    def command(*args):
        return [str(EXE), "--data-root", str(data), "adapter-selection", "--adapter-id", ADAPTER, *map(str, args)]

    def invoke(*args):
        process = subprocess.run(command(*args), capture_output=True, env=clean_env(), timeout=30)
        value = json.loads(process.stdout if process.returncode == 0 else process.stderr)
        calls.append({"args": list(map(str, args)), "exit_code": process.returncode, "observed": value})
        return process.returncode, value

    def files():
        return {str(p.relative_to(data)): hashlib.sha256(p.read_bytes()).hexdigest()
                for p in sorted((data / "adapters").rglob("*")) if p.is_file()}

    try:
        info = cli(data, "db-info")
        again = cli(data, "db-info")
        step(logs, "migracion-real", info["migrated_now"] == ["0001_bootstrap", "0002_adapter_selection", "0003_runtime_channel_identity"]
             and again["migrated_now"] == [] and info["healthy"] and again["healthy"],
             "base nueva: 0001/0002/0003 atómicas; segunda apertura no migra")
        fixtures = []
        with sqlite3.connect(db) as conn:
            conn.execute("PRAGMA foreign_keys=ON")
            for index, identity in enumerate(IDS):
                version = f"{index + 1}.0.0"
                state = "INSTALLED" if index == 2 else "VALIDATED"
                folder = data / "adapters" / ADAPTER / version
                folder.mkdir(parents=True)
                payload = f"fixture de selección v{version}; no es ejecutable\n".encode()
                (folder / "package.fixture").write_bytes(payload)
                sha = hashlib.sha256(payload).hexdigest()
                manifest = {"fixture_only": "prevalidated selection; not P026", "id": ADAPTER, "version": version}
                conn.execute(
                    "INSERT INTO adapter_installation(id,created_at,updated_at,row_version,metadata,adapter_id,version,"
                    "family,package_hash,publisher,trust,manifest,state) VALUES (?,?,?,1,'{}',?,?, 'runtime',?,'fixture','VERIFIED',?,?)",
                    (identity, TS, TS, ADAPTER, version, sha, json.dumps(manifest), state))
                fixtures.append({"id": identity, "version": version, "state": state, "sha256": sha, "manifest": manifest})
        before_files = files()
        status, value = invoke()
        step(logs, "sin-seleccion-implicita", status == 0 and value == {"selection": None},
             "migrar y disponer de filas VALIDATED no activa ninguna versión")
        initial = snapshot(db)
        status, error = invoke("--promote", IDS[2], "--expected-revision", 0)
        step(logs, "installed-no-promovible", status != 0 and error["details"]["reason"] == "adapter_not_validated"
             and snapshot(db) == initial, "INSTALLED rechazada: sin selección, journal, evento, reserva ni lock")
        status, value = invoke("--promote", IDS[0], "--expected-revision", 0)
        s = value["selection"]
        step(logs, "primera-promocion-deshabilitada", status == 0 and s["active_installation_id"] == IDS[0]
             and not s["enabled"] and s["row_version"] == 1, "selección explícita; habilitación separada")
        status, value = invoke("--enabled", "true", "--expected-revision", 1)
        step(logs, "habilitacion-explicita", status == 0 and value["selection"]["enabled"]
             and value["selection"]["row_version"] == 2, "habilitar registra otra revisión")
        status, value = invoke("--promote", IDS[1], "--expected-revision", 2)
        s = value["selection"]
        step(logs, "ultimo-bueno-persistido", status == 0 and s["active_installation_id"] == IDS[1]
             and s["previous_installation_id"] == IDS[0] and s["row_version"] == 3 and s["enabled"],
             "segunda versión seleccionada; conserva primera como anterior sin reescribir sus bytes")
        before = snapshot(db)
        status, error = invoke("--rollback", "--expected-revision", 2)
        step(logs, "revision-obsoleta-real", status != 0 and error["code"] == "STALE_REVISION"
             and snapshot(db) == before, "rollback con revisión vieja rechazado sin efectos")
        with sqlite3.connect(db) as conn:
            conn.execute("CREATE TRIGGER inject_p024_outbox BEFORE INSERT ON event_outbox "
                         "BEGIN SELECT RAISE(ABORT,'P024 injected IO failure'); END")
        try:
            status, error = invoke("--rollback", "--expected-revision", 3)
            step(logs, "fallo-outbox-real", status != 0 and error["code"] == "INTERNAL" and snapshot(db) == before,
                 "fallo de SQLite en outbox revierte selección, revisión, journal y evento juntos")
        finally:
            with sqlite3.connect(db) as conn:
                conn.execute("DROP TRIGGER inject_p024_outbox")
        processes = [subprocess.Popen(command("--rollback", "--expected-revision", 3), stdout=subprocess.PIPE,
                                     stderr=subprocess.PIPE, env=clean_env()) for _ in range(2)]
        concurrent = []
        try:
            for process in processes:
                stdout, stderr = process.communicate(timeout=30)
                concurrent.append({"exit_code": process.returncode, "observed": json.loads(stdout if process.returncode == 0 else stderr)})
        finally:
            for process in processes:
                if process.poll() is None:
                    process.kill()
                    process.wait(timeout=10)
        calls.append({"parallel_rollbacks": concurrent})
        winner = [p for p in concurrent if p["exit_code"] == 0]
        loser = [p for p in concurrent if p["exit_code"] != 0]
        step(logs, "rollback-concurrente-real", len(winner) == len(loser) == 1 and loser[0]["observed"]["code"] == "STALE_REVISION",
             "dos procesos Windows simultáneos; exactamente uno confirma el rollback")
        status, value = invoke()
        s = value["selection"]
        step(logs, "rollback-tras-reinicio", status == 0 and s["active_installation_id"] == IDS[0]
             and s["previous_installation_id"] is None and s["enabled"] and s["row_version"] == 4,
             "nuevo proceso vuelve a leer la versión anterior; el rollback no alterna versiones")
        with sqlite3.connect(db) as conn:
            conn.execute("UPDATE adapter_installation SET trust='REVOKED' WHERE id=?", (IDS[0],))
        before = snapshot(db)
        status, error = invoke("--enabled", "true", "--expected-revision", 4)
        step(logs, "revocacion-real", status != 0 and error["code"] == "FORBIDDEN" and snapshot(db) == before,
             "revalida confianza antes de habilitar incluso si la selección estaba habilitada")
        status, value = invoke("--enabled", "false", "--expected-revision", 4)
        step(logs, "desactivacion-revocada-real", status == 0 and not value["selection"]["enabled"]
             and value["selection"]["row_version"] == 5, "revocar no impide deshabilitar")
        after = snapshot(db)
        with sqlite3.connect(db) as conn:
            states = conn.execute("SELECT state FROM adapter_installation ORDER BY id").fetchall()
        step(logs, "journal-outbox-y-bytes-reales", len(after["journal"]) == len(after["events"]) == len(after["outbox"]) == 5
             and all(j[1] == "COMPLETED" and json.loads(j[2])["actor_id"] == "local-operator" for j in after["journal"])
             and not after["reservations"] and not after["locks"] and files() == before_files
             and states == [("VALIDATED",), ("VALIDATED",), ("INSTALLED",)],
             "5 cambios = 5 journals/eventos/outbox; estados y paquetes conservados; sin reservas/locks huérfanos")
        (run / "fixtures.json").write_text(json.dumps(fixtures, indent=2), encoding="utf-8")
        (run / "package-hashes.json").write_text(json.dumps(before_files, indent=2), encoding="utf-8")
        (run / "final-state.json").write_text(json.dumps(after, indent=2), encoding="utf-8")
        print(f"PASS: {len(logs)} pasos de selección real; suite P026 no ejecutada ni acreditada")
        return 0
    finally:
        record = {"scope": "P024 selección y durabilidad; VALIDATED son fixtures, no cualificación P026",
                  "steps": logs, "calls": calls, "run": str(run.relative_to(REPO)),
                  "daemon_sha256": hashlib.sha256(EXE.read_bytes()).hexdigest(),
                  "host": platform.platform(), "python": platform.python_version(),
                  "generated_at": datetime.now(timezone.utc).isoformat()}
        (run / "steps.json").write_text(json.dumps(record, indent=2, ensure_ascii=False), encoding="utf-8")
        (out / "selection-real.json").write_text(json.dumps(record, indent=2, ensure_ascii=False), encoding="utf-8")


if __name__ == "__main__":
    sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    raise SystemExit(main())
