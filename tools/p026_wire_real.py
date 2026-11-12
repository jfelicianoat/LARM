#!/usr/bin/env python3
"""Matriz de frames SDK + fallos de transporte del host contra cinco referencias.

Los proxies son mutantes de prueba con hash distinto, nunca candidatos cualificados.
No reemplaza los oráculos funcionales ni la cualificación/admisión de Core.
"""
import ctypes
import hashlib
import json
import os
import platform
import queue
import shutil
import subprocess
import sys
import threading
import uuid
from contextlib import contextmanager
from datetime import datetime, timezone
from pathlib import Path

REPO=Path(__file__).resolve().parent.parent
sys.path.insert(0,str(REPO/"tools/adapter-conformance"))
from runner import executable
from p026_negotiation_real import probe
from larm_adapter_sdk import Contracts

FLAGS=subprocess.CREATE_NO_WINDOW if os.name=="nt" else 0
ENV={k:v for k,v in os.environ.items() if k.upper() in ("SYSTEMROOT","SYSTEMDRIVE","WINDIR","TEMP","TMP","PATH","PROCESSOR_ARCHITECTURE","NUMBER_OF_PROCESSORS")}|{"PYTHONUTF8":"1"}
LIMIT=1<<20


def sha(path): return hashlib.sha256(path.read_bytes()).hexdigest()
def package_hash(root):
    files=sorted((p.relative_to(root).as_posix().lower(),sha(p)) for p in root.rglob("*") if p.is_file() and p.name!="manifest.json")
    return hashlib.sha256("".join(f"{name}\0{digest}\n" for name,digest in files).encode()).hexdigest()


@contextmanager
def reference(kind,folder):
    folder.mkdir(parents=True)
    helper=subprocess.Popen([sys.executable,str(REPO/"tools/p025_host_fixture.py"),kind,str(folder)],cwd=REPO,env=ENV,stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=subprocess.PIPE,creationflags=FLAGS)
    info=queue.Queue()
    reader=threading.Thread(target=lambda:info.put(helper.stdout.readline(LIMIT+1)),daemon=True)
    reader.start()
    try:
        raw=info.get(timeout=30)
        if not raw or len(raw)>LIMIT: raise AssertionError("fixture sin metadata válida")
        yield json.loads(raw)
    finally:
        helper.stdin.close()
        try: helper.wait(timeout=15)
        except subprocess.TimeoutExpired:
            subprocess.run(["taskkill","/PID",str(helper.pid),"/T","/F"],capture_output=True,check=False,timeout=10)
            helper.wait(timeout=5)
            raise AssertionError("fixture no cerró sus hijos tras EOF")
        finally:
            reader.join(timeout=2)
            helper.stdout.close(); helper.stderr.close()
        assert helper.returncode==0,"fixture terminó con error"


class Raw:
    def __init__(self,command,manifest,work):
        self.manifest,self.contracts=manifest,Contracts(REPO/"docs")
        self.process=subprocess.Popen(command,cwd=work,env=ENV,stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=subprocess.PIPE,creationflags=FLAGS)
        self.lines=queue.Queue()
        def read():
            while raw:=self.process.stdout.readline(LIMIT+2): self.lines.put(raw)
            self.lines.put(None)
        self.reader=threading.Thread(target=read,daemon=True); self.reader.start()
    def exchange(self,raw):
        self.process.stdin.write(raw+b"\n"); self.process.stdin.flush()
        while True:
            answer=self.lines.get(timeout=10)
            assert answer is not None and len(answer.removesuffix(b"\n"))<=LIMIT,"stdout inválido o cerrado"
            frame=json.loads(answer)
            self.contracts.frame("progress" if "method" in frame else "response",frame)
            if "method" not in frame: return frame
    def close(self):
        try:
            self.process.stdin.close(); self.process.wait(timeout=8)
            assert self.process.returncode==0
            self.reader.join(timeout=2); assert not self.reader.is_alive()
            assert self.process.stderr.read()==b""
        finally:
            if self.process.poll() is None:
                self.process.kill(); self.process.wait(timeout=5)
            self.process.stdout.close(); self.process.stderr.close()


def inspect_process(pid):
    """Consultar sólo los PID escritos por el proxy de esta ejecución propia."""
    kernel=ctypes.WinDLL("kernel32",use_last_error=True)
    kernel.OpenProcess.argtypes=[ctypes.c_uint32,ctypes.c_int,ctypes.c_uint32]
    kernel.OpenProcess.restype=ctypes.c_void_p
    kernel.CloseHandle.argtypes=[ctypes.c_void_p]
    kernel.WaitForSingleObject.argtypes=[ctypes.c_void_p,ctypes.c_uint32]
    kernel.WaitForSingleObject.restype=ctypes.c_uint32
    kernel.QueryFullProcessImageNameW.argtypes=[ctypes.c_void_p,ctypes.c_uint32,ctypes.c_wchar_p,ctypes.POINTER(ctypes.c_uint32)]
    handle=kernel.OpenProcess(0x100000|0x1000,False,int(pid))
    if not handle:
        if ctypes.get_last_error()==87: return None
        raise OSError("no se pudo comprobar un proceso propio")
    try:
        wait=kernel.WaitForSingleObject(handle,0)
        if wait==0: return None
        assert wait==258,"estado de proceso desconocido"
        text=ctypes.create_unicode_buffer(32768); size=ctypes.c_uint32(len(text))
        assert kernel.QueryFullProcessImageNameW(handle,0,text,ctypes.byref(size)),"imagen desconocida"
        return Path(text.value).resolve()
    finally: kernel.CloseHandle(handle)


def no_survivors(work,package):
    pids=json.loads((work/"wire-processes.json").read_text(encoding="utf-8"))
    leaks=[]
    for pid in pids.values():
        image=inspect_process(pid)
        if image is not None:
            assert image.is_relative_to(package.resolve()),"PID reutilizado fuera del paquete; no se termina"
            leaks.append(pid)
            subprocess.run(["taskkill","/PID",str(pid),"/T","/F"],capture_output=True,check=False,timeout=10)
    assert not leaks,"un proceso propio sobrevivió; limpiado, prueba FALLIDA"
    return pids


def raw_cases(kind,info,folder,passed):
    package,work=Path(info["package"]),Path(info["work"])
    manifest=json.loads((package/"manifest.json").read_text(encoding="utf-8"))
    before=package_hash(package)
    method,params=probe(kind,info)
    client=Raw(executable(package,manifest),manifest,work)
    fixtures=folder/"inputs"; fixtures.mkdir()
    def request(method,params,**extra): return {"jsonrpc":"2.0","id":str(uuid.uuid7()),"method":method,"params":params}|extra
    def exchange(name,frame,expected,*,method=None):
        raw=frame if isinstance(frame,bytes) else json.dumps(frame,ensure_ascii=False).encode()
        path=fixtures/(name+".ndjson"); path.write_bytes(raw+b"\n")
        answer=client.exchange(raw)
        if expected=="result":
            assert "result" in answer, name
            client.contracts.response(method,answer["result"])
            observed="result"
        else:
            observed=answer.get("error",{}).get("code")
            assert observed==expected,(kind,name,observed)
        if not isinstance(frame,bytes) and isinstance(frame["id"],str) and len(frame["id"])==36:
            assert answer["id"]==frame["id"],"se perdió la correlación"
        passed(kind,"sdk/"+name,expected,observed,{"input":path.relative_to(REPO).as_posix(),"input_sha256":sha(path),"input_bytes":len(raw)})
        return answer
    try:
        result=exchange("handshake-required",request(method,params),-32000)
        assert result["error"]["data"]["details"]["reason"]=="handshake_required"
        exchange("incompatible-major",request("core.handshake",{"protocol_version":"2.0","adapter_id":manifest["id"],"nonce":"incompatible"}),-32000)
        nonce=uuid.uuid4().hex
        result=exchange("compatible-handshake",request("core.handshake",{"protocol_version":"1.0","adapter_id":manifest["id"],"nonce":nonce}),"result",method="core.handshake")
        assert result["result"]["nonce"]==nonce and result["result"]["capabilities"]==manifest["capabilities"]
        exchange("extra-frame-field",request(method,params,unexpected=True),-32600)
        exchange("null-params",request(method,None),-32602)
        exchange("extra-param-field",request(method,params|{"unexpected":True}),-32602)
        exchange("malformed-json",b"{",-32700)
        exchange("invalid-utf8",b"\xff",-32700)
        exchange("nonfinite-json",b'{"value":NaN}',-32700)
        exchange("invalid-request-id",request(method,params,id="unknown-id"),-32600)
        exchange("unannounced-method",request("conformance.unannounced",{}),-32601)
        unknown=exchange("unknown-cancel-id",request("core.cancel",{"request_id":str(uuid.uuid7())}),"result",method="core.cancel")
        assert unknown["result"]=={"cancelled":False}
        boundary=request("core.heartbeat",{}); raw=json.dumps(boundary).encode()
        exact=raw+b" "*(LIMIT-len(raw))
        result=exchange("exact-frame-limit",exact,"result",method="core.heartbeat")
        assert result["id"]==boundary["id"]
        exchange("frame-limit-plus-one",exact+b" ",-32700)
        exchange("oversized-non-json",b"x"*(LIMIT+100),-32700)
        exchange("recover-valid-family-call",request(method,params),"result",method=method)
    finally: client.close()
    assert package_hash(package)==before==manifest["package_sha256"]
    passed(kind,"sdk/clean-eof-immutable-package","exit0; package unchanged","exit0; package unchanged",{"package_sha256":before})


def host_cases(kind,info,folder,passed):
    package,work=Path(info["package"]),Path(info["work"])
    manifest=json.loads((package/"manifest.json").read_text(encoding="utf-8"))
    method,params=probe(kind,info)
    entry=package/manifest["executable"]["relative_path"]
    actual=entry.with_name("wire-real-worker.exe")
    entry.rename(actual)
    shutil.copyfile(REPO/"target/debug/examples/wire_proxy.exe",entry)
    (package/"wire-proxy.json").write_text(json.dumps({"real_executable":actual.relative_to(package).as_posix(),"trigger_method":method}),encoding="utf-8")
    manifest["package_sha256"]=package_hash(package)
    (package/"manifest.json").write_text(json.dumps(manifest,indent=2),encoding="utf-8")
    expected_hash=manifest["package_sha256"]
    for mode in ("normal","malformed-once","unknown-id","duplicate","unknown-progress","reverse-request","malformed","oversized-no-newline","invalid-result","stall","crash","stderr-long"):
        case=folder/f"{mode}.input.json"
        case.write_text(json.dumps({"package":str(package),"work":str(work),"method":method,"params":params,"mode":mode},indent=2),encoding="utf-8")
        result=subprocess.run([str(REPO/"target/debug/examples/conformance_host.exe"),str(case)],cwd=REPO,env=ENV,capture_output=True,timeout=15,creationflags=FLAGS)
        assert result.returncode==0 and not result.stderr,(kind,mode,"host driver failed")
        observed=json.loads(result.stdout)
        (folder/f"{mode}.observed.json").write_bytes(result.stdout)
        assert "start_error" not in observed,(kind,mode,observed.get("start_error"))
        pids=no_survivors(work,package)
        assert observed["exited"] and package_hash(package)==expected_hash,(kind,mode,"process or package")
        if mode in ("normal","stderr-long","malformed-once"):
            expected_state="DEGRADED" if mode=="malformed-once" else "READY"
            assert observed["error"] is None and observed["state"]==expected_state,(kind,mode,observed)
            if mode=="malformed-once": assert observed["reason"]=="stdout_malformed"
            Contracts(REPO/"docs").response(method,observed["result"])
            if mode=="stderr-long": assert observed["stderr_dropped"]>=12<<20 and observed["retained_log_bytes"]<=16<<10
            expected=expected_state+"; canonical result; EOF closes both processes"
        else:
            expected_reason={"oversized-no-newline":"stdout_frame_too_large","stall":"cancel_grace_expired","crash":"process_exited"}.get(mode,"stdout_corrupt")
            assert observed["state"]=="FAILED" and observed["reason"]==expected_reason,(kind,mode,observed)
            if mode=="invalid-result":
                assert observed["error"]["code"]=="PROTOCOL_ERROR" and observed["error"]["details"]["reason"]=="invalid_adapter_reply"
                assert "private_fault_marker" not in result.stdout.decode()
            elif mode=="stall": assert observed["error"]["code"]=="TIMEOUT"
            elif mode!="duplicate": assert observed["error"] is not None and observed["result"] is None
            if mode=="unknown-progress": assert observed["foreign_progress_count"]==0
            expected="FAILED; "+expected_reason+"; no surviving processes"
        passed(kind,"host/"+mode,expected,{"state":observed["state"],"reason":observed["reason"],"exited":observed["exited"]},{"input":case.relative_to(REPO).as_posix(),"input_sha256":sha(case),"observed_sha256":sha(folder/f"{mode}.observed.json"),"package_sha256":expected_hash,"owned_pids":pids})


def main():
    assert os.name=="nt","Esta prueba requiere Windows real"
    run=REPO/"docs/reports/P026_runs/wire-real"/uuid.uuid4().hex[:12]
    run.mkdir(parents=True)
    steps=[]; success=False
    def passed(kind,name,expected,observed,details):
        steps.append({"family":kind,"id":name,"result":"PASS","expected":expected,"observed":observed,**details})
        print(f"PASS {kind}/{name}",flush=True)
    try:
        for kind in ("runtime","source","protocol","transformation","benchmark"):
            raw=run/kind/"raw"
            with reference(kind,raw) as info: raw_cases(kind,info,raw,passed)
            host=run/kind/"host"
            with reference(kind,host) as info: host_cases(kind,info,host,passed)
        success=True
        return 0
    finally:
        record={"result":"PASS" if success else "FAIL","scope":"Cinco workers reales: matriz de frames del SDK y host real con proxies mutantes para IDs, duplicados, progreso ajeno, sentido, schema, frame sin LF, cancel timeout, crash y stderr; no cualificación/promoción Core ni inferencia","steps":steps,"source_sha256":{p:sha(REPO/p) for p in ("tools/p026_wire_real.py","tools/p025_host_fixture.py","crates/larm-adapter-host/src/framing.rs","crates/larm-adapter-host/src/worker.rs","crates/larm-adapter-host/examples/wire_proxy.rs","crates/larm-adapter-host/examples/conformance_host.rs")},"run":run.relative_to(REPO).as_posix(),"host":platform.platform(),"generated_at":datetime.now(timezone.utc).isoformat()}
        record["binary_sha256"]={p:sha(REPO/p) for p in ("target/debug/examples/conformance_host.exe","target/debug/examples/wire_proxy.exe")}
        (run/"receipt.json").write_text(json.dumps(record,ensure_ascii=False,indent=2),encoding="utf-8")
        (REPO/"docs/reports/P026_runs/wire-real.json").write_text(json.dumps(record,ensure_ascii=False,indent=2),encoding="utf-8")


if __name__=="__main__":
    sys.stdout.reconfigure(encoding="utf-8",errors="replace")
    raise SystemExit(main())
