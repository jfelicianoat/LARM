"""Mediciones privadas durables; no inventa JobReceipt ni reserva de Core."""
import json
import hashlib
import msvcrt
import os
import shutil
import sqlite3
import threading
from contextlib import contextmanager
from datetime import datetime, timezone

from larm_adapter_sdk.access import path_allowed
from larm_adapter_sdk.capabilities import canonical, digest
from transport import failure, unique, nonfinite

RESERVE=3<<20
MAX_DOCUMENT=128<<10


def stamp(): return datetime.now(timezone.utc).isoformat(timespec="milliseconds").replace("+00:00","Z")


def read_json(path,roots,*,with_hash=False):
    with path_allowed(path,roots).open("rb") as source: raw=source.read(MAX_DOCUMENT+1)
    if len(raw)>MAX_DOCUMENT: raise failure("RESOURCE_UNAVAILABLE","reference_benchmark_document_budget_exceeded")
    value=json.loads(raw.decode("utf-8"),object_pairs_hook=unique,parse_constant=nonfinite)
    return (value,hashlib.sha256(raw).hexdigest()) if with_hash else value


def write_new(path,value,roots):
    raw=canonical(value)
    if len(raw)>MAX_DOCUMENT: raise failure("RESOURCE_UNAVAILABLE","reference_benchmark_document_budget_exceeded")
    with path_allowed(path,roots,writing=True).open("xb") as output:
        path_allowed(path,roots)
        if os.fstat(output.fileno()).st_nlink!=1: raise failure("FORBIDDEN","reference_benchmark_shared_write_denied")
        output.write(raw); output.flush(); os.fsync(output.fileno())
    return hashlib.sha256(raw).hexdigest()


class Ledger:
    def __init__(self,root,output,profile_hash,contracts):
        self.root,self.output,self.roots=root,output,[root,output]
        self.contracts=contracts
        self.mutex=threading.RLock()
        lock=path_allowed(root/"owner.lock",self.roots,writing=True)
        try:
            self.owner=lock.open("x+b"); self.owner.write(b"LARM_REFERENCE_BENCHMARK_1\n"); self.owner.flush(); os.fsync(self.owner.fileno())
        except FileExistsError:
            self.owner=lock.open("r+b")
        if os.fstat(self.owner.fileno()).st_nlink!=1:
            self.owner.close(); raise failure("FORBIDDEN","reference_benchmark_shared_write_denied")
        self.owner.seek(0)
        try: msvcrt.locking(self.owner.fileno(),msvcrt.LK_NBLCK,1)
        except OSError:
            self.owner.close(); raise failure("CONFLICT","reference_benchmark_worker_locked") from None
        # Leer una región bloqueada por otro proceso falla en Windows.
        self.owner.seek(0)
        if self.owner.read(64)!=b"LARM_REFERENCE_BENCHMARK_1\n":
            self.owner.close(); raise failure("CONFLICT","reference_benchmark_lock_not_owned")
        db_path=path_allowed(root/"benchmark.sqlite",self.roots,writing=True)
        existed=db_path.exists()
        if existed and db_path.stat().st_nlink!=1:
            self.owner.close(); raise failure("FORBIDDEN","reference_benchmark_shared_write_denied")
        path_allowed(root/"benchmark.sqlite-journal",self.roots,writing=True)
        self.db=sqlite3.connect(db_path,check_same_thread=False,isolation_level=None,timeout=2)
        try:
            if existed and (self.db.execute("PRAGMA application_id").fetchone()[0]!=0x4C415242 or self.db.execute("PRAGMA user_version").fetchone()[0]!=1):
                raise failure("CONFLICT","reference_benchmark_database_not_owned")
            self.db.execute("PRAGMA application_id=1279349314"); self.db.execute("PRAGMA user_version=1")
            self.db.execute("PRAGMA foreign_keys=ON"); self.db.execute("PRAGMA journal_mode=DELETE"); self.db.execute("PRAGMA synchronous=FULL"); self.db.execute("PRAGMA temp_store=MEMORY")
            if self.db.execute("PRAGMA page_size").fetchone()[0]!=4096 or self.db.execute("PRAGMA max_page_count=256").fetchone()[0]>256:
                raise failure("RESOURCE_UNAVAILABLE","reference_benchmark_database_budget_exceeded")
            self.db.executescript("""
              CREATE TABLE IF NOT EXISTS ref_profile(singleton INTEGER PRIMARY KEY CHECK(singleton=1),hash TEXT NOT NULL);
              CREATE TABLE IF NOT EXISTS ref_run(id TEXT PRIMARY KEY,phase TEXT NOT NULL,request TEXT NOT NULL,report TEXT,report_hash TEXT,error TEXT,version INTEGER NOT NULL);
              CREATE TABLE IF NOT EXISTS ref_sample(run_id TEXT NOT NULL REFERENCES ref_run(id),iteration INTEGER NOT NULL,sample TEXT NOT NULL,observation TEXT NOT NULL,PRIMARY KEY(run_id,iteration));
              CREATE TABLE IF NOT EXISTS ref_journal(run_id TEXT PRIMARY KEY REFERENCES ref_run(id),phase TEXT NOT NULL,path TEXT NOT NULL);
              CREATE TABLE IF NOT EXISTS ref_event(id INTEGER PRIMARY KEY AUTOINCREMENT,run_id TEXT NOT NULL,version INTEGER NOT NULL,phase TEXT NOT NULL,occurred_at TEXT NOT NULL,UNIQUE(run_id,version));
              CREATE TABLE IF NOT EXISTS ref_outbox(event_id INTEGER PRIMARY KEY REFERENCES ref_event(id),delivered_at TEXT);
              CREATE TABLE IF NOT EXISTS ref_lock(run_id TEXT PRIMARY KEY);
              CREATE TABLE IF NOT EXISTS ref_reservation(run_id TEXT PRIMARY KEY,total_bytes INTEGER NOT NULL,remaining_bytes INTEGER NOT NULL);
            """)
            profile=self.db.execute("SELECT hash FROM ref_profile WHERE singleton=1").fetchone()
            if profile is not None and profile[0]!=profile_hash: raise failure("CONFLICT","reference_benchmark_profile_changed")
            with self.transaction():
                self.db.execute("INSERT OR IGNORE INTO ref_profile VALUES (1,?)",(profile_hash,))
                # Lock físico adquirido: ningún escritor antiguo sigue vivo.
                # No se reanuda una medición; se conservan muestras/journal y un
                # nuevo run debe obtener preflight/reserva propios.
                for identity, in self.db.execute("SELECT id FROM ref_run WHERE phase IN ('RUNNING','VERIFYING')").fetchall():
                    cause=failure("ADAPTER_CRASHED","reference_benchmark_interrupted").data
                    fault=path_allowed(root/f"fault-{identity}.json",self.roots,writing=True)
                    if fault.exists():
                        saved=read_json(fault,self.roots)
                        if set(saved)!={"run_id","error"} or saved["run_id"]!=identity: raise failure("CONFLICT","reference_benchmark_fault_record_invalid")
                        self.contracts.validate("larm.schema.json#/$defs/Error",saved["error"])
                        cause=saved["error"]
                    self._transition(identity,"FAILED",error=cause)
        except BaseException:
            self.close(); raise

    @contextmanager
    def transaction(self):
        with self.mutex:
            self.db.execute("BEGIN IMMEDIATE")
            try:
                yield; self.db.execute("COMMIT")
            except BaseException:
                if self.db.in_transaction: self.db.execute("ROLLBACK")
                raise

    def preflight(self,*,new_run=True):
        if shutil.disk_usage(self.root).free<RESERVE+(1<<30): raise failure("STORAGE_INSUFFICIENT","reference_benchmark_free_space_insufficient")
        if new_run:
            if self.db.execute("SELECT 1 FROM ref_run WHERE phase IN ('RUNNING','VERIFYING')").fetchone(): raise failure("CONFLICT","reference_benchmark_recovery_required")
            if self.db.execute("SELECT count(*) FROM ref_run").fetchone()[0]>=32: raise failure("RESOURCE_UNAVAILABLE","reference_benchmark_history_full")

    def accept(self,identity,request):
        self.preflight()
        with self.transaction():
            self.db.execute("INSERT INTO ref_run VALUES (?,'RUNNING',?,NULL,NULL,NULL,0)",(identity,json.dumps(request)))
            self.db.execute("INSERT INTO ref_journal VALUES (?,'RUNNING',?)",(identity,identity))
            self.db.execute("INSERT INTO ref_lock VALUES (?)",(identity,))
            self.db.execute("INSERT INTO ref_reservation VALUES (?,?,?)",(identity,RESERVE,RESERVE))
            self._transition(identity,"RUNNING")

    def _transition(self,identity,phase,*,report=None,error=None):
        old=self.db.execute("SELECT phase FROM ref_run WHERE id=?",(identity,)).fetchone()[0]
        allowed={"RUNNING":{"RUNNING","VERIFYING","FAILED","CANCELLED"},"VERIFYING":{"COMPLETED","FAILED","CANCELLED"}}
        if phase not in allowed.get(old,set()): raise failure("INVALID_STATE_TRANSITION","reference_benchmark_transition_invalid")
        self.db.execute("UPDATE ref_run SET phase=?,report=?,report_hash=?,error=?,version=version+1 WHERE id=?",(phase,json.dumps(report) if report is not None else None,digest(report) if report is not None else None,json.dumps(error) if error is not None else None,identity))
        version=self.db.execute("SELECT version FROM ref_run WHERE id=?",(identity,)).fetchone()[0]
        event=self.db.execute("INSERT INTO ref_event(run_id,version,phase,occurred_at) VALUES (?,?,?,?)",(identity,version,phase,stamp())).lastrowid
        self.db.execute("INSERT INTO ref_outbox(event_id) VALUES (?)",(event,))
        self.db.execute("UPDATE ref_journal SET phase=? WHERE run_id=?",(phase,identity))
        if phase in ("COMPLETED","FAILED","CANCELLED"):
            self.db.execute("DELETE FROM ref_lock WHERE run_id=?",(identity,)); self.db.execute("DELETE FROM ref_reservation WHERE run_id=?",(identity,))

    def transition(self,identity,phase,**kwargs):
        try:
            with self.transaction(): self._transition(identity,phase,**kwargs)
        except sqlite3.Error as error:
            self.remember_fault(identity,kwargs.get("error") or self.persistence_error(error))
            raise

    @staticmethod
    def persistence_error(error):
        result=failure("RESOURCE_UNAVAILABLE","reference_benchmark_persistence_failed").data
        result["details"]["original_error"]={"system":"benchmark","code":type(error).__name__,"message":"owned_reference_operation_failed"}
        return result

    def remember_fault(self,identity,error):
        target=path_allowed(self.root/f"fault-{identity}.json",self.roots,writing=True)
        if not target.exists():
            temporary=target.with_suffix(".tmp")
            write_new(temporary,{"run_id":identity,"error":error},self.roots)
            os.rename(temporary,target)

    def sample(self,identity,sample,observation):
        with self.transaction():
            self.db.execute("INSERT INTO ref_sample VALUES (?,?,?,?)",(identity,sample["iteration"],json.dumps(sample),json.dumps(observation)))

    def samples(self,identity):
        with self.mutex: rows=self.db.execute("SELECT sample,observation FROM ref_sample WHERE run_id=? ORDER BY iteration",(identity,)).fetchall()
        return [(json.loads(a),json.loads(b)) for a,b in rows]

    def recorded(self,identity):
        with self.mutex:
            row=self.db.execute("SELECT r.phase,r.request,r.report,r.report_hash FROM ref_run r JOIN ref_journal j ON j.run_id=r.id AND j.phase=r.phase AND j.path=r.id JOIN ref_event e ON e.run_id=r.id AND e.version=r.version AND e.phase=r.phase JOIN ref_outbox o ON o.event_id=e.id WHERE r.id=?",(identity,)).fetchone()
        if row is None or row[0]!="COMPLETED": return None
        return {"request":json.loads(row[1]),"report":json.loads(row[2]),"hash":row[3]}

    def close(self):
        self.db.close(); self.owner.seek(0); msvcrt.locking(self.owner.fileno(),msvcrt.LK_UNLCK,1); self.owner.close()
