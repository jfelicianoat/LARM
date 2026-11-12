"""Ledger privado de transformación: ownership físico, intentos, journal y outbox.

No usa la DB de Core ni concede reservas globales. Ningún offset se reanuda.
"""
import json
import msvcrt
import os
import sqlite3
import threading
import uuid
from contextlib import contextmanager

from larm_adapter_sdk.access import path_allowed
from larm_adapter_sdk.capabilities import canonical
from files import fail, read_json, stamp, write_new

TRANSITIONS = {
    "PLANNED":{"QUEUED"}, "QUEUED":{"RUNNING", "CANCELLING", "FAILED", "FAILED_RECOVERABLE"},
    "RUNNING":{"VERIFYING", "CANCELLING", "FAILED", "FAILED_RECOVERABLE"},
    "VERIFYING":{"COMPLETED", "CANCELLING", "FAILED", "FAILED_RECOVERABLE"},
    "CANCELLING":{"CANCELLED", "FAILED_RECOVERABLE"},
    "FAILED":{"QUEUED"}, "FAILED_RECOVERABLE":{"QUEUED"}, "COMPLETED":set(), "CANCELLED":set(),
}


class State:
    def __init__(self, root, contracts, profile_hash):
        self.root, self.roots, self.contracts = root, [root], contracts
        self.mutex = threading.RLock()
        lock_path = path_allowed(root / "owner.lock", self.roots, writing=True)
        try:
            self.lock_file = lock_path.open("x+b")
            self.lock_file.write(b"LARM_REFERENCE_TRANSFORM_1\n")
            self.lock_file.flush()
            os.fsync(self.lock_file.fileno())
        except FileExistsError:
            self.lock_file = lock_path.open("r+b")
        if os.fstat(self.lock_file.fileno()).st_nlink != 1:
            self.lock_file.close()
            raise fail("FORBIDDEN", "reference_transform_shared_write_denied")
        self.lock_file.seek(0)
        try:
            msvcrt.locking(self.lock_file.fileno(), msvcrt.LK_NBLCK, 1)
        except OSError:
            self.lock_file.close()
            raise fail("CONFLICT", "reference_transform_worker_locked") from None
        # Windows bloquea también lecturas de la región de otro propietario.
        # Comprobar el marcador sólo después de adquirir el lock físico.
        self.lock_file.seek(0)
        if self.lock_file.read(64) != b"LARM_REFERENCE_TRANSFORM_1\n":
            self.lock_file.close()
            raise fail("CONFLICT", "reference_transform_lock_not_owned")
        path = path_allowed(root / "transform.sqlite", self.roots, writing=True)
        existed = path.exists()
        if existed and path.stat().st_nlink != 1:
            self.lock_file.close()
            raise fail("FORBIDDEN", "reference_transform_shared_write_denied")
        path_allowed(path.with_name(path.name + "-journal"), self.roots, writing=True)
        self.db = sqlite3.connect(path, check_same_thread=False, isolation_level=None, timeout=2)
        try:
            if existed and (self.db.execute("PRAGMA application_id").fetchone()[0] != 0x4C415254 or self.db.execute("PRAGMA user_version").fetchone()[0] != 1):
                raise fail("CONFLICT", "reference_transform_database_not_owned")
            self.db.execute("PRAGMA application_id=1279349332")
            self.db.execute("PRAGMA user_version=1")
            self.db.execute("PRAGMA foreign_keys=ON")
            self.db.execute("PRAGMA journal_mode=DELETE")
            self.db.execute("PRAGMA synchronous=FULL")
            # Control de crecimiento real; el journal de rollback es adicional.
            if self.db.execute("PRAGMA page_size").fetchone()[0] != 4096:
                raise fail("CONFLICT", "reference_transform_database_layout_invalid")
            if self.db.execute("PRAGMA max_page_count=256").fetchone()[0] > 256:
                raise fail("RESOURCE_UNAVAILABLE", "reference_transform_database_budget_exceeded")
            self.db.executescript("""
              CREATE TABLE IF NOT EXISTS ref_profile(singleton INTEGER PRIMARY KEY CHECK(singleton=1),hash TEXT NOT NULL);
              CREATE TABLE IF NOT EXISTS ref_plan(id TEXT PRIMARY KEY,hash TEXT NOT NULL,document TEXT NOT NULL,request TEXT NOT NULL,binding TEXT NOT NULL,job_id TEXT,outcome TEXT);
              CREATE TABLE IF NOT EXISTS ref_job(id TEXT PRIMARY KEY,identity TEXT NOT NULL,attempt INTEGER NOT NULL,phase TEXT NOT NULL,document TEXT NOT NULL,version INTEGER NOT NULL);
              CREATE TABLE IF NOT EXISTS ref_attempt(job_id TEXT NOT NULL REFERENCES ref_job(id),attempt INTEGER NOT NULL,phase TEXT NOT NULL,snapshot TEXT NOT NULL,PRIMARY KEY(job_id,attempt));
              CREATE TABLE IF NOT EXISTS ref_journal(job_id TEXT NOT NULL,attempt INTEGER NOT NULL,plan_id TEXT NOT NULL REFERENCES ref_plan(id),phase TEXT NOT NULL,payload TEXT NOT NULL,closed INTEGER NOT NULL DEFAULT 0,PRIMARY KEY(job_id,attempt));
              CREATE TABLE IF NOT EXISTS ref_event(id INTEGER PRIMARY KEY AUTOINCREMENT,job_id TEXT NOT NULL,version INTEGER NOT NULL,event_type TEXT NOT NULL,phase TEXT NOT NULL,occurred_at TEXT NOT NULL,payload TEXT NOT NULL,UNIQUE(job_id,version));
              CREATE TABLE IF NOT EXISTS ref_outbox(event_id INTEGER PRIMARY KEY REFERENCES ref_event(id),delivered_at TEXT);
              CREATE TABLE IF NOT EXISTS ref_reservation(job_id TEXT PRIMARY KEY,total_bytes INTEGER NOT NULL,remaining_bytes INTEGER NOT NULL);
              CREATE TABLE IF NOT EXISTS ref_lock(job_id TEXT PRIMARY KEY,generation INTEGER NOT NULL);
              CREATE TABLE IF NOT EXISTS ref_checkpoint(job_id TEXT NOT NULL,attempt INTEGER NOT NULL,offset INTEGER NOT NULL,PRIMARY KEY(job_id,attempt));
            """)
            row = self.db.execute("SELECT hash FROM ref_profile WHERE singleton=1").fetchone()
            if row is not None and row[0] != profile_hash:
                raise fail("CONFLICT", "reference_transform_profile_changed")
            with self.transaction():
                self.db.execute("INSERT OR IGNORE INTO ref_profile VALUES (1,?)", (profile_hash,))
                for job_id, in self.db.execute("SELECT id FROM ref_job WHERE phase IN ('QUEUED','RUNNING','VERIFYING','CANCELLING')").fetchall():
                    # Sólo después de adquirir el lock físico, nunca por edad.
                    job = self.job(job_id)
                    fault = self.root / f"fault-{job_id}-{job['progress']['attempt']}.json"
                    error = fail("ADAPTER_CRASHED", "reference_transform_interrupted").data
                    if path_allowed(fault,self.roots,writing=True).exists():
                        saved = read_json(fault,self.roots,16 << 10)
                        if set(saved) != {"job_id","attempt","error"} or saved["job_id"] != job_id or saved["attempt"] != job["progress"]["attempt"]:
                            raise fail("CONFLICT", "reference_transform_fault_record_invalid")
                        self.contracts.validate("larm.schema.json#/$defs/Error",saved["error"])
                        error = saved["error"]
                    self._transition(job_id, "FAILED_RECOVERABLE", error=error)
        except BaseException:
            self.close()
            raise

    @contextmanager
    def transaction(self):
        with self.mutex:
            self.db.execute("BEGIN IMMEDIATE")
            try:
                yield
                self.db.execute("COMMIT")
            except BaseException:
                if self.db.in_transaction:
                    self.db.execute("ROLLBACK")
                raise

    def plan(self, value, request, binding):
        with self.transaction():
            self.db.execute("INSERT INTO ref_plan VALUES (?,?,?,?,?,NULL,NULL)", (value["plan_id"], value["plan_hash"], json.dumps(value), json.dumps(request), json.dumps(binding)))

    def get_plan(self, identity):
        with self.mutex:
            row = self.db.execute("SELECT hash,document,request,binding,job_id,outcome FROM ref_plan WHERE id=?", (identity,)).fetchone()
        if row is None:
            raise fail("NOT_FOUND", "reference_transform_plan_not_found")
        return {"hash":row[0], "document":json.loads(row[1]), "request":json.loads(row[2]), "binding":json.loads(row[3]), "job_id":row[4], "outcome":json.loads(row[5]) if row[5] else None}

    def job(self, identity):
        with self.mutex:
            row = self.db.execute("SELECT document FROM ref_job WHERE id=?", (identity,)).fetchone()
        if row is None:
            raise fail("NOT_FOUND", "reference_transform_job_not_found")
        return json.loads(row[0])

    def journal(self, identity):
        with self.mutex:
            row = self.db.execute("SELECT payload FROM ref_journal WHERE job_id=? AND attempt=(SELECT attempt FROM ref_job WHERE id=?)", (identity,identity)).fetchone()
        if row is None:
            raise fail("CONFLICT", "reference_transform_journal_missing")
        return json.loads(row[0])

    def accept(self, plan_id, identity, reserve):
        with self.transaction():
            if self.db.execute("SELECT job_id FROM ref_plan WHERE id=?", (plan_id,)).fetchone()[0] is not None:
                raise fail("CONFLICT", "reference_transform_plan_already_used")
            if self.db.execute("SELECT 1 FROM ref_job WHERE identity=? AND phase IN ('QUEUED','RUNNING','VERIFYING','CANCELLING')", (identity,)).fetchone():
                raise fail("CONFLICT", "reference_transform_job_active")
            prior = self.db.execute("SELECT id,attempt FROM ref_job WHERE identity=? AND phase IN ('FAILED','FAILED_RECOVERABLE') ORDER BY rowid DESC LIMIT 1", (identity,)).fetchone()
            job_id, attempt = (prior[0],prior[1]+1) if prior else (str(uuid.uuid7()),1)
            plan = self.get_plan(plan_id)
            if prior:
                self.db.execute("UPDATE ref_journal SET closed=1 WHERE job_id=? AND attempt=?", prior)
                job = self.job(job_id)
                job.update(progress={"attempt":attempt, "completed_bytes":"0"}, result=None, error=None)
                self.db.execute("UPDATE ref_job SET attempt=?,document=? WHERE id=?", (attempt,json.dumps(job),job_id))
            else:
                job = {"job_id":job_id, "state":"PLANNED", "resource_type":"reference_transformation", "resource_id":plan["request"]["input_variant_id"], "progress":{"attempt":attempt, "completed_bytes":"0"}, "result":None, "error":None, "evidence_refs":[]}
                self.db.execute("INSERT INTO ref_job VALUES (?,?,?,'PLANNED',?,0)", (job_id,identity,attempt,json.dumps(job)))
            paths = {"staging":f"staging/{job_id}-{attempt}", "output":f"{job_id}-{attempt}", "scope":"reference_private_only", "input":plan["binding"]["input"], "predicted":plan["binding"]["predicted"]}
            self.db.execute("INSERT INTO ref_journal VALUES (?,?,?,'QUEUED',?,0)", (job_id,attempt,plan_id,json.dumps(paths)))
            self.db.execute("UPDATE ref_plan SET job_id=? WHERE id=?", (job_id,plan_id))
            self.db.execute("INSERT OR REPLACE INTO ref_reservation VALUES (?,?,?)", (job_id,reserve,reserve))
            self.db.execute("INSERT OR REPLACE INTO ref_lock VALUES (?,?)", (job_id,attempt))
            self.db.execute("INSERT INTO ref_checkpoint VALUES (?,?,0)", (job_id,attempt))
            self._transition(job_id,"QUEUED")
        return job_id

    def _transition(self, identity, phase, *, result=None, error=None):
        old = self.db.execute("SELECT phase,attempt FROM ref_job WHERE id=?", (identity,)).fetchone()
        if phase not in TRANSITIONS[old[0]]:
            raise fail("INVALID_STATE_TRANSITION", "reference_transform_transition_invalid")
        job = self.job(identity)
        job.update(state=phase, result=result, error=error)
        job["progress"]["phase"] = phase
        self.contracts.validate("larm.schema.json#/$defs/JobState", job)
        raw = canonical(job).decode()
        self.db.execute("UPDATE ref_job SET phase=?,document=?,version=version+1 WHERE id=?", (phase,raw,identity))
        version = self.db.execute("SELECT version FROM ref_job WHERE id=?", (identity,)).fetchone()[0]
        self.db.execute("INSERT INTO ref_attempt VALUES (?,?,?,?) ON CONFLICT(job_id,attempt) DO UPDATE SET phase=excluded.phase,snapshot=excluded.snapshot", (identity,old[1],phase,raw))
        event = self.db.execute("INSERT INTO ref_event(job_id,version,event_type,phase,occurred_at,payload) VALUES (?,?,'transformation.state_changed',?,?,?)", (identity,version,phase,stamp(),raw)).lastrowid
        self.db.execute("INSERT INTO ref_outbox(event_id) VALUES (?)", (event,))
        self.db.execute("UPDATE ref_journal SET phase=? WHERE job_id=? AND attempt=?", (phase,identity,old[1]))
        if phase in ("COMPLETED","CANCELLED","FAILED","FAILED_RECOVERABLE"):
            outcome = {"completed_job_id":identity} if phase=="COMPLETED" else {"error":error}
            self.db.execute("UPDATE ref_plan SET outcome=? WHERE id=(SELECT plan_id FROM ref_journal WHERE job_id=? AND attempt=?)", (json.dumps(outcome),identity,old[1]))
        if phase in ("COMPLETED","CANCELLED"):
            self.db.execute("DELETE FROM ref_lock WHERE job_id=?", (identity,))
            self.db.execute("DELETE FROM ref_reservation WHERE job_id=?", (identity,))
            self.db.execute("UPDATE ref_journal SET closed=1 WHERE job_id=? AND attempt=?", (identity,old[1]))

    def transition(self, identity, phase, **kwargs):
        try:
            with self.transaction():
                self._transition(identity,phase,**kwargs)
        except sqlite3.Error as error:
            # Si falla registrar un fallo, conservar la causa anterior, no sólo
            # el error secundario de outbox. El commit incierto no libera nada.
            cause=kwargs.get("error") or fail("RESOURCE_UNAVAILABLE","reference_transform_persistence_failed",original=error).data
            self.remember_fault(identity,cause)
            raise

    def checkpoint(self, identity, offset):
        with self.transaction():
            job = self.job(identity)
            attempt = job["progress"]["attempt"]
            self.db.execute("UPDATE ref_checkpoint SET offset=? WHERE job_id=? AND attempt=?", (offset,identity,attempt))
            self.db.execute("UPDATE ref_reservation SET remaining_bytes=MAX(0,total_bytes-?) WHERE job_id=?", (offset,identity))
            job["progress"]["completed_bytes"] = str(offset)
            self.db.execute("UPDATE ref_job SET document=? WHERE id=?", (json.dumps(job),identity))

    def remember_fault(self, identity, error):
        job = self.job(identity)
        attempt = job["progress"]["attempt"]
        target = path_allowed(self.root / f"fault-{identity}-{attempt}.json",self.roots,writing=True)
        if not target.exists():
            # Una muerte durante la escritura sólo deja un .tmp sin confirmar;
            # el recovery nunca confunde ese prefijo con un registro completo.
            temporary=target.with_suffix(".tmp")
            write_new(temporary,{"job_id":identity,"attempt":attempt,"error":error},self.roots)
            os.rename(temporary,target)

    def receipt(self, identity):
        job = self.job(identity)
        if job["state"] != "COMPLETED":
            raise fail("INVALID_STATE_TRANSITION", "reference_transform_job_not_completed")
        folder = path_allowed(self.root / "job-status",self.roots,writing=True)
        folder.mkdir(exist_ok=True)
        target = path_allowed(folder / f"{identity}.json",self.roots,writing=True)
        if target.exists():
            if target.read_bytes() != canonical(job):
                raise fail("INTEGRITY_FAILED", "reference_transform_status_changed")
        else:
            # Une las observaciones de DB y fichero sin una URL /api/jobs inventada.
            temporary = folder / f"{identity}.tmp"
            if temporary.exists():
                path_allowed(temporary,self.roots).unlink()
            write_new(temporary,job,self.roots)
            os.rename(temporary,target)
        return {"job_id":identity, "state":"COMPLETED", "status_url":target.as_uri()}

    def close(self):
        self.db.close()
        self.lock_file.seek(0)
        msvcrt.locking(self.lock_file.fileno(),msvcrt.LK_UNLCK,1)
        self.lock_file.close()
