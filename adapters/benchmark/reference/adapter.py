"""Tres firmas benchmark sobre echo HTTP real, muestras durables y validación."""
import json
import os
import sqlite3
import threading
import time
import uuid
from pathlib import Path

from larm_adapter_sdk import CapabilityRegistry, ContractViolation, DomainError, RpcError
from larm_adapter_sdk.access import origin, path_allowed
from larm_adapter_sdk.capabilities import canonical, digest
from ledger import Ledger, MAX_DOCUMENT, read_json, stamp, write_new
from stats import fingerprint_hash, summarize
from transport import Echo, failure
from config import CAPABILITY, MAPPING, ESTIMATOR, SUITE, SETTINGS, UNAVAILABLE


def memory(value):
    result={key:None for key in ("weights_bytes","kv_bytes","activation_bytes","workspace_bytes","overhead_bytes","total_bytes","multimodal_bytes","temporary_bytes","safety_margin_bytes")}
    result.update(workspace_bytes=str((1<<20)+(32<<10)+len(canonical(value))),per_pool=[],confidence="UNKNOWN",evidence_refs=["reference:bounded-capture-process-memory-unknown"])
    return result


class Adapter:
    def __init__(self,package,work,manifest,contracts):
        self.package=path_allowed(Path(package).absolute(),[Path(package).absolute()])
        self.work=path_allowed(Path(work).absolute(),[Path(work).absolute()])
        self.manifest,self.contracts=manifest,contracts
        roots={"reference-package":self.package,**{"reference-benchmark-"+name:self.work/name for name in ("profile","state","output")}}
        permissions=manifest["permissions"]
        if set(permissions["read_roots"])!=set(roots) or set(permissions["write_roots"])!={"reference-benchmark-state","reference-benchmark-output"} or permissions["process_allowlist"] or permissions["secret_refs"]:
            raise failure("FORBIDDEN","reference_benchmark_permissions_invalid")
        self.read_roots=list(roots.values()); self.write_roots=[roots[name] for name in permissions["write_roots"]]
        self.policy=read_json(self.package/"reference-benchmark.json",self.read_roots)
        if set(self.policy)!={"base_url","suite","fixture","settings"} or self.policy["suite"]!=SUITE or self.policy["settings"]!=SETTINGS:
            raise failure("VALIDATION_FAILED","reference_benchmark_policy_invalid")
        self.client=Echo(self.policy["base_url"])
        if len(permissions["network_origins"])!=1 or origin(permissions["network_origins"][0])!=origin(self.policy["base_url"]):
            raise failure("FORBIDDEN","reference_benchmark_origin_not_granted")
        self.profile=read_json(self.work/"profile/profile.json",self.read_roots)
        if set(self.profile)!={"variant_id","runtime_version_id","backend_id","launch_profile_id"}:
            raise failure("VALIDATION_FAILED","reference_benchmark_profile_invalid")
        self.registry=CapabilityRegistry(read_json(self.package/"reference-capabilities.json",self.read_roots),contracts,{ESTIMATOR:memory},{MAPPING})
        if manifest["capabilities"]!=[CAPABILITY] or self.policy["fixture"]!=self.registry.entry(CAPABILITY)["fixture"]:
            raise failure("UNSUPPORTED_CAPABILITY","reference_benchmark_fixture_not_supported")
        self.suite_hash,self.fixture_hash,self.settings_hash=digest(SUITE),digest(self.policy["fixture"]),digest(SETTINGS)
        self.binding(self.request())
        self.profile_hash=digest({"profile":self.profile,"policy":self.policy,"registry":self.registry.profile_hash,"package":manifest["package_sha256"]})
        self.output=self.work/"output"
        self.mutation=threading.Lock()
        self.ledger=None

    def open_session(self):
        self.ledger=Ledger(self.work/"state",self.output,self.profile_hash,self.contracts)

    def request(self):
        return self.profile|{"suite_id":SUITE["id"],"fixture_hash":self.fixture_hash,"settings_hash":self.settings_hash}

    def binding(self,request):
        self.contracts.request("benchmark.run",request)
        if any(request[key]!=value for key,value in self.profile.items()): raise failure("FORBIDDEN","reference_benchmark_resource_not_bound")
        if request["suite_id"]!=SUITE["id"]: raise failure("UNSUPPORTED_CAPABILITY","reference_benchmark_suite_not_supported")
        if request["fixture_hash"]!=self.fixture_hash or request["settings_hash"]!=self.settings_hash: raise failure("CONFLICT","reference_benchmark_hash_not_bound")

    def fingerprint(self,uri,observed):
        unavailable=("artifact_bundle_hash","revision","runtime_id","runtime_version","installation_hash","backend","driver_versions","hardware_fingerprint","launch_profile_hash","tokenizer_hash","chat_template_hash","effective_generation","artifact_storage_volume_keys")
        components={name:{"value":None,"status":"UNAVAILABLE","evidence_refs":["reference:backend-model-and-hardware-not-observed"]} for name in unavailable}
        components.update({
            "scope":{"value":"controlled_reference_http_measurement","status":"DECLARED","evidence_refs":["reference:no-inference-qualification"]},
            "protocol":{"value":MAPPING,"status":"VERIFIED" if observed else "DECLARED","evidence_refs":[uri]},
            "protocol_profile_hash":{"value":digest(self.policy),"status":"VERIFIED","evidence_refs":["reference:pinned-policy"]},
            "observer_profile_hash":{"value":self.profile_hash,"status":"VERIFIED","evidence_refs":["reference:private-observer-binding"]},
            "adapter_package":{"value":self.manifest["package_sha256"],"status":"DECLARED","evidence_refs":["reference:host-owns-package-verification"]},
        })
        return {"hash":fingerprint_hash(components),"components":components}

    def metrics(self,samples,artifact_sha):
        return {"scope":"controlled_reference_http_measurement","measurement":"http_roundtrip_decode_and_fixture_validation","latency_ms":summarize(samples),"successful_repetitions":sum(not sample["warmup"] and sample["success"] for sample in samples),"failed_samples":sum(not sample["success"] for sample in samples),"unavailable":UNAVAILABLE,"generation_settings_applicable":False,"inference_qualification_eligible":False,"observation_artifact_sha256":artifact_sha,"observer_profile_hash":self.profile_hash}

    def run(self,request,ctx):
        self.binding(request)
        self.registry.estimate(CAPABILITY,self.policy["fixture"]["input"])
        ctx.check_cancelled(); self.ledger.preflight()
        identity=str(uuid.uuid7())
        self.ledger.accept(identity,request)
        samples,observations=[],[]
        started_at,deadline=stamp(),time.monotonic()+1800
        try:
            for iteration in range(6):
                ctx.check_cancelled()
                if time.monotonic()>=deadline: raise failure("RESOURCE_UNAVAILABLE","reference_benchmark_timeout")
                ctx.progress("BENCHMARK_HTTP",None,None,"reference_request_started")
                start=time.monotonic_ns(); response,wire_hash,reason=None,None,None
                try:
                    response,wire_hash=self.client.invoke(self.policy["fixture"]["input"],ctx)
                    self.registry.validate(CAPABILITY,"output",response)
                    if response!=self.policy["fixture"]["output"]: raise failure("INTEGRITY_FAILED","reference_benchmark_fixture_mismatch")
                except DomainError as error:
                    ctx.check_cancelled()
                    reason=error.data["details"]["reason"]
                    response=None  # Nunca persiste cuerpo privado que falló schema.
                end=time.monotonic_ns()
                sample={"iteration":iteration,"warmup":iteration==0,"start_monotonic_ns":str(start),"end_monotonic_ns":str(end),"load_ms":None,"ttft_ms":None,"input_tokens":None,"output_tokens":None,"generation_duration_ms":None,"peak_ram_bytes":None,"peak_vram_bytes":None,"backend_effective":"REFERENCE_HTTP_ECHO","success":reason is None,"error":reason}
                observation={"iteration":iteration,"response":response,"wire_sha256":wire_hash}
                self.contracts.validate("larm.schema.json#/$defs/RawBenchmarkSample",sample)
                self.ledger.sample(identity,sample,observation)
                samples.append(sample); observations.append(observation)
                ctx.progress("BENCHMARK_CAPTURE",None,None,"reference_sample_durable")
            ctx.check_cancelled()
            self.ledger.transition(identity,"VERIFYING")
            if self.ledger.samples(identity)!=list(zip(samples,observations,strict=True)):
                raise failure("INTEGRITY_FAILED","reference_benchmark_samples_changed")
            raw={"schema_version":"1.0","benchmark_id":identity,"profile_hash":self.profile_hash,"binding":request,"suite":SUITE,"settings":SETTINGS,"fixture_hash":self.fixture_hash,"samples":samples,"observations":observations,"started_at":started_at,"finished_at":stamp()}
            stage=self.work/"state"/f"{identity}.staging"
            path_allowed(stage,self.write_roots,writing=True).mkdir(exist_ok=False)
            artifact_sha=write_new(stage/"observations.json",raw,self.write_roots)
            target=self.output/identity
            uri=(target/"observations.json").as_uri()
            metrics=self.metrics(samples,artifact_sha)
            status="FAIL" if any(not item["success"] for item in samples) else "PASS" if metrics["latency_ms"] is not None else "INCONCLUSIVE"
            report={"benchmark_id":identity,"fingerprint":self.fingerprint(uri,any(item["success"] for item in samples)),"suite_hash":self.suite_hash,"fixture_hash":self.fixture_hash,"settings_hash":self.settings_hash,"raw_samples":samples,"artifact_refs":[uri],"metrics":metrics,"status":status}
            self.contracts.response("benchmark.run",report)
            write_new(stage/"report.json",report,self.write_roots)
            ctx.check_cancelled(); self.ledger.preflight(new_run=False)
            path_allowed(stage,self.write_roots); path_allowed(target,self.write_roots,writing=True)
            os.rename(stage,target)
            self.ledger.transition(identity,"COMPLETED",report=report)
        except RpcError as error:
            cancelled=error.data and error.data["details"]["reason"]=="operation_cancelled"
            self.ledger.transition(identity,"CANCELLED" if cancelled else "FAILED",error=error.data or failure("RESOURCE_UNAVAILABLE","reference_benchmark_execution_failed").data)
            raise
        except sqlite3.Error as error:
            self.ledger.remember_fault(identity,self.ledger.persistence_error(error)); raise
        except OSError as original:
            error=failure("RESOURCE_UNAVAILABLE","reference_benchmark_io_failed")
            error.data["details"]["original_error"]={"system":"benchmark","code":type(original).__name__,"message":"owned_reference_operation_failed"}
            self.ledger.transition(identity,"FAILED",error=error.data); raise error from None
        return report

    def validate(self,report,ctx):
        stored=self.ledger.recorded(report["benchmark_id"])
        if stored is None: return {"valid":False,"errors":["reference_benchmark_not_owned_or_completed"]}
        if digest(report)!=stored["hash"] or report!=stored["report"]: return {"valid":False,"errors":["reference_benchmark_report_changed"]}
        self.binding(stored["request"])
        folder=path_allowed(self.output/report["benchmark_id"],self.read_roots)
        artifact=path_allowed(folder/"observations.json",self.read_roots)
        ctx.check_cancelled()
        raw,observed_sha=read_json(artifact,self.read_roots,with_hash=True)
        if report["artifact_refs"]!=[artifact.as_uri()] or observed_sha!=report["metrics"]["observation_artifact_sha256"]:
            return {"valid":False,"errors":["reference_benchmark_observations_changed"]}
        if read_json(folder/"report.json",self.read_roots)!=report:
            return {"valid":False,"errors":["reference_benchmark_published_report_changed"]}
        if raw["benchmark_id"]!=report["benchmark_id"] or raw["profile_hash"]!=self.profile_hash or raw["binding"]!=stored["request"] or raw["suite"]!=SUITE or raw["settings"]!=SETTINGS or raw["fixture_hash"]!=self.fixture_hash:
            return {"valid":False,"errors":["reference_benchmark_snapshot_changed"]}
        samples=report["raw_samples"]
        if len(samples)!=6 or [s["iteration"] for s in samples]!=list(range(6)) or [s["warmup"] for s in samples]!=[True,False,False,False,False,False] or raw["samples"]!=samples or self.ledger.samples(report["benchmark_id"])!=list(zip(samples,raw["observations"],strict=True)):
            return {"valid":False,"errors":["reference_benchmark_samples_changed"]}
        last=-1
        for sample,observation in zip(samples,raw["observations"],strict=True):
            start,end=int(sample["start_monotonic_ns"]),int(sample["end_monotonic_ns"])
            if start<last or end<=start or any(sample[key] is not None for key in UNAVAILABLE) or sample["backend_effective"]!="REFERENCE_HTTP_ECHO" or observation["iteration"]!=sample["iteration"] or (sample["success"] and (sample["error"] is not None or observation["response"]!=self.policy["fixture"]["output"])):
                return {"valid":False,"errors":["reference_benchmark_invalid_sample"]}
            last=end
        if report["metrics"]!=self.metrics(samples,report["metrics"]["observation_artifact_sha256"]) or report["fingerprint"]!=self.fingerprint(artifact.as_uri(),any(item["success"] for item in samples)):
            return {"valid":False,"errors":["reference_benchmark_aggregate_or_fingerprint_changed"]}
        if report["status"]!="PASS" or any(not s["success"] for s in samples) or summarize(samples) is None:
            return {"valid":False,"errors":["reference_benchmark_suite_not_passed"]}
        return {"valid":True,"errors":[]}

    def call(self,method,params,ctx):
        try:
            ctx.check_cancelled()
            if method=="benchmark.describe":
                return {"suite_id":SUITE["id"],"suite_hash":self.suite_hash,"capabilities":[CAPABILITY],"settings_schema":{"type":"object","additionalProperties":False,"required":list(SETTINGS),"properties":{key:{"const":value} for key,value in SETTINGS.items()}}}
            while not self.mutation.acquire(timeout=.05): ctx.check_cancelled()
            try:
                ctx.check_cancelled()
                if method=="benchmark.run": return self.run(params,ctx)
                if method=="benchmark.validate": return self.validate(params,ctx)
                raise failure("UNSUPPORTED_CAPABILITY","method_not_announced")
            finally: self.mutation.release()
        except ContractViolation:
            raise failure("VALIDATION_FAILED","reference_benchmark_contract_invalid") from None
        except (OSError,sqlite3.Error,ValueError,KeyError,TypeError):
            raise failure("RESOURCE_UNAVAILABLE","reference_benchmark_persistence_failed") from None

    def shutdown(self):
        if self.ledger is not None:
            self.ledger.close()
            self.ledger=None
