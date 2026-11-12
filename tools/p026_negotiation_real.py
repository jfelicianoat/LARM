#!/usr/bin/env python3
"""Negociación de las cinco referencias reales; no acredita cualificación P026."""
import copy
import hashlib
import json
import os
import platform
import subprocess
import sys
import uuid
from datetime import datetime, timezone
from pathlib import Path

REPO=Path(__file__).resolve().parent.parent
sys.path.insert(0,str(REPO/"tools/adapter-conformance"))
from runner import executable
from session import Session
from larm_adapter_sdk.capabilities import digest


def snapshot(root, *, exclude_locks=False):
    return {p.relative_to(root).as_posix():hashlib.sha256(p.read_bytes()).hexdigest() for p in root.rglob("*") if p.is_file() and not (exclude_locks and p.name in ("owner.lock","reference-source.lock"))}


def probe(kind, info):
    if kind=="runtime": return "runtime.list_versions",{"runtime_id":str(uuid.uuid7()),"channel":"custom","cursor":None}
    if kind=="source":
        profile=json.loads((Path(info["work"])/"profile.json").read_text(encoding="utf-8"))
        return "source.auth_probe",{"source_id":profile["source_id"],"auth_mode":"NONE","account_id":None,"credential_reference_id":None}
    if kind=="protocol": return "protocol.describe",{"endpoint_id":info["profile"]["endpoint_id"]}
    if kind=="transformation": return "transformation.describe",{}
    return "benchmark.describe",{}


def main():
    run=REPO/"docs/reports/P026_runs/negotiation-real"/uuid.uuid4().hex[:12]
    run.mkdir(parents=True)
    steps=[]; packages={}; success=False
    def passed(kind,name,condition):
        assert condition,f"{kind}: {name}"
        steps.append({"family":kind,"id":name,"result":"PASS"})
        print(f"PASS {kind}/{name}",flush=True)
    try:
        for kind in ("runtime","source","protocol","transformation","benchmark"):
            folder=run/kind; folder.mkdir()
            fixture=subprocess.Popen([sys.executable,str(REPO/"tools/p025_host_fixture.py"),kind,str(folder)],stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=subprocess.PIPE,creationflags=subprocess.CREATE_NO_WINDOW)
            session=None
            try:
                info=json.loads(fixture.stdout.readline())
                package,work=Path(info["package"]),Path(info["work"])
                manifest=json.loads((package/"manifest.json").read_text(encoding="utf-8"))
                packages[kind]={"manifest_sha256":hashlib.sha256((package/"manifest.json").read_bytes()).hexdigest(),"package_sha256":manifest["package_sha256"],"capabilities":manifest["capabilities"]}
                command=executable(package,manifest)
                initial_package=snapshot(package); untouched=snapshot(work)
                method,params=probe(kind,info)
                session=Session(command,manifest,cwd=work)
                refused=session.call("core.handshake",{"protocol_version":"2.0","adapter_id":manifest["id"],"nonce":"incompatible"},validate=False)
                blocked=session.call(method,params)
                session.close(); session=None
                expected_reason="protocol_version_mismatch" if kind=="runtime" else "handshake_mismatch"
                passed(kind,"incompatible-before-state-and-callback",refused.get("error",{}).get("data",{}).get("code")=="PROTOCOL_ERROR" and refused["error"]["data"]["details"]["reason"]==expected_reason and blocked.get("error",{}).get("data",{}).get("details",{}).get("reason")=="handshake_required" and snapshot(work)==untouched)
                session=Session(command,manifest,cwd=work)
                hello=session.handshake()
                session.result(method,params)
                if kind in ("source","transformation","benchmark"):
                    before=snapshot(work,exclude_locks=True)
                    rival=Session(command,manifest,cwd=work)
                    try:
                        denied=rival.call("core.handshake",{"protocol_version":"1.0","adapter_id":manifest["id"],"nonce":"second-owner"})
                        still_blocked=rival.call(method,params)
                        expected={"source":"reference_worker_locked","transformation":"reference_transform_worker_locked","benchmark":"reference_benchmark_worker_locked"}[kind]
                        passed(kind,"second-owner-conflict-without-recovery",denied.get("error",{}).get("data",{}).get("code")=="CONFLICT" and denied["error"]["data"]["details"]["reason"]==expected and still_blocked.get("error",{}).get("data",{}).get("details",{}).get("reason")=="handshake_required" and snapshot(work,exclude_locks=True)==before and session.result("core.heartbeat",{})["alive"])
                    finally:
                        rival.close()
                session.close(); session=None
                passed(kind,"explicit-capabilities-and-family-callback",bool(hello["capabilities"]) and set(hello["capabilities"])==set(manifest["capabilities"]))
                baseline=snapshot(work)
                path=package/"reference-capabilities.json"
                original=path.read_bytes(); bundle=json.loads(original)
                for mutation in ("missing-bundle","unregistered-estimator","changed-schema-hash","external-schema-ref"):
                    try:
                        if mutation=="missing-bundle": path.unlink()
                        else:
                            changed=copy.deepcopy(bundle); entry=changed["entries"][0]
                            if mutation=="unregistered-estimator": entry["estimator_id"]="unregistered.reference.v1"
                            elif mutation=="changed-schema-hash": entry["input_sha256"]="0"*64
                            else:
                                entry["input_schema"]={"$ref":"https://127.0.0.1:1/forbidden.json"}
                                entry["input_sha256"]=digest(entry["input_schema"])
                            path.write_text(json.dumps(changed),encoding="utf-8")
                        request={"jsonrpc":"2.0","id":str(uuid.uuid7()),"method":"core.handshake","params":{"protocol_version":"1.0","adapter_id":manifest["id"],"nonce":"invalid-package"}}
                        env={k:v for k,v in os.environ.items() if k.upper() in ("SYSTEMROOT","SYSTEMDRIVE","WINDIR","TEMP","TMP","PATH","PROCESSOR_ARCHITECTURE","NUMBER_OF_PROCESSORS")}
                        result=subprocess.run(command,cwd=work,input=(json.dumps(request)+"\n").encode(),capture_output=True,timeout=10,env=env|{"PYTHONUTF8":"1"},creationflags=subprocess.CREATE_NO_WINDOW)
                        passed(kind,mutation,result.returncode==2 and result.stdout==b"" and 0<len(result.stderr)<512 and snapshot(work)==baseline)
                    finally:
                        path.write_bytes(original)
                if kind in ("source","transformation","benchmark"):
                    lock=work/"reference-source.lock" if kind=="source" else work/"state/owner.lock"
                    magic=lock.read_bytes()
                    try:
                        lock.write_bytes(b"foreign-lock-sentinel\n")
                        corrupt=snapshot(work)
                        session=Session(command,manifest,cwd=work)
                        denied=session.call("core.handshake",{"protocol_version":"1.0","adapter_id":manifest["id"],"nonce":"foreign-lock"})
                        expected={"source":"reference_lock_file_conflict","transformation":"reference_transform_lock_not_owned","benchmark":"reference_benchmark_lock_not_owned"}[kind]
                        assert denied.get("error",{}).get("data",{}).get("code")=="CONFLICT" and denied["error"]["data"]["details"]["reason"]==expected
                        session.close(); session=None
                        assert snapshot(work)==corrupt,"un lock ajeno no permite modificar ni recuperar la DB"
                    finally:
                        lock.write_bytes(magic)
                    # El intento rechazado debe liberar también el lock físico.
                    session=Session(command,manifest,cwd=work)
                    session.handshake(); session.result(method,params)
                    session.close(); session=None
                    passed(kind,"foreign-lock-preserved-and-failed-owner-released",lock.read_bytes()==magic)
                assert snapshot(package)==initial_package,"paquete propio no restaurado"
            finally:
                if session is not None: session.close()
                fixture.stdin.close()
                try:
                    fixture.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    fixture.kill(); fixture.wait(timeout=5)
                    raise AssertionError("fixture no terminó tras EOF")
                finally:
                    fixture.stdout.close(); fixture.stderr.close()
                assert fixture.returncode==0,"fixture falló"
        success=True
        return 0
    finally:
        record={"result":"PASS" if success else "FAIL","scope":"Negociación y cuatro corrupciones de contratos sobre paquetes propios; cinco procesos reales, sin acceso a cuentas ni cualificación/admisión Core","steps":steps,"packages":packages,"run":run.relative_to(REPO).as_posix(),"host":platform.platform(),"generated_at":datetime.now(timezone.utc).isoformat()}
        (run/"receipt.json").write_text(json.dumps(record,ensure_ascii=False,indent=2),encoding="utf-8")
        (REPO/"docs/reports/P026_runs/negotiation-real.json").write_text(json.dumps(record,ensure_ascii=False,indent=2),encoding="utf-8")


if __name__=="__main__":
    sys.stdout.reconfigure(encoding="utf-8",errors="replace")
    raise SystemExit(main())
