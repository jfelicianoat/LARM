#!/usr/bin/env python3
"""P004 - Pruebas reales de persistencia sobre el daemon y ficheros NTFS del PC IA.

Cada subcomando trabaja en docs/reports/P004_runs/<caso> y sale con 0 si pasa.

  dbinfo          `larm-daemon db-info` dos veces sobre una raíz nueva: la primera migra
                  las 14 migraciones hasta adquisición 3.12.0; segunda no aplica nada;
                  base sana (WAL, FK, busy 5000), 54 tablas y sin rutas personales
  backup-restore  filas reales -> db-backup -> se borran -> db-restore -> vuelven; la base previa
                  queda como *.pre-restore
  restore-in-use  otro proceso mantiene la base abierta: db-restore falla con
                  RESOURCE_UNAVAILABLE y la base viva queda intacta, sin restos .restoring
  busy-timeout    otro proceso retiene el bloqueo de escritura de una base nueva: db-info, que
                  debe migrar, espera busy_timeout (5 s) y falla sin dejar el esquema a medias;
                  liberado el bloqueo, migra entera. En WAL un backup (lectura) sí convive con
                  ese escritor: también se comprueba
"""
import json
import os
import re
import sqlite3
import subprocess
import sys
import threading
import time
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
EXE = REPO / "target/debug/larm-daemon.exe"
RUNS = REPO / "docs/reports/P004_runs"
TS = "2026-10-11T00:00:00.000Z"
EXPECTED_MIGRATIONS = [
    "0001_bootstrap", "0002_adapter_selection", "0003_runtime_channel_identity",
    "0004_runtime_profile_locks", "0005_runtime_profile_qualifications",
    "0006_p052_artifact_size_estimate", "0007_p098_local_import_source",
    "0008_p093_failed_recoverable", "0009_p028_signed_adapter_registry",
    "0010_p028_registry_url_binding", "0011_p127_source_operational_snapshot",
    "0012_p128_low_disk_transition", "0013_p032_runtime_version_pinning_and_retention",
    "0014_model_acquisition_manifest",
]


def case_dir(name: str) -> Path:
    d = RUNS / name / f"run-{time.time_ns()}"
    d.mkdir(parents=True)
    return d


def daemon(root: Path, *args: str, timeout: int = 60) -> subprocess.CompletedProcess:
    env = {k: v for k, v in os.environ.items() if not k.startswith("LARM_")}
    return subprocess.run([str(EXE), "--data-root", str(root), *args], env=env, capture_output=True,
                          timeout=timeout, text=True, encoding="utf-8", errors="replace", check=False)


def db_path(root: Path) -> Path:
    return root / "state/larm.sqlite"


def seed(root: Path, n: int) -> None:
    con = sqlite3.connect(db_path(root))
    con.execute("PRAGMA foreign_keys=ON")
    for i in range(n):
        con.execute("INSERT INTO canonical_model(id,created_at,updated_at,row_version,metadata,name,capabilities,"
                    "possible_duplicates) VALUES (?,?,?,1,'{}',?,'[\"text.generate\"]','[]')",
                    (f"cm-{i}", TS, TS, f"modelo-{i}"))
    con.commit()
    con.close()


def count(root: Path) -> int:
    con = sqlite3.connect(db_path(root))
    try:
        return con.execute("SELECT COUNT(*) FROM canonical_model").fetchone()[0]
    finally:
        con.close()


def dbinfo() -> str:
    root = case_dir("dbinfo")
    first = daemon(root, "db-info")
    second = daemon(root, "db-info")
    for p in (first, second):
        if p.returncode != 0:
            raise AssertionError(f"exit {p.returncode}: {p.stderr[-300:]}")
    a, b = json.loads(first.stdout), json.loads(second.stdout)
    (root / "db-info-1.json").write_text(first.stdout, encoding="utf-8")
    (root / "db-info-2.json").write_text(second.stdout, encoding="utf-8")
    if a["migrated_now"] != EXPECTED_MIGRATIONS or b["migrated_now"] != []:
        raise AssertionError(f"migrated_now {a['migrated_now']} / {b['migrated_now']}")
    if a["migrations"] != b["migrations"] or not a["healthy"] or not b["healthy"]:
        raise AssertionError("schema_metadata cambió o la base no está sana")
    if a["tables"] != 54:
        raise AssertionError(f"tablas {a['tables']} != 54 (49 contractuales + 5 auxiliares)")
    personal = [s for s in (str(root), os.environ.get("USERPROFILE", "?"), "AppData") if s in first.stdout]
    if personal:
        raise AssertionError(f"la salida contiene rutas personales: {personal}")
    if not re.fullmatch(r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{3}Z", a["migrations"][0]["applied_at"]):
        raise AssertionError(f"applied_at fuera de formato: {a['migrations'][0]['applied_at']}")
    return (f"1ª: 14 migraciones hasta adquisición 3.12.0, bootstrap sha256 {a['migrations'][0]['sha256'][:12]}…; 2ª: nada; "
            f"{a['tables']} tablas; WAL/FK/busy 5000; SQLite {a['sqlite_version']}; sin rutas personales")


def backup_restore() -> str:
    root = case_dir("backup-restore")
    if daemon(root, "db-info").returncode != 0:
        raise AssertionError("no se pudo crear la base")
    seed(root, 25)
    backup = root / "backups/larm-backup.sqlite"
    b = daemon(root, "db-backup", "--out", str(backup))
    if b.returncode != 0 or json.loads(b.stdout)["verification"]["backup_rows"]["canonical_model"] != 25:
        raise AssertionError(f"db-backup: {b.returncode} {b.stdout[-300:]} {b.stderr[-300:]}")
    con = sqlite3.connect(db_path(root))
    con.execute("DELETE FROM canonical_model")
    con.commit()
    con.close()
    if count(root) != 0:
        raise AssertionError("no se borraron las filas")
    r = daemon(root, "db-restore", "--from", str(backup))
    if r.returncode != 0:
        raise AssertionError(f"db-restore: {r.returncode} {r.stderr[-400:]}")
    report = json.loads(r.stdout)
    restored = count(root)
    pre = db_path(root).with_name("larm.sqlite.pre-restore")
    if restored != 25 or not report["previous_kept"] or not pre.exists():
        raise AssertionError(f"filas {restored}, previa conservada {report['previous_kept']}/{pre.exists()}")
    info = json.loads(daemon(root, "db-info").stdout)
    if not info["healthy"] or info["migrated_now"]:
        raise AssertionError("la base restaurada no está sana o requería migrar")
    return "25 filas -> backup verificado -> borradas -> restore -> 25 filas; previa en larm.sqlite.pre-restore; base sana"


def restore_in_use() -> str:
    root = case_dir("restore-in-use")
    daemon(root, "db-info")
    seed(root, 7)
    backup = root / "b.sqlite"
    if daemon(root, "db-backup", "--out", str(backup)).returncode != 0:
        raise AssertionError("backup")
    holder = sqlite3.connect(db_path(root))
    holder.execute("SELECT COUNT(*) FROM canonical_model").fetchone()
    try:
        r = daemon(root, "db-restore", "--from", str(backup))
    finally:
        holder.close()
    if r.returncode == 0 or "RESOURCE_UNAVAILABLE" not in r.stderr:
        raise AssertionError(f"se esperaba RESOURCE_UNAVAILABLE: rc={r.returncode} {r.stderr[-300:]}")
    leftovers = [p.name for p in db_path(root).parent.iterdir() if p.name.endswith((".restoring", ".pre-restore"))]
    if leftovers or count(root) != 7:
        raise AssertionError(f"restos {leftovers} o filas {count(root)}")
    return "con la base abierta por otro proceso: exit 1 RESOURCE_UNAVAILABLE; 7 filas intactas y sin restos"


def busy_timeout() -> str:
    root = case_dir("busy-timeout")
    path = db_path(root)
    path.parent.mkdir(parents=True)
    locker = sqlite3.connect(path, timeout=1, check_same_thread=False)
    locker.execute("PRAGMA journal_mode=WAL")
    locker.execute("BEGIN EXCLUSIVE")
    release = threading.Event()

    def hold() -> None:
        release.wait(60)
        locker.rollback()

    t = threading.Thread(target=hold)
    t.start()
    try:
        start = time.monotonic()
        p = daemon(root, "db-info", timeout=60)
        elapsed = time.monotonic() - start
    finally:
        release.set()
        t.join()
    tables_after_fail = locker.execute(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name NOT IN ('schema_metadata','sqlite_sequence')").fetchone()[0]
    if p.returncode == 0 or "RESOURCE_UNAVAILABLE" not in p.stderr:
        raise AssertionError(f"se esperaba RESOURCE_UNAVAILABLE: rc={p.returncode} {p.stderr[-300:]}")
    if elapsed < 4.5 or elapsed > 40:
        raise AssertionError(f"tiempo {elapsed:.1f}s fuera de busy_timeout (5 s)")
    if tables_after_fail != 0:
        raise AssertionError(f"migración a medias: {tables_after_fail} tablas del DDL tras el fallo")
    ok = daemon(root, "db-info")
    if ok.returncode != 0 or json.loads(ok.stdout)["migrated_now"] != EXPECTED_MIGRATIONS:
        raise AssertionError(f"tras liberar no migra: {ok.stderr[-300:]}")
    # con la base ya migrada, un escritor activo no impide un backup (lectura en WAL)
    locker.execute("BEGIN IMMEDIATE")
    try:
        b = daemon(root, "db-backup", "--out", str(root / "b.sqlite"))
    finally:
        locker.rollback()
        locker.close()
    if b.returncode != 0:
        raise AssertionError(f"backup con escritor concurrente falló: {b.stderr[-300:]}")
    code = re.search(r"código: (\w+)", p.stderr)
    return (f"bloqueo retenido: db-info exit {p.returncode} tras {elapsed:.1f}s ({code.group(1) if code else '?'}), "
            f"0 tablas a medias; liberado: migra entera; backup convive con un escritor en WAL")


COMMANDS = {"dbinfo": dbinfo, "backup-restore": backup_restore, "restore-in-use": restore_in_use,
            "busy-timeout": busy_timeout}

if __name__ == "__main__":
    sys.stdout.reconfigure(encoding="utf-8")
    if not EXE.exists():
        print(f"FAIL no existe {EXE}")
        raise SystemExit(1)
    try:
        print("PASS " + COMMANDS[sys.argv[1]]())
        raise SystemExit(0)
    except (AssertionError, KeyError, IndexError, OSError, json.JSONDecodeError, sqlite3.Error,
            subprocess.TimeoutExpired) as e:
        print(f"FAIL {type(e).__name__}: {e}")
        raise SystemExit(1)
