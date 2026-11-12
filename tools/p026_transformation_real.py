#!/usr/bin/env python3
"""Worker transformation real: archivos, SQLite, cancel, crash y outbox incierto."""
import base64
import hashlib
import importlib.util
import json
import os
import platform
import sqlite3
import subprocess
import sys
import time
import urllib.parse
import uuid
from datetime import datetime, timezone
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
sys.path.insert(0,str(REPO / "tools/adapter-conformance"))
from runner import common, executable  # noqa: E402
from session import Session  # noqa: E402
from larm_adapter_sdk.capabilities import digest  # noqa: E402


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main():
    run = REPO / "docs/reports/P026_runs/transformation-real" / uuid.uuid4().hex[:12]
    run.mkdir(parents=True)
    spec = importlib.util.spec_from_file_location("transformation_factory",REPO / "adapters/transformation/reference/package.py")
    factory = importlib.util.module_from_spec(spec); spec.loader.exec_module(factory)
    data = bytes(range(256)) * (48 << 10) + b"LARM!"  # propio, 12 MiB+5, padding y bloques
    package,work = run/"package",run/"work"
    manifest,profile,_entry = factory.create(REPO,package,work,data)
    initial = {p.relative_to(package).as_posix():sha(p) for p in package.rglob("*") if p.is_file()}
    source = work / "input/fixture.bin"
    db_path = work / "state/transform.sqlite"
    steps,success,session = [],False,None

    def step(name,condition,detail):
        assert condition,name
        steps.append({"id":name,"result":"PASS","detail":detail}); print("PASS "+name,flush=True)
    def connect(handshake=True):
        current = Session(executable(package,manifest),manifest,cwd=work)
        if handshake: current.handshake()
        return current
    def query(sql,params=()):
        with sqlite3.connect(db_path,timeout=3) as db:
            return db.execute(sql,params).fetchall()
    def mutate(sql,params=()):
        with sqlite3.connect(db_path,timeout=3) as db: db.execute(sql,params)
    def request(**changes):
        return {"input_variant_id":profile["input_variant_id"],"adapter_id":manifest["id"],"tool_version":"1.0.0","target_format":"BASE64","parameters":{},"storage_location_id":profile["storage_location_id"],"hard_budget_bytes":None} | changes
    def plan(): return session.result("transformation.plan",request())
    def execute(value): return {"plan_id":value["plan_id"],"plan_hash":value["plan_hash"],"approval_id":None}
    def rejected(method,params,reason):
        reply = session.call(method,params)
        assert reply["error"]["data"]["details"]["reason"]==reason,(method,reply)
        return reply["error"]["data"]
    def wait_encoding(identity):
        end=time.monotonic()+15
        with session.condition:
            while not any(p["phase"]=="TRANSFORM_ENCODING" for p in session.progress.get(identity,[])):
                assert identity not in session.replies and not session.failure and not session.closed,"operación terminó antes del checkpoint requerido"
                remaining=end-time.monotonic(); assert remaining>0,"sin checkpoint durable"
                session.condition.wait(remaining)
    def active_job():
        row=query("SELECT id,document FROM ref_job WHERE phase='RUNNING' ORDER BY rowid DESC LIMIT 1")
        assert len(row)==1
        return row[0][0],json.loads(row[0][1])
    def held(job_id):
        return query("SELECT (SELECT count(*) FROM ref_lock WHERE job_id=?),(SELECT count(*) FROM ref_reservation WHERE job_id=?)",(job_id,job_id))[0]
    def output_observed(receipt):
        artifacts=session.result("transformation.verify",{"job_id":receipt["job_id"]})
        payload=next(a for a in artifacts["artifacts"] if a["required"])
        path=work/"output"/payload["relative_path"]
        encoded=path.read_bytes()
        assert base64.b64decode(encoded,validate=True)==data and encoded!=data
        assert sha(path)==payload["sha256"] and len(encoded)==int(payload["size_bytes"])
        return artifacts,path
    try:
        session=connect()
        description=session.result("transformation.describe",{})
        step("autonomous-four-methods",description["formats_input"]==["BINARY"] and description["formats_output"]==["BASE64"] and not description["pause_supported"] and len([m for m in manifest["methods"] if m.startswith("transformation.")])==4,"Python/SDK/contratos fijados; conversión binaria real, sin afirmar cuantización ni soporte de pausa")
        for changes,reason in (({"input_variant_id":str(uuid.uuid7())},"reference_transform_resource_not_bound"),({"storage_location_id":str(uuid.uuid7())},"reference_transform_resource_not_bound"),({"parameters":{"script":"forbidden"}},"reference_transform_options_not_supported"),({"target_format":"GGUF"},"reference_transform_options_not_supported"),({"tool_version":"2.0"},"reference_transform_options_not_supported"),({"hard_budget_bytes":"1"},"reference_transform_hard_budget_insufficient")):
            rejected("transformation.plan",request(**changes),reason)
        invalid=session.call("transformation.plan",request()|{"extra":True},validate=False)
        step("binding-schema-budget-before-write",invalid["error"]["code"]==-32602 and not query("SELECT id FROM ref_job") and not list((work/"output").iterdir()),"recursos ajenos, opciones arbitrarias, versión/formato no implementados, presupuesto y extra fields no admiten jobs")
        value=plan()
        binding=value["steps"][0]["request"]["binding"]
        expected_size=4*((len(data)+2)//3)
        step("pinned-dry-plan",binding["input"]["sha256"]==hashlib.sha256(data).hexdigest() and binding["predicted"]["size_bytes"]==str(expected_size) and not value["storage_estimate"]["per_volume"][0]["eligible"] and not list((work/"output").iterdir()),"preflight lee/hash real, predice salida y reserva privada conservadora; no concede elegibilidad global")
        rejected("transformation.execute",execute(value)|{"plan_hash":"b"*64},"reference_transform_plan_hash_mismatch")
        rejected("transformation.execute",execute(value)|{"approval_id":str(uuid.uuid7())},"reference_transform_approval_owned_by_core")
        source.write_bytes(b"!"+data[1:])
        rejected("transformation.execute",execute(value),"reference_transform_input_changed")
        source.write_bytes(data)
        step("input-and-plan-revalidated",not query("SELECT id FROM ref_job"),"input de igual tamaño pero SHA diferente y plan_hash ajeno no llegan a admisión ni alteran el original")
        expired=plan(); expired["expires_at"]="2000-01-01T00:00:00.000Z"; expired["plan_hash"]=digest({k:v for k,v in expired.items() if k!="plan_hash"})
        mutate("UPDATE ref_plan SET document=?,hash=? WHERE id=?",(json.dumps(expired),expired["plan_hash"],expired["plan_id"]))
        rejected("transformation.execute",execute(expired),"reference_transform_plan_expired")
        step("expired-plan-denied",not query("SELECT id FROM ref_job"),"fixture de plan expirado coherente se rechaza antes de reservar")
        mutate("CREATE TRIGGER fixture_accept_fault BEFORE INSERT ON ref_outbox BEGIN SELECT RAISE(ABORT,'controlled_accept_fault'); END")
        rejected("transformation.execute",execute(value),"reference_transform_persistence_failed")
        step("outbox-accept-atomic",not query("SELECT id FROM ref_job") and not query("SELECT * FROM ref_lock") and not query("SELECT * FROM ref_reservation") and query("SELECT job_id FROM ref_plan WHERE id=?",(value["plan_id"],))==[(None,)],"fallo SQL real revierte job, journal, evento, outbox, reserva, lock y consumo del plan juntos")
        mutate("DROP TRIGGER fixture_accept_fault")
        blocker=work/"state/staging"; blocker.write_bytes(b"owned-write-fault")
        failure=rejected("transformation.execute",execute(value),"reference_transform_io_failed")
        failed_id,failed_phase=query("SELECT id,phase FROM ref_job")[0]
        step("real-write-failure-preserves-state",failed_phase=="FAILED" and held(failed_id)==(1,1) and sha(source)==profile["input"]["sha256"] and failure["details"]["original_error"] is not None,"archivo propio bloquea crear staging; error redactado, journal y recursos conservados, sin salida parcial publicada")
        blocker.unlink()
        rejected("transformation.verify",{"job_id":failed_id},"reference_transform_job_not_completed")
        retry=plan(); receipt=session.result("transformation.execute",execute(retry))
        artifacts,payload=output_observed(receipt)
        step("actual-base64-with-provenance",receipt["job_id"]==failed_id and query("SELECT attempt,phase FROM ref_job WHERE id=?",(failed_id,))==[(2,"COMPLETED")] and sha(source)==profile["input"]["sha256"],"nuevo preflight reinicia intento desde cero, bytes distintos decodifican al input y original permanece intacto")
        provenance=json.loads((payload.parent/"provenance.json").read_text(encoding="utf-8"))
        step("provenance-and-manifest-observed",provenance["source"]["sha256"]==sha(source) and provenance["output"]["sha256"]==sha(payload) and provenance["tool"]["version"]=="1.0.0" and provenance["parameters"]=={} and provenance["attempt"]==2 and provenance["logs"]==["input_verified","base64_encoded","output_verified"],"herramienta/versión/parámetros/fecha, padre, output y logs seguros respaldados por bytes reales")
        phases=query("SELECT attempt,phase FROM ref_attempt WHERE job_id=? ORDER BY attempt",(failed_id,))
        event_count=query("SELECT count(*) FROM ref_event")[0][0]
        step("terminal-state-and-outbox-consistent",phases==[(1,"FAILED"),(2,"COMPLETED")] and held(failed_id)==(0,0) and query("SELECT count(*) FROM ref_event")==query("SELECT count(*) FROM ref_outbox"),"intento anterior conservado, transiciones y outbox atómicos; final libera sólo sus recursos")
        status=Path(urllib.parse.unquote(urllib.parse.urlsplit(receipt["status_url"]).path.lstrip("/")))
        status_job=json.loads(status.read_text(encoding="utf-8"))
        step("receipt-points-to-existing-job",status_job["job_id"]==failed_id and status_job["state"]=="COMPLETED" and status_job["result"]["manifest"]==artifacts,"JobReceipt file:// apunta al JobState privado existente; no hay /api/jobs de Core inventado")
        replay=session.result("transformation.execute",execute(retry))
        old_error=rejected("transformation.execute",execute(value),"reference_transform_io_failed")
        step("replay-and-old-failure-immutable",replay==receipt and old_error==failure and query("SELECT count(*) FROM ref_event")[0][0]==event_count,"mismo plan no ejecuta otra vez; plan fallido conserva su error aun tras completar otro intento")
        saved=payload.read_bytes(); payload.write_bytes(b"!"+saved[1:])
        rejected("transformation.verify",{"job_id":failed_id},"reference_transform_output_changed")
        payload.write_bytes(saved)
        step("verify-rereads-real-bytes",session.result("transformation.verify",{"job_id":failed_id})==artifacts,"verify detecta corrupción posterior y no valida sólo el registro; restauración explícita de la fixture vuelve a verificar")
        cancel_plan=plan(); active=session.send("transformation.execute",execute(cancel_plan)); wait_encoding(active)
        cancelled_id,_=active_job()
        session.result("core.cancel",{"request_id":active})
        cancelled=session.response(active)["error"]["data"]
        step("durable-cancel-cleans-only-owned-files",cancelled["details"]["reason"]=="operation_cancelled" and query("SELECT phase FROM ref_job WHERE id=?",(cancelled_id,))==[("CANCELLED",)] and held(cancelled_id)==(0,0) and not list((work/"state/staging"/f"{cancelled_id}-1").iterdir()) and payload.read_bytes()==saved,"cancelación después de checkpoint fsync pasa por CANCELLING/CANCELLED, limpia sólo tres paths del intento y preserva otras salidas")
        crash_plan=plan(); active=session.send("transformation.execute",execute(crash_plan)); wait_encoding(active)
        crashed_id,_=active_job(); session.crash(); session=None
        offset=query("SELECT offset FROM ref_checkpoint WHERE job_id=?",(crashed_id,))[0][0]
        partial=work/"state/staging"/f"{crashed_id}-1/artifact.b64"
        step("actual-worker-crash",offset>0 and partial.stat().st_size>=offset and held(crashed_id)==(1,1),"muerte sólo del proceso propio tras progreso durable; partial y reserva/lock siguen enumerados")
        incomplete_fault=work/"state"/f"fault-{crashed_id}-1.tmp"
        incomplete_fault.write_bytes(b'{"job_id":')  # fixture de metadata no confirmada
        session=connect()
        recovered=json.loads(query("SELECT document FROM ref_job WHERE id=?",(crashed_id,))[0][0])
        step("physical-ownership-recovery",recovered["state"]=="FAILED_RECOVERABLE" and recovered["error"]["code"]=="ADAPTER_CRASHED" and held(crashed_id)==(1,1) and partial.exists() and incomplete_fault.read_bytes()==b'{"job_id":',"nuevo worker toma lock físico antes de reconciliar; conserva input/plan/hashes/temporales, ignora metadata .tmp no confirmada y no reanuda offsets")
        hello=json.dumps({"jsonrpc":"2.0","id":str(uuid.uuid7()),"method":"core.handshake","params":{"protocol_version":"1.0","adapter_id":manifest["id"],"nonce":uuid.uuid4().hex}}).encode()+b"\n"
        contender=subprocess.run(executable(package,manifest),cwd=work,input=hello,capture_output=True,timeout=15)
        denied=json.loads(contender.stdout)
        step("second-worker-denied",contender.returncode==0 and denied["error"]["data"]["details"]["reason"]=="reference_transform_worker_locked" and query("SELECT phase FROM ref_job WHERE id=?",(crashed_id,))==[("FAILED_RECOVERABLE",)],"worker competidor recibe error de ownership durante handshake válido; no recupera por tiempo ni negocia una sesión operativa")
        source.write_bytes(b"!"+data[1:]); rejected("transformation.plan",request(),"reference_transform_input_changed"); source.write_bytes(data)
        step("restart-needs-original-input",query("SELECT attempt,phase FROM ref_job WHERE id=?",(crashed_id,))==[(1,"FAILED_RECOVERABLE")] and held(crashed_id)==(1,1),"input modificado bloquea nuevo plan y no consume intento ni libera recursos pendientes")
        restarted=plan(); restarted_receipt=session.result("transformation.execute",execute(restarted)); output_observed(restarted_receipt)
        step("restart-new-attempt-not-resume",restarted_receipt["job_id"]==crashed_id and query("SELECT attempt,phase FROM ref_attempt WHERE job_id=? ORDER BY attempt",(crashed_id,))==[(1,"FAILED_RECOVERABLE"),(2,"COMPLETED")] and partial.exists() and held(crashed_id)==(0,0),"nuevo plan+reserva+generation, intento incrementado y salida nueva; intento interrumpido inmutable retenido")
        final_plan=plan()
        mutate("CREATE TRIGGER fixture_final_fault BEFORE INSERT ON ref_outbox WHEN (SELECT phase FROM ref_event WHERE id=NEW.event_id)='COMPLETED' BEGIN SELECT RAISE(ABORT,'controlled_final_fault'); END")
        rejected("transformation.execute",execute(final_plan),"reference_transform_persistence_failed")
        uncertain_id=query("SELECT id FROM ref_job WHERE phase='VERIFYING'")[0][0]
        uncertain_folder=work/"output"/f"{uncertain_id}-1"
        uncertain_sha=sha(uncertain_folder/"artifact.b64")
        step("final-outbox-failure-retains-resources",held(uncertain_id)==(1,1) and (uncertain_folder/"manifest.json").is_file() and not (work/"state/job-status"/f"{uncertain_id}.json").exists(),"DB no afirma COMPLETED ni retorna recibo al fallar outbox; directorio completo y journal quedan para reconciliar")
        session.close(); session=None
        mutate("DROP TRIGGER fixture_final_fault")
        session=connect()
        uncertain=json.loads(query("SELECT document FROM ref_job WHERE id=?",(uncertain_id,))[0][0])
        step("persistence-cause-survives-recovery",uncertain["state"]=="FAILED_RECOVERABLE" and uncertain["error"]["details"]["original_error"]["code"]=="IntegrityError" and held(uncertain_id)==(1,1) and (work/"state"/f"fault-{uncertain_id}-1.json").is_file() and not (work/"state"/f"fault-{uncertain_id}-1.tmp").exists(),"causa SQL redactada publicada con fsync+rename fuera del commit incierto; sólo el documento completo reaparece en el fallo recuperable")
        rejected("transformation.verify",{"job_id":uncertain_id},"reference_transform_job_not_completed")
        final_retry=plan(); final_receipt=session.result("transformation.execute",execute(final_retry)); output_observed(final_receipt)
        step("retry-after-uncertain-publication",final_receipt["job_id"]==uncertain_id and sha(uncertain_folder/"artifact.b64")==uncertain_sha and query("SELECT attempt,phase FROM ref_job WHERE id=?",(uncertain_id,))==[(2,"COMPLETED")],"sólo un nuevo preflight reinicia; publicación anterior completa queda intacta y nunca se presenta como un intento validado")
        original_fault_plan=plan()
        mutate("CREATE TRIGGER fixture_error_fault BEFORE INSERT ON ref_outbox WHEN (SELECT phase FROM ref_event WHERE id=NEW.event_id)='FAILED' BEGIN SELECT RAISE(ABORT,'controlled_error_record_fault'); END")
        active=session.send("transformation.execute",execute(original_fault_plan)); wait_encoding(active)
        original_fault_id,_=active_job()
        try:
            # Escritor de fault injection ajeno al worker; se restaura la fixture.
            # El primer bloque ya fue leído antes de TRANSFORM_ENCODING. Evitar
            # truncar el fichero: un truncate concurrente probaría otro fallo
            # (lectura corta/RUNNING) en lugar del hash final/VERIFYING.
            with source.open("r+b") as corruptor:
                corruptor.write(b"!")
                corruptor.flush()
                os.fsync(corruptor.fileno())
            reply=session.response(active)
        finally:
            source.write_bytes(data)
        step("error-commit-failure-is-not-success",reply["error"]["data"]["details"]["reason"]=="reference_transform_persistence_failed" and held(original_fault_id)==(1,1) and query("SELECT phase FROM ref_job WHERE id=?",(original_fault_id,))==[("VERIFYING",)],"corrupción externa real durante IO más fallo de outbox al registrar FAILED conserva el intento incierto")
        session.close(); session=None
        mutate("DROP TRIGGER fixture_error_fault")
        session=connect()
        original_error=json.loads(query("SELECT document FROM ref_job WHERE id=?",(original_fault_id,))[0][0])["error"]
        step("original-cause-precedes-secondary-outbox-error",original_error["code"]=="INTEGRITY_FAILED" and original_error["details"]["reason"]=="reference_transform_final_verification_failed" and held(original_fault_id)==(1,1),"recuperación conserva la causa original de integridad y no la sustituye por el fallo secundario al persistirla")
        original_retry=plan(); original_receipt=session.result("transformation.execute",execute(original_retry)); output_observed(original_receipt)
        step("combined-fault-restart-verifies-original",original_receipt["job_id"]==original_fault_id and query("SELECT attempt,phase FROM ref_job WHERE id=?",(original_fault_id,))==[(2,"COMPLETED")] and sha(source)==profile["input"]["sha256"],"nuevo preflight sobre fixture restaurada permite reiniciar desde cero con otra generation y liberar recursos al completar")
        eof_plan=plan(); active=session.send("transformation.execute",execute(eof_plan)); wait_encoding(active); eof_id,_=active_job()
        session.close(); session=None
        step("eof-cancels-active-and-drains",query("SELECT phase FROM ref_job WHERE id=?",(eof_id,))==[("CANCELLED",)] and held(eof_id)==(0,0),"EOF cancela trabajo real, persiste terminal y cierra sin pipes/tareas retenidos")
        session=connect(); common_plan=plan(); session.close(); session=None
        session=connect(handshake=False)
        checks=common(session,valid_method="transformation.describe",valid_params={},cancel_method="transformation.execute",cancel_params=execute(common_plan))
        session.close(); session=None
        step("common-conformance",len(checks)==7,"handshake obligatorio, nonce/capabilities, método ausente, params, reply, progress/heartbeat y core.cancel con worker real")
        current={p.relative_to(package).as_posix():sha(p) for p in package.rglob("*") if p.is_file()}
        step("immutable-package-and-input",current==initial and sha(source)==profile["input"]["sha256"] and not manifest["permissions"]["network_origins"] and not manifest["permissions"]["secret_refs"],"paquete/Python/SDK inmutables e input original intacto tras éxito, fallo, cancel, crash y recuperación; sin red ni secretos")
        success=True
        return 0
    finally:
        if session: session.close()
        record={"result":"PASS" if success else "FAIL","scope":"autonomous transformation worker with real streaming Base64 files, private ledger/attempts/journal/outbox/reservations; not model conversion/Core admission/P026 qualification","steps":steps,"run":run.relative_to(REPO).as_posix(),"package_sha256":manifest["package_sha256"],"input_sha256":profile["input"]["sha256"],"input_size_bytes":profile["input"]["size_bytes"],"host":platform.platform(),"generated_at":datetime.now(timezone.utc).isoformat(timespec="milliseconds").replace("+00:00","Z")}
        (run/"receipt.json").write_text(json.dumps(record,ensure_ascii=False,indent=2),encoding="utf-8")
        (REPO/"docs/reports/P026_runs/transformation-real.json").write_text(json.dumps(record,ensure_ascii=False,indent=2),encoding="utf-8")


if __name__=="__main__":
    sys.stdout.reconfigure(encoding="utf-8",errors="replace")
    raise SystemExit(main())
