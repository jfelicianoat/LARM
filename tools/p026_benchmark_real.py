#!/usr/bin/env python3
"""Cinco repeticiones HTTP reales, artefactos/SQLite, errores y crash del benchmark."""
import copy
import hashlib
import http.client
import http.server
import importlib.util
import json
import math
import platform
import socket
import sqlite3
import statistics
import subprocess
import sys
import threading
import time
import uuid
from datetime import datetime, timezone
from pathlib import Path

REPO=Path(__file__).resolve().parent.parent
sys.path.insert(0,str(REPO/"tools/adapter-conformance"))
from runner import common, executable  # noqa: E402
from session import Session  # noqa: E402


def sha(path): return hashlib.sha256(path.read_bytes()).hexdigest()


class Proxy(http.server.BaseHTTPRequestHandler):
    def log_message(self,*_args): pass
    def do_POST(self):
        body=self.rfile.read(int(self.headers.get("Content-Length","0")))
        with self.server.lock:
            index=len(self.server.requests)
            self.server.requests.append({"path":self.path,"body_sha256":hashlib.sha256(body).hexdigest(),"authorization":self.headers.get("Authorization")})
        mode=self.server.mode
        status,content_type=200,"application/json"
        extra={}
        if mode=="auth": status,result=401,b'{}'
        elif mode=="rate": status,result=429,b'{}'
        elif mode=="redirect": status,result,extra=302,b'{}',{"Location":"http://127.0.0.1:1/never-follow"}
        elif mode=="wrong": result=b'{"echo":{"private":"private-benchmark-upstream-marker"}}'
        elif mode=="invalid": result=b'{"echo":'
        elif mode=="large": result=b' '*(40<<10)
        else:
            connection=http.client.HTTPConnection("127.0.0.1",self.server.backend_port,timeout=2)
            try:
                connection.request("POST",self.path,body=body,headers={"Content-Type":"application/json"})
                response=connection.getresponse(); status,result=response.status,response.read((1<<20)+1)
            except OSError: status,result=503,b'{}'
            finally: connection.close()
        self.server.received.set()
        if mode=="held": self.server.release.wait(timeout=3)
        if mode=="type": content_type="text/plain"
        with self.server.lock: self.server.responses[index]={"status":status,"wire_sha256":hashlib.sha256(result).hexdigest()}
        try:
            self.send_response(status); self.send_header("Content-Type",content_type); self.send_header("Content-Length",str(len(result)+(7 if mode=="length" else 0)))
            for name,value in extra.items(): self.send_header(name,value)
            self.end_headers(); self.wfile.write(result)
        except OSError: pass


def main():
    run=REPO/"docs/reports/P026_runs/benchmark-real"/uuid.uuid4().hex[:12]
    run.mkdir(parents=True)
    with socket.socket() as reserved: reserved.bind(("127.0.0.1",0)); backend_port=reserved.getsockname()[1]
    server=http.server.ThreadingHTTPServer(("127.0.0.1",0),Proxy)
    server.backend_port,server.requests,server.responses,server.mode=backend_port,[],{},"normal"
    server.lock,server.received,server.release=threading.Lock(),threading.Event(),threading.Event()
    thread=threading.Thread(target=server.serve_forever,daemon=True); thread.start()
    spec=importlib.util.spec_from_file_location("benchmark_factory",REPO/"adapters/benchmark/reference/package.py")
    factory=importlib.util.module_from_spec(spec); spec.loader.exec_module(factory)
    package,work=run/"package",run/"work"
    manifest,profile,policy,request=factory.create(REPO,package,work,f"http://127.0.0.1:{server.server_port}")
    initial={p.relative_to(package).as_posix():sha(p) for p in package.rglob("*") if p.is_file()}
    database=work/"state/benchmark.sqlite"
    session,backend,success,steps=None,None,False,[]

    def step(name,condition,detail):
        assert condition,name
        steps.append({"id":name,"result":"PASS","detail":detail}); print("PASS "+name,flush=True)
    def start_backend():
        child=subprocess.Popen([str(REPO/"target/debug/larm-reference-echo.exe"),"--port",str(backend_port)],stdin=subprocess.DEVNULL,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL,creationflags=subprocess.CREATE_NO_WINDOW)
        end=time.monotonic()+5
        while True:
            try:
                with socket.create_connection(("127.0.0.1",backend_port),timeout=.1): return child
            except OSError:
                if time.monotonic()>=end: child.kill(); child.wait(timeout=5); raise RuntimeError("backend propio no iniciado")
                time.sleep(.01)
    def connect(handshake=True):
        current=Session(executable(package,manifest),manifest,cwd=work)
        if handshake: current.handshake()
        return current
    def query(sql,parameters=()):
        with sqlite3.connect(database,timeout=3) as db: return db.execute(sql,parameters).fetchall()
    def mutate(sql):
        with sqlite3.connect(database,timeout=3) as db: db.execute(sql)
    def held(identity): return query("SELECT (SELECT count(*) FROM ref_lock WHERE run_id=?),(SELECT count(*) FROM ref_reservation WHERE run_id=?)",(identity,identity))[0]
    def rejected(params,reason):
        reply=session.call("benchmark.run",params)
        assert reply["error"]["data"]["details"]["reason"]==reason,reply
    def validate(report): return session.result("benchmark.validate",report)
    def capture(): return session.result("benchmark.run",request)
    def active():
        result=query("SELECT id FROM ref_run WHERE phase='RUNNING' ORDER BY rowid DESC LIMIT 1")
        assert len(result)==1
        return result[0][0]
    try:
        backend=start_backend(); session=connect()
        description=session.result("benchmark.describe",{})
        step("autonomous-three-methods",description["suite_id"]==request["suite_id"] and description["capabilities"]==["vendor.reference.echo"] and len([m for m in manifest["methods"] if m.startswith("benchmark.")])==3,"Python/SDK/contratos y descriptor completos; suite privada de transporte, sin atribuir inferencia")
        for changes,reason in (({"variant_id":str(uuid.uuid7())},"reference_benchmark_resource_not_bound"),({"backend_id":str(uuid.uuid7())},"reference_benchmark_resource_not_bound"),({"suite_id":"LLM_TEXT"},"reference_benchmark_suite_not_supported"),({"fixture_hash":"a"*64},"reference_benchmark_hash_not_bound"),({"settings_hash":"b"*64},"reference_benchmark_hash_not_bound")):
            rejected(request|changes,reason)
        invalid=session.call("benchmark.run",request|{"extra":True},validate=False)
        step("binding-hashes-schema-before-io",invalid["error"]["code"]==-32602 and not server.requests and not query("SELECT id FROM ref_run"),"recursos ajenos, suite de inferencia no implementada, hashes y campo extra no admiten medición ni HTTP")
        before=len(server.requests); report=capture(); samples=report["raw_samples"]
        step("actual-one-warmup-five-repetitions",report["status"]=="PASS" and len(server.requests)-before==6 and [s["iteration"] for s in samples]==list(range(6)) and [s["warmup"] for s in samples]==[True,False,False,False,False,False] and all(s["success"] for s in samples),"seis POST reales pasan por proxy al ejecutable Rust echo; warmup separado de cinco repeticiones")
        values=[(int(s["end_monotonic_ns"])-int(s["start_monotonic_ns"]))/1e6 for s in samples[1:]]
        observed=report["metrics"]["latency_ms"]; expected_mean=statistics.mean(values); expected_sd=statistics.stdev(values); spread=2.776*expected_sd/math.sqrt(5)
        near=lambda a,b:math.isclose(a,b,rel_tol=1e-10,abs_tol=1e-10)
        step("statistics-independent-oracle",all(v>0 for v in values) and near(observed["mean"],expected_mean) and near(observed["median"],statistics.median(values)) and near(observed["p95"],max(values)) and near(observed["sample_stddev"],expected_sd) and near(observed["ci95_mean"]["lower"],max(0,expected_mean-spread)) and near(observed["ci95_mean"]["upper"],expected_mean+spread),"stdlib statistics independiente del agregador del worker; monotonic ns, mediana, nearest-rank p95, sd muestral e IC95% t=2.776")
        folder=work/"output"/report["benchmark_id"]; observation_file=folder/"observations.json"
        raw=json.loads(observation_file.read_text(encoding="utf-8"))
        step("actual-artifact-and-response-hashes",report["artifact_refs"]==[observation_file.as_uri()] and report["metrics"]["observation_artifact_sha256"]==sha(observation_file) and all(o["wire_sha256"]==server.responses[before+i]["wire_sha256"] for i,o in enumerate(raw["observations"])),"artefacto existente contiene muestras y snapshots; wire SHA observado por otro proceso/servidor, no referencia ficticia")
        step("unavailable-not-zero-or-qualified",all(all(s[key] is None for key in ("load_ms","ttft_ms","input_tokens","output_tokens","generation_duration_ms","peak_ram_bytes","peak_vram_bytes")) for s in samples) and report["fingerprint"]["components"]["hardware_fingerprint"]["status"]=="UNAVAILABLE" and not report["metrics"]["inference_qualification_eligible"],"tokens/TTFT/memoria/backend/hardware no observados conservan null y razones; PASS sólo corresponde a la suite echo")
        count=len(server.requests)
        step("validate-reloads-owned-evidence",validate(report)=={"valid":True,"errors":[]} and len(server.requests)==count and held(report["benchmark_id"])==(0,0),"validador relee ledger, muestras, artefacto, hashes y fórmulas; no hace más HTTP ni confunde estructura con procedencia")
        for mutator in (lambda value:value["metrics"]["latency_ms"].update(mean=999),lambda value:value.update(benchmark_id=str(uuid.uuid7())),lambda value:value.update(settings_hash="f"*64),lambda value:value["fingerprint"]["components"]["hardware_fingerprint"].update(status="VERIFIED",value="invented"),lambda value:value["metrics"].update(inference_qualification_eligible=True)):
            bad=copy.deepcopy(report); mutator(bad); assert not validate(bad)["valid"]
        step("forged-report-rejected",True,"estadística, identidad, settings, evidencia desconocida elevada o elegibilidad inventada no coinciden con medición durable")
        saved=observation_file.read_bytes(); observation_file.write_bytes(saved+b" ")
        step("artifact-tampering-detected",not validate(report)["valid"],"cambio físico posterior invalida evidencia aunque la DB y el JSON semántico sigan iguales")
        observation_file.write_bytes(saved)
        session.close(); session=None; session=connect()
        step("completed-measurement-survives-worker-restart",validate(report)["valid"],"ledger y artefactos reales permiten validar después de otro proceso, sin caché volátil de resultados")
        replay=capture()
        step("new-run-observes-again-same-execution-identity",replay["benchmark_id"]!=report["benchmark_id"] and replay["fingerprint"]["hash"]==report["fingerprint"]["hash"] and replay["artifact_refs"]!=report["artifact_refs"],"nueva medición vuelve a ejecutar HTTP; trazas/tiempos cambian sin alterar el hash de componentes observables")
        for mode in ("auth","rate","redirect","invalid","wrong","large","type","length"):
            server.mode=mode; start=len(server.requests); failed=capture()
            step("real-http-failure-"+mode,failed["status"]=="FAIL" and failed["metrics"]["latency_ms"] is None and failed["metrics"]["failed_samples"]==6 and len(server.requests)-start==6 and not validate(failed)["valid"],"HTTP real fallido conserva seis muestras/causas y no aprueba ni inventa agregados")
            artifact=work/"output"/failed["benchmark_id"] / "observations.json"
            assert "private-benchmark-upstream-marker" not in artifact.read_text(encoding="utf-8")
        server.mode="normal"
        mutate("CREATE TRIGGER fixture_accept_fault BEFORE INSERT ON ref_outbox BEGIN SELECT RAISE(ABORT,'controlled_accept_fault'); END")
        runs=query("SELECT count(*) FROM ref_run")[0][0]; start=len(server.requests)
        rejected(request,"reference_benchmark_persistence_failed")
        step("outbox-accept-atomic",query("SELECT count(*) FROM ref_run")[0][0]==runs and not query("SELECT * FROM ref_lock") and not query("SELECT * FROM ref_reservation") and len(server.requests)==start,"fallo SQL de admisión revierte run/journal/event/outbox/reserva/lock antes del primer HTTP")
        mutate("DROP TRIGGER fixture_accept_fault")
        server.mode="held"; server.release.clear(); server.received.clear()
        pending=session.send("benchmark.run",request); session.wait_progress(pending); assert server.received.wait(timeout=3)
        collision_id=active(); collision=work/"output"/collision_id; collision.mkdir(); (collision/"sentinel.txt").write_bytes(b"owned-collision-sentinel")
        server.mode="normal"; server.release.set()
        write_error=session.response(pending)["error"]["data"]
        step("real-publication-io-failure-no-overwrite",write_error["details"]["reason"]=="reference_benchmark_io_failed" and query("SELECT phase FROM ref_run WHERE id=?",(collision_id,))==[("FAILED",)] and held(collision_id)==(0,0) and (collision/"sentinel.txt").read_bytes()==b"owned-collision-sentinel" and write_error["details"]["original_error"] is not None,"destino propio aparecido durante HTTP provoca error real de rename; conserva causa y staging sin sobrescribir ni retornar informe")
        server.mode="held"; server.release.clear(); server.received.clear()
        pending=session.send("benchmark.run",request); session.wait_progress(pending); assert server.received.wait(timeout=3)
        cancelled_id=active(); session.result("core.cancel",{"request_id":pending}); server.release.set()
        cancelled=session.response(pending)["error"]["data"]
        step("active-cancel-durable",cancelled["details"]["reason"]=="operation_cancelled" and query("SELECT phase FROM ref_run WHERE id=?",(cancelled_id,))==[("CANCELLED",)] and held(cancelled_id)==(0,0) and not (work/"output"/cancelled_id).exists(),"cancelación durante IO real no publica informe ni pierde estado terminal/outbox")
        server.release.clear(); server.received.clear()
        pending=session.send("benchmark.run",request); session.wait_progress(pending); assert server.received.wait(timeout=3)
        crashed_id=active(); session.crash(); session=None; server.release.set()
        step("actual-worker-crash-holds-resources",held(crashed_id)==(1,1),"muerte sólo del worker creado; journal y reserva continúan hasta adquirir nueva ownership física")
        session=connect()
        step("recovery-retains-evidence-and-ends-interrupted-run",query("SELECT phase FROM ref_run WHERE id=?",(crashed_id,))==[("FAILED",)] and held(crashed_id)==(0,0) and query("SELECT phase FROM ref_journal WHERE run_id=?",(crashed_id,))==[("FAILED",)],"nuevo dueño marca fallido sin reanudar ni publicar; conserva muestras/journal y libera recursos tras comprobar dueño anterior terminado")
        hello=json.dumps({"jsonrpc":"2.0","id":str(uuid.uuid7()),"method":"core.handshake","params":{"protocol_version":"1.0","adapter_id":manifest["id"],"nonce":uuid.uuid4().hex}}).encode()+b"\n"
        contender=subprocess.run(executable(package,manifest),cwd=work,input=hello,capture_output=True,timeout=15)
        denied=json.loads(contender.stdout)
        step("second-worker-denied",contender.returncode==0 and denied["error"]["data"]["details"]["reason"]=="reference_benchmark_worker_locked","lock real niega abrir una segunda sesión durante handshake válido; no recupera por tiempo")
        server.mode="normal"
        mutate("CREATE TRIGGER fixture_sample_fault BEFORE INSERT ON ref_sample BEGIN SELECT RAISE(ABORT,'controlled_sample_fault'); END")
        rejected(request,"reference_benchmark_persistence_failed")
        incomplete=active(); rejected(request,"reference_benchmark_recovery_required")
        step("sample-commit-failure-no-false-success",held(incomplete)==(1,1) and query("SELECT count(*) FROM ref_sample WHERE run_id=?",(incomplete,))==[(0,)],"fallo real al confirmar muestra no retorna benchmark ni abre otro writer con recursos inciertos")
        session.close(); session=None; mutate("DROP TRIGGER fixture_sample_fault"); session=connect()
        cause=json.loads(query("SELECT error FROM ref_run WHERE id=?",(incomplete,))[0][0])
        step("original-persistence-error-survives",cause["details"]["original_error"]["code"]=="IntegrityError" and held(incomplete)==(0,0),"registro atómico de causa redactada sobrevive al commit fallido y al reinicio")
        mutate("CREATE TRIGGER fixture_final_fault BEFORE INSERT ON ref_outbox WHEN (SELECT phase FROM ref_event WHERE id=NEW.event_id)='COMPLETED' BEGIN SELECT RAISE(ABORT,'controlled_final_fault'); END")
        rejected(request,"reference_benchmark_persistence_failed")
        uncertain=query("SELECT id FROM ref_run WHERE phase='VERIFYING'")[0][0]
        uncommitted=json.loads((work/"output"/uncertain/"report.json").read_text(encoding="utf-8")); preserved=sha(work/"output"/uncertain/"observations.json")
        step("final-outbox-fault-not-validatable",held(uncertain)==(1,1) and not validate(uncommitted)["valid"],"directorio completo no se convierte en informe confirmado cuando falla transacción/outbox final")
        session.close(); session=None; mutate("DROP TRIGGER fixture_final_fault"); session=connect()
        recovered=capture()
        step("fresh-run-after-uncertain-publication",recovered["benchmark_id"]!=uncertain and validate(recovered)["valid"] and query("SELECT phase FROM ref_run WHERE id=?",(uncertain,))==[("FAILED",)] and sha(work/"output"/uncertain/"observations.json")==preserved,"nuevo preflight/run mide desde warmup; evidencia anterior conservada e inválida, sin reutilizar un supuesto éxito")
        backend.kill(); backend.wait(timeout=5); backend=None
        offline=capture()
        step("actual-backend-crash",offline["status"]=="FAIL" and offline["metrics"]["failed_samples"]==6,"muerte del ejecutable echo propio produce seis fallos de IO reales, sin benchmark aprobado")
        backend=start_backend(); repaired=capture()
        step("backend-restarted-next-measurement",repaired["status"]=="PASS" and validate(repaired)["valid"],"worker mide de nuevo al volver el backend, manteniendo desconocida su identidad binaria remota")
        server.mode="held"; server.release.clear(); server.received.clear()
        pending=session.send("benchmark.run",request); session.wait_progress(pending); assert server.received.wait(timeout=3); eof_id=active()
        session.close(); session=None; server.release.set()
        step("eof-cancels-owned-measurement",query("SELECT phase FROM ref_run WHERE id=?",(eof_id,))==[("CANCELLED",)] and held(eof_id)==(0,0),"EOF cancela callback, persiste estado y drena pipes sin publicar un resultado incompleto")
        server.release.clear(); session=connect(handshake=False)
        checks=common(session,valid_method="benchmark.describe",valid_params={},cancel_method="benchmark.run",cancel_params=request)
        server.release.set(); session.close(); session=None
        step("common-conformance",len(checks)==7,"handshake/nonce/capabilities, métodos, params, reply, progreso/heartbeat y core.cancel en worker real")
        current={p.relative_to(package).as_posix():sha(p) for p in package.rglob("*") if p.is_file()}
        step("immutable-package-and-consistent-ledger",current==initial and not query("SELECT * FROM ref_lock") and not query("SELECT * FROM ref_reservation") and query("SELECT count(*) FROM ref_event")==query("SELECT count(*) FROM ref_outbox") and all(item["authorization"] is None for item in server.requests),"paquete/Python/SDK intactos, estados/outbox coherentes y ninguna credencial heredada")
        success=True; return 0
    finally:
        server.release.set()
        if session: session.close()
        if backend and backend.poll() is None:
            try:
                connection=http.client.HTTPConnection("127.0.0.1",backend_port,timeout=2); connection.request("POST","/shutdown",body=b""); connection.getresponse().read(); connection.close(); backend.wait(timeout=5)
            except (OSError,subprocess.TimeoutExpired): backend.kill(); backend.wait(timeout=5)
        server.shutdown(); server.server_close(); thread.join(timeout=2)
        record={"result":"PASS" if success else "FAIL","scope":"autonomous benchmark worker measures actual controlled HTTP echo (1 warmup + 5), durable raw artifacts/ledger/outbox/reservations; not inference/quality/Core admission or P026 qualification","steps":steps,"run":run.relative_to(REPO).as_posix(),"package_sha256":manifest["package_sha256"],"backend_sha256":sha(REPO/"target/debug/larm-reference-echo.exe"),"host":platform.platform(),"generated_at":datetime.now(timezone.utc).isoformat(timespec="milliseconds").replace("+00:00","Z")}
        (run/"receipt.json").write_text(json.dumps(record,ensure_ascii=False,indent=2),encoding="utf-8")
        (REPO/"docs/reports/P026_runs/benchmark-real.json").write_text(json.dumps(record,ensure_ascii=False,indent=2),encoding="utf-8")


if __name__=="__main__":
    sys.stdout.reconfigure(encoding="utf-8",errors="replace")
    raise SystemExit(main())
