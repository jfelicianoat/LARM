"""Ledger privado del worker: planes, jobs, journal/outbox y reservas atómicos.

No es la base administrativa del Core ni concede elegibilidad P050.
"""
import json
import msvcrt
import os
import sqlite3
import threading
import uuid
from contextlib import contextmanager
from pathlib import Path

from catalog import stamp
from transfer import fail, path_allowed


class State:
    def __init__(self, root, roots, contracts, profile_hash):
        self.root, self.roots, self.contracts = path_allowed(root, roots), roots, contracts
        self.mutex = threading.RLock()
        lock_path = self.root / "reference-source.lock"
        path_allowed(lock_path, roots, writing=True)
        try:
            self.lock_file = lock_path.open("x+b")
            self.lock_file.write(b"LARM_REFERENCE_SOURCE_1\n")
            self.lock_file.flush()
        except FileExistsError:
            self.lock_file = lock_path.open("r+b")
        self.lock_file.seek(0)
        try:
            msvcrt.locking(self.lock_file.fileno(), msvcrt.LK_NBLCK, 1)
        except OSError:
            self.lock_file.close()
            raise fail("CONFLICT", "reference_worker_locked") from None
        # No leer bytes protegidos por el lock de otro worker de Windows.
        self.lock_file.seek(0)
        if self.lock_file.read(64) != b"LARM_REFERENCE_SOURCE_1\n":
            self.lock_file.close()
            raise fail("CONFLICT", "reference_lock_file_conflict")
        db_path = path_allowed(self.root / "reference-source.sqlite", roots, writing=True)
        existed = db_path.exists()
        self.db = sqlite3.connect(db_path, check_same_thread=False, isolation_level=None, timeout=10)
        if existed and self.db.execute("PRAGMA application_id").fetchone()[0] != 0x4C415253:
            self.db.close()
            self.lock_file.close()
            raise fail("CONFLICT", "reference_database_not_owned")
        self.db.execute("PRAGMA application_id=1279349331")
        self.db.execute("PRAGMA journal_mode=DELETE")
        self.db.execute("PRAGMA synchronous=FULL")
        self.db.executescript("""
          CREATE TABLE IF NOT EXISTS ref_plan(id TEXT PRIMARY KEY,hash TEXT NOT NULL,request TEXT NOT NULL,document TEXT NOT NULL,binding TEXT NOT NULL,job_id TEXT,outcome TEXT);
          CREATE TABLE IF NOT EXISTS ref_job(id TEXT PRIMARY KEY,identity TEXT NOT NULL,attempt INTEGER NOT NULL,phase TEXT NOT NULL,document TEXT NOT NULL,version INTEGER NOT NULL);
          CREATE TABLE IF NOT EXISTS ref_journal(job_id TEXT PRIMARY KEY,plan_id TEXT NOT NULL,phase TEXT NOT NULL,payload TEXT NOT NULL);
          CREATE TABLE IF NOT EXISTS ref_event(id INTEGER PRIMARY KEY AUTOINCREMENT,job_id TEXT NOT NULL,version INTEGER NOT NULL,phase TEXT NOT NULL,occurred_at TEXT NOT NULL,UNIQUE(job_id,version));
          CREATE TABLE IF NOT EXISTS ref_outbox(event_id INTEGER PRIMARY KEY REFERENCES ref_event(id),delivered_at TEXT);
          CREATE TABLE IF NOT EXISTS ref_reservation(job_id TEXT PRIMARY KEY,total_bytes INTEGER NOT NULL,remaining_bytes INTEGER NOT NULL);
          CREATE TABLE IF NOT EXISTS ref_lock(job_id TEXT PRIMARY KEY,attempt INTEGER NOT NULL);
          CREATE TABLE IF NOT EXISTS ref_checkpoint(job_id TEXT NOT NULL,path TEXT NOT NULL,offset INTEGER NOT NULL,PRIMARY KEY(job_id,path));
          CREATE TABLE IF NOT EXISTS ref_profile(singleton INTEGER PRIMARY KEY CHECK(singleton=1),hash TEXT NOT NULL);
        """)
        profile = self.db.execute("SELECT hash FROM ref_profile WHERE singleton=1").fetchone()
        if profile is not None and profile[0] != profile_hash:
            self.close()
            raise fail("CONFLICT", "reference_profile_changed")
        with self.transaction():
            self.db.execute("INSERT OR IGNORE INTO ref_profile VALUES (1,?)", (profile_hash,))
            rows = self.db.execute("SELECT id,document FROM ref_job WHERE phase IN ('RUNNING','VERIFYING','PUBLISHING')").fetchall()
            for identity, raw in rows:
                job = json.loads(raw)
                error = fail("ADAPTER_CRASHED", "reference_worker_interrupted").data
                job["state"], job["error"] = "FAILED", error
                self._transition(identity, "FAILED", job)

    @contextmanager
    def transaction(self):
        with self.mutex:
            self.db.execute("BEGIN IMMEDIATE")
            try:
                yield
                self.db.execute("COMMIT")
            except BaseException:
                self.db.execute("ROLLBACK")
                raise

    def plan(self, value, request, binding):
        with self.transaction():
            self.db.execute("INSERT INTO ref_plan VALUES (?,?,?,?,?,NULL,NULL)", (value["plan_id"], value["plan_hash"], json.dumps(request), json.dumps(value), json.dumps(binding)))

    def get_plan(self, identity):
        with self.mutex:
            row = self.db.execute("SELECT hash,request,document,binding,job_id,outcome FROM ref_plan WHERE id=?", (identity,)).fetchone()
        if row is None:
            raise fail("NOT_FOUND", "reference_plan_not_found")
        return {"hash": row[0], "request": json.loads(row[1]), "document": json.loads(row[2]), "binding": json.loads(row[3]), "job_id": row[4], "outcome": json.loads(row[5]) if row[5] else None}

    def job(self, identity):
        with self.mutex:
            row = self.db.execute("SELECT document FROM ref_job WHERE id=?", (identity,)).fetchone()
        if row is None:
            raise fail("NOT_FOUND", "reference_job_not_found")
        return json.loads(row[0])

    def accept(self, plan_id, identity, size):
        with self.transaction():
            plan = self.db.execute("SELECT job_id FROM ref_plan WHERE id=?", (plan_id,)).fetchone()
            if plan[0] is not None:
                raise fail("CONFLICT", "reference_plan_already_used")
            if self.db.execute("SELECT 1 FROM ref_job WHERE identity=? AND phase IN ('RUNNING','VERIFYING','PUBLISHING')", (identity,)).fetchone():
                raise fail("CONFLICT", "reference_job_active")
            previous = self.db.execute("SELECT id,attempt FROM ref_job WHERE identity=? AND phase='FAILED' ORDER BY rowid DESC LIMIT 1", (identity,)).fetchone()
            job_id, attempt = (previous[0], previous[1] + 1) if previous else (str(uuid.uuid7()), 1)
            job = {"job_id": job_id, "state": "RUNNING", "resource_type": "model_variant", "resource_id": json.loads(self.db.execute("SELECT request FROM ref_plan WHERE id=?", (plan_id,)).fetchone()[0])["variant_id"], "progress": {"attempt": attempt, "completed_bytes": "0", "total_bytes": str(size)}, "result": None, "error": None, "evidence_refs": []}
            if previous:
                self.db.execute("UPDATE ref_job SET attempt=? WHERE id=?", (attempt, job_id))
            else:
                self.db.execute("INSERT INTO ref_job VALUES (?,?,?,'RUNNING',?,0)", (job_id, identity, attempt, json.dumps(job)))
            self.db.execute("UPDATE ref_plan SET job_id=? WHERE id=?", (job_id, plan_id))
            self.db.execute("INSERT OR REPLACE INTO ref_reservation VALUES (?,?,?)", (job_id, size, size))
            self.db.execute("INSERT OR REPLACE INTO ref_lock VALUES (?,?)", (job_id, attempt))
            self.db.execute("INSERT OR REPLACE INTO ref_journal VALUES (?,?,'RUNNING',?)", (job_id, plan_id, json.dumps({"scope": "reference_private_only", "attempt": attempt})))
            self._transition(job_id, "RUNNING", job)
        return job_id

    def _transition(self, identity, phase, job):
        self.contracts.validate("larm.schema.json#/$defs/JobState", job)
        self.db.execute("UPDATE ref_job SET phase=?,document=?,version=version+1 WHERE id=?", (phase, json.dumps(job), identity))
        version = self.db.execute("SELECT version FROM ref_job WHERE id=?", (identity,)).fetchone()[0]
        cursor = self.db.execute("INSERT INTO ref_event(job_id,version,phase,occurred_at) VALUES (?,?,?,?)", (identity, version, phase, stamp()))
        self.db.execute("INSERT INTO ref_outbox(event_id) VALUES (?)", (cursor.lastrowid,))
        self.db.execute("UPDATE ref_journal SET phase=? WHERE job_id=?", (phase, identity))
        if phase in ("FAILED", "CANCELLED", "COMPLETED"):
            outcome = {"completed_job_id": identity} if phase == "COMPLETED" else {"error": job["error"]}
            self.db.execute("UPDATE ref_plan SET outcome=? WHERE id=(SELECT plan_id FROM ref_journal WHERE job_id=?)", (json.dumps(outcome), identity))
            self.db.execute("DELETE FROM ref_lock WHERE job_id=?", (identity,))
            self.db.execute("DELETE FROM ref_reservation WHERE job_id=?", (identity,))

    def transition(self, identity, phase, *, result=None, error=None):
        with self.transaction():
            job = self.job(identity)
            job["state"] = "COMPLETED" if phase == "COMPLETED" else "FAILED" if phase in ("FAILED", "CANCELLED") else "RUNNING"
            job["result"], job["error"] = result, error
            job["progress"]["phase"] = phase
            self._transition(identity, phase, job)

    def checkpoint(self, identity, path, offset, total_done):
        with self.transaction():
            self.db.execute("INSERT OR REPLACE INTO ref_checkpoint VALUES (?,?,?)", (identity, path, offset))
            self.db.execute("UPDATE ref_reservation SET remaining_bytes=MAX(0,total_bytes-?) WHERE job_id=?", (total_done, identity))
            job = self.job(identity)
            job["progress"]["completed_bytes"] = str(total_done)
            self.db.execute("UPDATE ref_job SET document=? WHERE id=?", (json.dumps(job), identity))

    def receipt(self, identity):
        job = self.job(identity)
        if job["state"] != "COMPLETED":
            raise fail("CONFLICT", "reference_job_not_completed")
        folder = self.root / "job-status"
        path_allowed(folder, self.roots, writing=True)
        folder.mkdir(exist_ok=True)
        path = path_allowed(folder / f"{identity}.json", self.roots, writing=True)
        temporary = path.with_suffix(".tmp")
        path_allowed(temporary, self.roots, writing=True)
        with temporary.open("wb") as output:
            output.write(json.dumps(job, ensure_ascii=False, allow_nan=False).encode())
            output.flush()
            os.fsync(output.fileno())
        os.replace(temporary, path)
        # URL realmente legible del JobState del worker, nunca un /api/jobs ficticio.
        return {"job_id": identity, "state": "COMPLETED", "status_url": path.as_uri()}

    def close(self):
        self.db.close()
        self.lock_file.seek(0)
        msvcrt.locking(self.lock_file.fileno(), msvcrt.LK_UNLCK, 1)
        self.lock_file.close()
