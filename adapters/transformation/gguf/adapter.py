"""Conversor GGUF de producto: cuatro firmas transformation con conversión
safetensors→GGUF v3 real, planes fijados y ledger propio."""
import os
import shutil
import sqlite3
import threading
from datetime import datetime, timedelta, timezone
from pathlib import Path

from larm_adapter_sdk import CapabilityRegistry, ContractViolation, DomainError, RpcError
from larm_adapter_sdk.access import path_allowed, relative
from larm_adapter_sdk.capabilities import digest
from files import CAPABILITY, ESTIMATOR, MAPPING, MAX_INPUT, TOOL_VERSION, bundle_hash, convert, fail, hash_file, inspect_input, memory, read_json, stamp, storage_estimate, write_new
from gguf import QUANTIZATIONS
from state import State


class Adapter:
    def __init__(self, package, work, manifest, contracts):
        self.package = path_allowed(Path(package).absolute(),[Path(package).absolute()])
        self.work = path_allowed(Path(work).absolute(),[Path(work).absolute()])
        self.manifest, self.contracts = manifest, contracts
        roots = {"product-package":self.package, **{f"product-gguf-{name}":self.work/name for name in ("input","profile","state","output")}}
        permissions = manifest["permissions"]
        if set(permissions["read_roots"]) != set(roots) or set(permissions["write_roots"]) != {"product-gguf-state","product-gguf-output"} or any(permissions[key] for key in ("network_origins","process_allowlist","secret_refs")):
            raise fail("FORBIDDEN", "product_gguf_permissions_invalid")
        self.read_roots = list(roots.values())
        self.write_roots = [roots[key] for key in permissions["write_roots"]]
        for root in self.read_roots:
            path_allowed(root,self.read_roots)
        self.profile = read_json(self.work / "profile/profile.json",self.read_roots)
        if set(self.profile) != {"input_variant_id","storage_location_id","input"} or set(self.profile["input"]) != {"relative_path","sha256","size_bytes"}:
            raise fail("VALIDATION_FAILED", "product_gguf_profile_invalid")
        self.input = self.work / "input" / relative(self.profile["input"]["relative_path"])
        self.output, self.state_root = self.work / "output", self.work / "state"
        self.registry = CapabilityRegistry(read_json(self.package / "product-capabilities.json",self.read_roots),contracts,{ESTIMATOR:memory},{MAPPING})
        if manifest["capabilities"] != [CAPABILITY]:
            raise fail("UNSUPPORTED_CAPABILITY", "product_gguf_capability_not_supported")
        self.registry.validate(CAPABILITY,"input",self.input_descriptor())
        self.request_contract(self.request())
        self.profile_hash = digest({"profile":self.profile,"registry":self.registry.profile_hash,"package":manifest["package_sha256"]})
        self.mutation = threading.Lock()
        self.state = None

    def open_session(self):
        self.state=State(self.state_root,self.contracts,self.profile_hash)

    def input_descriptor(self):
        return {key:self.profile["input"][key] for key in ("sha256","size_bytes")}

    def request(self):
        return {"input_variant_id":self.profile["input_variant_id"],"adapter_id":self.manifest["id"],"tool_version":TOOL_VERSION,"target_format":"GGUF","parameters":{},"storage_location_id":self.profile["storage_location_id"],"hard_budget_bytes":None}

    def request_contract(self, request):
        self.contracts.request("transformation.plan",request)
        if request["input_variant_id"] != self.profile["input_variant_id"] or request["storage_location_id"] != self.profile["storage_location_id"]:
            raise fail("FORBIDDEN", "product_gguf_resource_not_bound")
        parameters = request["parameters"]
        if not isinstance(parameters,dict) or set(parameters)-{"quantization"}:
            raise fail("UNSUPPORTED_CAPABILITY", "product_gguf_options_not_supported")
        if parameters.get("quantization") not in (None,)+QUANTIZATIONS:
            raise fail("UNSUPPORTED_CAPABILITY", "product_gguf_quant_unsupported")
        if request["adapter_id"] != self.manifest["id"] or request["tool_version"] != TOOL_VERSION or request["target_format"] != "GGUF":
            raise fail("UNSUPPORTED_CAPABILITY", "product_gguf_options_not_supported")

    def quantization(self, request):
        return request["parameters"].get("quantization") or None

    def preflight(self, request, ctx):
        self.request_contract(request)
        self.registry.estimate(CAPABILITY,self.input_descriptor())
        predicted = inspect_input(self.input,self.input_descriptor(),self.read_roots,ctx,self.quantization(request))
        self.registry.validate(CAPABILITY,"output",predicted)
        estimate = storage_estimate(int(predicted["size_bytes"]),int(self.profile["input"]["size_bytes"]),self.output,self.state_root)
        upper = int(estimate["upper_bound_bytes"])
        if request["hard_budget_bytes"] is not None and int(request["hard_budget_bytes"]) < upper:
            raise fail("STORAGE_INSUFFICIENT", "product_gguf_hard_budget_insufficient")
        if shutil.disk_usage(self.output).free < int(estimate["required_free_bytes"]):
            raise fail("STORAGE_INSUFFICIENT", "product_gguf_free_space_insufficient")
        return predicted,estimate

    def plan(self, request, ctx):
        predicted,estimate = self.preflight(request,ctx)
        binding = {"profile_hash":self.profile_hash,"input":self.input_descriptor(),"predicted":predicted,"scope":"product_private_conversion"}
        import uuid
        plan = {"plan_id":str(uuid.uuid7()),"operation":"transformation.execute","resource_ids":[self.profile["input_variant_id"],self.profile["storage_location_id"]],"storage_estimate":estimate,"steps":[{"phase":"GGUF","adapter_id":self.manifest["id"],"request":{"transform":request,"binding":binding}}],"expires_at":stamp(datetime.now(timezone.utc)+timedelta(seconds=900)),"approval_required":False}
        plan["plan_hash"] = digest(plan)
        self.contracts.response("transformation.plan",plan)
        ctx.check_cancelled()
        self.state.plan(plan,request,binding)
        return plan

    def journal(self, job_id):
        job = self.state.job(job_id)
        attempt = job["progress"]["attempt"]
        paths = self.state.journal(job_id)
        if set(paths) != {"staging","output","scope","input","predicted"} or paths["staging"] != f"staging/{job_id}-{attempt}" or paths["output"] != f"{job_id}-{attempt}" or paths["scope"] != "product_private_only" or paths["input"] != self.input_descriptor():
            raise fail("CONFLICT", "product_gguf_journal_changed")
        self.registry.validate(CAPABILITY,"output",paths["predicted"])
        return paths

    def verify(self, job_id, ctx):
        job = self.state.job(job_id)
        if job["state"] != "COMPLETED":
            raise fail("INVALID_STATE_TRANSITION", "product_gguf_job_not_completed")
        paths = self.journal(job_id)
        result = job["result"]
        folder = path_allowed(self.output / relative(paths["output"]),self.read_roots)
        stored = read_json(folder / "manifest.json",self.read_roots,16 << 10)
        self.contracts.response("transformation.verify",stored)
        if result is None or stored != result["manifest"] or bundle_hash(stored["artifacts"]) != stored["manifest_hash"] or stored["revision_key"] != "content:"+stored["manifest_hash"]:
            raise fail("INTEGRITY_FAILED", "product_gguf_manifest_changed")
        for artifact in stored["artifacts"]:
            name = relative(artifact["relative_path"])
            if name not in (paths["output"]+"/model.gguf",paths["output"]+"/provenance.json"):
                raise fail("INTEGRITY_FAILED", "product_gguf_manifest_changed")
            observed = hash_file(self.output / name,self.read_roots,ctx,max(int(paths["predicted"]["size_bytes"]),16 << 10))
            if observed != {key:artifact[key] for key in ("sha256","size_bytes")}:
                raise fail("INTEGRITY_FAILED", "product_gguf_output_changed")
        if len(stored["artifacts"]) != 2 or len({item["relative_path"] for item in stored["artifacts"]}) != 2:
            raise fail("INTEGRITY_FAILED", "product_gguf_manifest_changed")
        if read_json(folder / "provenance.json",self.read_roots,16 << 10) != result["provenance"]:
            raise fail("INTEGRITY_FAILED", "product_gguf_provenance_changed")
        return stored

    def execute(self, request, ctx):
        if request["approval_id"] is not None:
            raise fail("UNSUPPORTED_CAPABILITY", "product_gguf_approval_owned_by_core")
        stored = self.state.get_plan(request["plan_id"])
        plan = stored["document"]
        self.contracts.response("transformation.plan",plan)
        if request["plan_hash"] != stored["hash"] or plan["plan_hash"] != request["plan_hash"] or digest({key:value for key,value in plan.items() if key!="plan_hash"}) != request["plan_hash"] or plan["steps"] != [{"phase":"GGUF","adapter_id":self.manifest["id"],"request":{"transform":stored["request"],"binding":stored["binding"]}}] or stored["binding"]["profile_hash"] != self.profile_hash:
            raise fail("CONFLICT", "product_gguf_plan_hash_mismatch")
        if stored["outcome"]:
            if "error" in stored["outcome"]:
                error = stored["outcome"]["error"]
                raise RpcError(-32000,error["message"],error)
            self.verify(stored["outcome"]["completed_job_id"],ctx)
            return self.state.receipt(stored["outcome"]["completed_job_id"])
        if stored["job_id"]:
            raise fail("CONFLICT", "product_gguf_plan_already_used")
        if datetime.fromisoformat(plan["expires_at"].replace("Z","+00:00")) <= datetime.now(timezone.utc):
            raise fail("INVALID_STATE_TRANSITION", "product_gguf_plan_expired")
        predicted,estimate = self.preflight(stored["request"],ctx)
        if stored["binding"]["input"] != self.input_descriptor() or predicted != stored["binding"]["predicted"] or estimate["upper_bound_bytes"] != plan["storage_estimate"]["upper_bound_bytes"]:
            raise fail("CONFLICT", "product_gguf_snapshot_changed")
        ctx.check_cancelled()
        identity = digest({"profile_hash":self.profile_hash,"request":{key:value for key,value in stored["request"].items() if key!="hard_budget_bytes"}})
        job_id = self.state.accept(plan["plan_id"],identity,int(estimate["upper_bound_bytes"]))
        paths = self.journal(job_id)
        stage = self.state_root / relative(paths["staging"])
        target = self.output / relative(paths["output"])
        try:
            self.state.transition(job_id,"RUNNING")
            path_allowed(stage,self.write_roots,writing=True).mkdir(parents=True,exist_ok=False)
            path_allowed(stage,self.write_roots)
            upper = int(estimate["upper_bound_bytes"])
            def check_space(offset):
                remaining = upper-offset
                margin = max(1 << 30,(upper+19)//20)
                if shutil.disk_usage(self.output).free < remaining+margin:
                    raise fail("STORAGE_INSUFFICIENT", "product_gguf_free_space_insufficient")
            def on_tensor(name, written):
                ctx.progress("GGUF_TENSOR", written, int(predicted["size_bytes"]), "product_gguf_tensor_durable")
            convert(self.input,stage/"model.gguf",predicted,self.read_roots,self.write_roots,ctx,lambda offset:self.state.checkpoint(job_id,offset),check_space,quant=self.quantization(stored["request"]))
            self.state.transition(job_id,"VERIFYING")
            if hash_file(stage/"model.gguf",self.read_roots,ctx,int(predicted["size_bytes"])) != predicted or hash_file(self.input,self.read_roots,ctx,MAX_INPUT) != self.input_descriptor():
                raise fail("INTEGRITY_FAILED", "product_gguf_final_verification_failed")
            job = self.state.job(job_id)
            provenance = {"schema_version":"1.0","scope":"product_gguf_conversion","input_variant_id":self.profile["input_variant_id"],"source":self.profile["input"],"tool":{"id":self.manifest["id"],"version":TOOL_VERSION,"adapter_package_sha256":self.manifest["package_sha256"],"package_verification":"HOST_OWNED"},"parameters":stored["request"]["parameters"],"output":predicted,"job_id":job_id,"attempt":job["progress"]["attempt"],"created_at":stamp(),"actor":"product-gguf-worker","logs":["input_verified","gguf_written","output_verified"]}
            write_new(stage/"provenance.json",provenance,self.write_roots)
            artifacts = [{"relative_path":paths["output"]+"/model.gguf",**predicted,"required":True,"role":"DATA"},{"relative_path":paths["output"]+"/provenance.json",**hash_file(stage/"provenance.json",self.read_roots,ctx,16 << 10),"required":False,"role":"PROVENANCE"}]
            manifest_hash = bundle_hash(artifacts)
            manifest = {"revision_key":"content:"+manifest_hash,"manifest_hash":manifest_hash,"artifacts":artifacts}
            self.contracts.response("transformation.verify",manifest)
            write_new(stage/"manifest.json",manifest,self.write_roots)
            ctx.check_cancelled()
            path_allowed(stage,self.write_roots)
            path_allowed(target,self.write_roots,writing=True)
            os.rename(stage,target)  # Windows, mismo volumen y sin reemplazar destino
            self.state.transition(job_id,"COMPLETED",result={"manifest":manifest,"provenance":provenance})
        except RpcError as error:
            cancelled = error.data and error.data["details"]["reason"]=="operation_cancelled"
            if cancelled:
                self.state.transition(job_id,"CANCELLING",error=error.data)
                # Exactamente los tres archivos de este intento, nunca barrido.
                for name in ("model.gguf","provenance.json","manifest.json"):
                    path = path_allowed(stage/name,self.write_roots,writing=True)
                    if path.exists():
                        path.unlink()
                self.state.transition(job_id,"CANCELLED",error=error.data)
            else:
                self.state.transition(job_id,"FAILED",error=error.data or fail("RESOURCE_UNAVAILABLE","product_gguf_execution_failed").data)
            raise
        except sqlite3.Error as error:
            self.state.remember_fault(job_id,fail("RESOURCE_UNAVAILABLE","product_gguf_persistence_failed",original=error).data)
            raise
        except OSError as error:
            failure = fail("RESOURCE_UNAVAILABLE","product_gguf_io_failed",original=error)
            self.state.transition(job_id,"FAILED",error=failure.data)
            raise failure from None
        # SQLite/outbox incierto conserva fase, journal, locks y reservas.
        return self.state.receipt(job_id)

    def call(self, method, params, ctx):
        try:
            ctx.check_cancelled()
            if method=="transformation.describe":
                return {"formats_input":["SAFETENSORS"],"formats_output":["GGUF"],"parameter_schema":{"type":"object","properties":{"quantization":{"type":"string","enum":list(QUANTIZATIONS)}},"additionalProperties":False},"tool_version":TOOL_VERSION,"pause_supported":False}
            while not self.mutation.acquire(timeout=0.05):
                ctx.check_cancelled()
            try:
                ctx.check_cancelled()
                if method=="transformation.plan": return self.plan(params,ctx)
                if method=="transformation.execute": return self.execute(params,ctx)
                if method=="transformation.verify": return self.verify(params["job_id"],ctx)
                raise fail("UNSUPPORTED_CAPABILITY","method_not_announced")
            finally:
                self.mutation.release()
        except ContractViolation:
            raise fail("VALIDATION_FAILED","product_gguf_contract_invalid") from None
        except (OSError,sqlite3.Error) as error:
            raise fail("RESOURCE_UNAVAILABLE","product_gguf_persistence_failed",original=error) from None

    def shutdown(self):
        if self.state is not None:
            self.state.close()
            self.state=None
