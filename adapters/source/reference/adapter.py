"""Nueve métodos source P043 con catálogo y transferencia reales de fixtures."""
import copy
import base64
import ctypes
import json
import os
import re
import shutil
import sqlite3
import threading
from datetime import datetime, timedelta, timezone
from pathlib import Path

from larm_adapter_sdk import CapabilityRegistry, ContractViolation, DomainError, RpcError
from larm_adapter_sdk.source_acquisition import validate_source_acquisition, select_source_acquisition, source_acquisition_payload
from catalog import Catalog, digest, stamp
from state import State
from transfer import fail, file_hash, open_public, origin, path_allowed, relative, transfer

CAPABILITY_METHODS={"source.search":"source.search","source.list_revisions":"source.revisions","source.download":"source.resume","source.get_manifest":"source.sha256","source.sync":"source.sync"}
CAPABILITY_MAPPINGS={"reference.source.search.v1","reference.source.revisions.v1","reference.source.range-download.v1","reference.source.sha256-manifest.v1","reference.source.sync.v1"}


def memory(_value):
    result={key:None for key in ("weights_bytes","kv_bytes","activation_bytes","workspace_bytes","overhead_bytes","total_bytes","multimodal_bytes","temporary_bytes","safety_margin_bytes")}
    result.update(workspace_bytes=str(2<<20),per_pool=[],confidence="UNKNOWN",evidence_refs=["reference:bounded-rpc-buffers-process-memory-unknown"])
    return result


def volume_key(path):
    if os.name != "nt":
        raise fail("UNSUPPORTED_CAPABILITY", "reference_windows_only")
    mount, name = ctypes.create_unicode_buffer(32768), ctypes.create_unicode_buffer(128)
    kernel = ctypes.windll.kernel32
    if not kernel.GetVolumePathNameW(ctypes.c_wchar_p(str(path)), mount, len(mount)) or not kernel.GetVolumeNameForVolumeMountPointW(ctypes.c_wchar_p(mount.value), name, len(name)):
        raise fail("RESOURCE_UNAVAILABLE", "reference_volume_identity_unknown")
    return name.value.split("{")[1].split("}")[0].lower()


class Adapter:
    def __init__(self, package, work, manifest, contracts):
        self.package, self.work = Path(package).resolve(strict=True), Path(work).resolve(strict=True)
        self.manifest, self.contracts = manifest, contracts
        self.config = json.loads((self.package / "reference-source.json").read_text(encoding="utf-8"))
        root_map = {"reference-package": self.package, "reference-work": self.work, "reference-catalog": self.work / "catalog"}
        permissions = manifest["permissions"]
        if set(permissions["read_roots"] + permissions["write_roots"]) - root_map.keys():
            raise fail("FORBIDDEN", "reference_root_unknown")
        self.read_roots = [root_map[name] for name in permissions["read_roots"]]
        self.write_roots = [root_map[name] for name in permissions["write_roots"]]
        self.origins = permissions["network_origins"]
        profile_path = path_allowed(self.work / "profile.json", self.read_roots)
        profile = json.loads(profile_path.read_text(encoding="utf-8"))
        if set(profile) != {"source_id", "storage_location_id"}:
            raise fail("VALIDATION_FAILED", "reference_profile_invalid")
        self.source_id, self.storage_id = profile["source_id"], profile["storage_location_id"]
        contracts.request("source.auth_probe", {"source_id": self.source_id, "auth_mode": "NONE", "account_id": None, "credential_reference_id": None})
        contracts.request("source.download_plan", {"source_id": self.source_id, "variant_id": self.storage_id, "storage_location_id": self.storage_id, "hard_budget_bytes": None, "priority": "NORMAL"})
        self.support = self.config["search_support"]
        if self.support["search_applicable"] != ("source.search" in manifest["capabilities"]) or set(self.support["supported_filters"]) - {"source.search.q", "source.search.capabilities"}:
            raise fail("VALIDATION_FAILED", "reference_search_binding_invalid")
        self.catalog_path = self.work / "catalog/catalog.json"
        self.storage = path_allowed(self.work / "storage", self.write_roots)
        self.mutation = threading.Lock()
        bundle=json.loads(path_allowed(self.package / "reference-capabilities.json",self.read_roots).read_text(encoding="utf-8"))
        self.registry=CapabilityRegistry(bundle,contracts,{"reference.source.buffers.v1":memory},CAPABILITY_MAPPINGS)
        self.profile_hash=digest(profile)
        self.state=None

    def open_session(self):
        self.state=State(self.work,self.write_roots,self.contracts,self.profile_hash)

    def catalog(self):
        return Catalog(self.catalog_path, self.read_roots, self.contracts, self.source_id)

    def _auth(self):
        result = {"authenticated": False, "auth_required": False, "license_required": self.config["license_required"], "reason": "reference_public_fixture"}
        if self.config["probe_url"]:
            try:
                with open_public(self.config["probe_url"], self.origins, {"Accept-Encoding": "identity"}):
                    pass
            except DomainError as error:
                if error.data["code"] != "AUTH_REQUIRED":
                    raise
                result["auth_required"], result["reason"] = True, "source_auth_required"
        return result

    def _estimate(self, entry, *, reuse_existing=True):
        size = sum(int(f["size_bytes"]) for f in entry["inspection"]["artifacts"])
        existing = 0
        if reuse_existing:
            for artifact in entry["inspection"]["artifacts"]:
                target = path_allowed(self.storage / entry["revision"]["manifest_hash"] / relative(artifact["relative_path"]), self.read_roots, writing=True)
                if target.is_file() and target.stat().st_size == int(artifact["size_bytes"]) and file_hash(target) == artifact["sha256"]:
                    existing += int(artifact["size_bytes"])
        extra = size - existing
        margin = max(1 << 30, (extra + 19) // 20)
        free = shutil.disk_usage(path_allowed(self.storage, self.read_roots)).free
        return {"download_bytes": str(extra), "final_artifact_bytes": str(size), "installed_bytes": "0", "derived_cache_bytes": "0", "temporary_peak_bytes": "0", "existing_bytes": str(existing), "peak_extra_bytes": str(extra), "required_free_bytes": str(extra + margin), "upper_bound_bytes": str(extra), "confidence": "EXACT", "reason": "reference_manifest_exact_private_execution_only", "evidence_refs": ["reference:manifest:" + entry["revision"]["manifest_hash"]], "per_volume": [{"volume_key": volume_key(self.storage), "peak_extra_bytes": str(extra), "margin_bytes": str(margin), "other_reservations_bytes": "0", "usable_free_bytes": str(free), "eligible": False, "reason": "core_admission_P050_not_accredited_by_reference"}]}

    def _download_plan(self, request, catalog):
        acquisition = validate_source_acquisition(request)
        storage_id = request["storage_location_id"]
        # A bound streaming plan only names Core's destination. It gives this
        # worker no access to that directory; Core owns placement and admission.
        # Private legacy execution still needs its explicitly bound local root.
        if acquisition is None and storage_id != self.storage_id:
            raise fail("FORBIDDEN", "reference_storage_not_bound")
        if acquisition is not None:
            entry = copy.deepcopy(catalog.find(acquisition["revision"]))
            selected = select_source_acquisition(request, catalog.manifest(entry))
            entry["inspection"]["artifacts"] = selected
            paths = {item["relative_path"] for item in selected}
            entry["downloads"] = [item for item in entry["downloads"] if item["relative_path"] in paths]
        else:
            entry = next((entry for entry in catalog.entries if entry["model"]["id"] == request["variant_id"]), None)
            if entry is None:
                raise fail("MODEL_NOT_FOUND", "reference_variant_not_found")
            if request.get("optional_artifact_ids"):
                raise fail("UNSUPPORTED_CAPABILITY", "reference_optional_artifact_binding_owned_by_core")
        auth = self._auth()
        revision = {"source_id": self.source_id, "upstream_namespace": entry["upstream_namespace"], "upstream_model_id": entry["upstream_model_id"], "revision_key": entry["revision"]["revision_key"]}
        for item in entry["downloads"]:
            if origin(item["url"]) not in {origin(value) for value in self.origins}:
                raise fail("FORBIDDEN", "reference_origin_not_allowed")
        spec = {"variant_id": request["variant_id"], "storage_location_id": storage_id, "revision": revision, "artifacts": entry["inspection"]["artifacts"], "auth": auth, "resumable": all(item["etag"] is not None for item in entry["downloads"]), "priority": request["priority"], "hard_budget_bytes": request["hard_budget_bytes"]}
        binding = {"catalog_sha256": catalog.hash, "downloads": entry["downloads"], "manifest_hash": entry["revision"]["manifest_hash"], "scope": "reference_fixture_only"}
        import uuid
        # El plan de bloques declara todos los bytes del manifiesto. La caché
        # privada del adaptador no acredita ficheros ni checkpoints del Core.
        plan = {"plan_id": str(uuid.uuid7()), "operation": "source.download", "resource_ids": [request["variant_id"], storage_id], "storage_estimate": self._estimate(entry, reuse_existing=False), "steps": [{"phase": "DOWNLOAD", "adapter_id": self.manifest["id"], "request": source_acquisition_payload(request, spec) | {"reference_binding_v1": binding}}], "expires_at": stamp(datetime.now(timezone.utc) + timedelta(seconds=900)), "approval_required": False}
        plan["plan_hash"] = digest(plan)
        self.contracts.response("source.download_plan", plan)
        self.state.plan(plan, request, binding)
        return plan

    def _download_chunk(self, request):
        """Entrega un bloque bounded para el DownloadManager de Core.

        El plan y su binding siguen en el ledger privado del worker; Core conserva
        el job, el checkpoint durable y la verificación final del artefacto.
        """
        if request["approval_id"] is not None:
            raise fail("UNSUPPORTED_CAPABILITY", "reference_approval_owned_by_core")
        stored = self.state.get_plan(request["plan_id"])
        plan = stored["document"]
        if stored["hash"] != request["plan_hash"] or plan["plan_hash"] != request["plan_hash"]:
            raise fail("CONFLICT", "plan_hash_mismatch")
        if datetime.fromisoformat(plan["expires_at"].replace("Z", "+00:00")) <= datetime.now(timezone.utc):
            raise fail("INVALID_STATE_TRANSITION", "plan_expired")
        catalog = self.catalog()
        if catalog.hash != stored["binding"]["catalog_sha256"]:
            raise fail("CONFLICT", "source_snapshot_changed")
        spec = plan["steps"][0]["request"]["source_download_v1"]
        if spec["revision"] != request["revision"]:
            raise fail("CONFLICT", "source_revision_changed")
        artifact = next((item for item in spec["artifacts"] if item["relative_path"] == request["relative_path"]), None)
        download = next((item for item in stored["binding"]["downloads"] if item["relative_path"] == request["relative_path"]), None)
        if artifact is None or download is None:
            raise fail("VALIDATION_FAILED", "reference_artifact_not_planned")
        self._auth_gate(spec["auth"])
        self._auth_gate(self._auth())

        total = int(artifact["size_bytes"])
        offset, maximum = int(request["offset"]), int(request["max_bytes"])
        if offset > total or not 1 <= maximum <= (512 << 10):
            raise fail("VALIDATION_FAILED", "reference_chunk_bounds")
        headers = {"Accept-Encoding": "identity"}
        if offset:
            validator = request["validator"]
            if validator is None or validator["kind"] != "STRONG_ETAG":
                raise fail("UNSUPPORTED_CAPABILITY", "reference_resume_validator_required")
            headers.update({"Range": f"bytes={offset}-", "If-Range": validator["value"]})
        try:
            with open_public(download["url"], self.origins, headers, allow_unsatisfied_range=True) as response:
                if response.status == 416:
                    match = re.fullmatch(r"bytes \*/([0-9]+)", response.headers.get("Content-Range", ""))
                    digits = (match[1].lstrip("0") or "0") if match else ""
                    if not digits or len(digits) > 19 or int(digits) > 9223372036854775807:
                        raise fail("PROTOCOL_ERROR", "reference_content_range_mismatch")
                    if int(digits) != total or offset != total or artifact["sha256"] is None:
                        raise fail("STALE_REVISION", "source_resume_validator_changed")
                    etag = response.headers.get("ETag")
                    validator = {"kind":"STRONG_ETAG","value":etag} if etag and etag.startswith('"') and etag.endswith('"') else request["validator"]
                    return {"offset":str(offset),"data_base64":"","eof":True,"total_bytes":str(total),"validator":validator}
                if response.headers.get("Content-Encoding", "identity").lower() != "identity":
                    raise fail("PROTOCOL_ERROR", "reference_content_encoding")
                etag = response.headers.get("ETag")
                validator = {"kind": "STRONG_ETAG", "value": etag} if etag and etag.startswith('"') and etag.endswith('"') else None
                if offset:
                    if response.status == 200:
                        raise fail("STALE_REVISION", "source_resume_validator_changed")
                    expected = f"bytes {offset}-{total - 1}/{total}"
                    if response.status != 206 or response.headers.get("Content-Range") != expected:
                        raise fail("PROTOCOL_ERROR", "reference_content_range_mismatch")
                elif response.status != 200:
                    raise fail("PROTOCOL_ERROR", "reference_response_status")
                declared = response.headers.get("Content-Length")
                if declared is not None and (not declared.isdecimal() or int(declared) != total - offset):
                    raise fail("INTEGRITY_FAILED", "reference_content_length_mismatch")
                data = response.read(min(maximum, total - offset))
        except (OSError, ValueError):
            raise fail("UPSTREAM_UNAVAILABLE", "source_network_interrupted", retryable=True) from None

        end = offset + len(data)
        if not data and end < total:
            raise fail("UPSTREAM_UNAVAILABLE", "source_network_interrupted", retryable=True)
        return {
            "offset": str(offset),
            "data_base64": base64.b64encode(data).decode("ascii"),
            "eof": end == total,
            "total_bytes": str(total),
            "validator": validator,
        }

    def _download(self, request, ctx):
        if not self.config["allow_fixture_execution"]:
            raise fail("UNSUPPORTED_CAPABILITY", "reference_fixture_execution_disabled")
        if request["approval_id"] is not None:
            raise fail("UNSUPPORTED_CAPABILITY", "reference_approval_owned_by_core")
        while not self.mutation.acquire(timeout=0.05):
            ctx.check_cancelled()
        try:
            ctx.check_cancelled()
            stored = self.state.get_plan(request["plan_id"])
            plan = stored["document"]
            self.contracts.response("source.download_plan", plan)
            calculated = digest({key: value for key, value in plan.items() if key != "plan_hash"})
            if stored["hash"] != request["plan_hash"] or plan["plan_hash"] != calculated:
                raise fail("CONFLICT", "plan_hash_mismatch")
            if stored["outcome"]:
                if "error" in stored["outcome"]:
                    raise RpcError(-32000, stored["outcome"]["error"]["message"], stored["outcome"]["error"])
                return self.state.receipt(stored["outcome"]["completed_job_id"])
            if stored["job_id"]:
                raise fail("CONFLICT", "reference_job_incomplete")
            if datetime.fromisoformat(plan["expires_at"].replace("Z", "+00:00")) <= datetime.now(timezone.utc):
                raise fail("INVALID_STATE_TRANSITION", "plan_expired")
            catalog = self.catalog()
            if catalog.hash != stored["binding"]["catalog_sha256"] or plan["steps"][0]["request"]["reference_binding_v1"] != stored["binding"]:
                raise fail("CONFLICT", "source_snapshot_changed")
            spec = plan["steps"][0]["request"]["source_download_v1"]
            if spec["storage_location_id"] != self.storage_id:
                raise fail("FORBIDDEN", "reference_private_execution_storage_not_bound")
            self._auth_gate(spec["auth"])
            self._auth_gate(self._auth())
            total = sum(int(file["size_bytes"]) for file in spec["artifacts"])
            budget = int(spec["hard_budget_bytes"]) if spec["hard_budget_bytes"] is not None else total
            if total > budget:
                raise fail("STORAGE_INSUFFICIENT", "reference_budget_exceeded")
            if shutil.disk_usage(self.storage).free < total + max(1 << 30, (total + 19) // 20):
                raise fail("STORAGE_INSUFFICIENT", "reference_free_space_insufficient")
            identity = digest({"source_id": self.source_id, "variant_id": spec["variant_id"], "storage_location_id": self.storage_id, "manifest_hash": stored["binding"]["manifest_hash"]})
            job_id = self.state.accept(plan["plan_id"], identity, total)
            completed = 0
            try:
                for artifact, item in zip(spec["artifacts"], stored["binding"]["downloads"], strict=True):
                    ctx.check_cancelled()
                    name = relative(artifact["relative_path"])
                    target = self.storage / stored["binding"]["manifest_hash"] / name
                    partial = self.work / "staging" / job_id / (name + ".partial")
                    outer, ledger, base = ctx, self.state, completed
                    class TransferContext:
                        def check_cancelled(self):
                            outer.check_cancelled()

                        def progress(self, phase, offset, _size, code):
                            ledger.checkpoint(job_id, name, offset, base + offset)
                            outer.progress(phase, base + offset, total, code)
                    transfer(url=item["url"], target=target, partial=partial, size=int(artifact["size_bytes"]), sha256=artifact["sha256"], etag=item["etag"], read_roots=self.read_roots, write_roots=self.write_roots, origins=self.origins, ctx=TransferContext(), hard_budget=budget)
                    completed += int(artifact["size_bytes"])
                    self.state.checkpoint(job_id, name, int(artifact["size_bytes"]), completed)
                self.state.transition(job_id, "VERIFYING")
                for artifact in spec["artifacts"]:
                    ctx.check_cancelled()
                    target = path_allowed(self.storage / stored["binding"]["manifest_hash"] / relative(artifact["relative_path"]), self.read_roots)
                    if target.stat().st_size != int(artifact["size_bytes"]) or file_hash(target) != artifact["sha256"]:
                        raise fail("INTEGRITY_FAILED", "reference_final_verification_failed")
                self.state.transition(job_id, "COMPLETED", result={"manifest_hash": stored["binding"]["manifest_hash"], "artifacts": spec["artifacts"]})
                return self.state.receipt(job_id)
            except RpcError as error:
                cancelled = error.data and error.data["details"]["reason"] == "operation_cancelled"
                if cancelled:
                    self._cancel_partials(job_id, spec["artifacts"])
                self.state.transition(job_id, "CANCELLED" if cancelled else "FAILED", error=error.data or fail("RESOURCE_UNAVAILABLE", "reference_execution_failed").data)
                raise
            except (OSError, sqlite3.Error):
                # Un commit/outbox incierto conserva journal/locks hasta recuperar.
                raise fail("RESOURCE_UNAVAILABLE", "reference_execution_persistence_failed") from None
        finally:
            self.mutation.release()

    def _cancel_partials(self, job_id, artifacts):
        for artifact in artifacts:
            candidate = path_allowed(self.work / "staging" / job_id / (relative(artifact["relative_path"]) + ".partial"), self.write_roots, writing=True)
            if candidate.exists():
                candidate.unlink()  # sólo los paths del job actual, nunca barrido/recursión

    @staticmethod
    def _auth_gate(auth):
        if auth["auth_required"] and not auth["authenticated"]:
            raise fail("AUTH_REQUIRED", "source_auth_required")
        if auth["license_required"]:
            raise fail("LICENSE_REQUIRED", "source_license_required")

    def call(self, method, params, ctx):
        try:
            if params.get("source_id") is not None and params["source_id"] != self.source_id:
                raise fail("VALIDATION_FAILED", "source_id_mismatch")
            ctx.check_cancelled()
            capability=CAPABILITY_METHODS.get(method)
            if capability is not None:
                self.registry.estimate(capability,params)
            if method == "source.download":
                return self._download(params, ctx)
            if method == "source.auth_probe":
                if params["auth_mode"] != "NONE" or params["account_id"] is not None or params["credential_reference_id"] is not None:
                    raise fail("AUTH_REQUIRED", "reference_credentials_not_resolved")
                return self._auth()
            catalog = self.catalog()
            if method == "source.search":
                return catalog.search(params, self.support)
            if method == "source.download_plan":
                return self._download_plan(params, catalog)
            if method == "source.download_chunk":
                return self._download_chunk(params)
            if method == "source.sync":
                if params["cursor"] is not None or params["query"]:
                    raise fail("UNSUPPORTED_CAPABILITY", "reference_sync_query_unsupported")
                changed = params["etag"] != catalog.hash
                return {"cursor": None, "etag": catalog.hash, "changed": changed, "models": [copy.deepcopy(e["inspection"]) for e in catalog.entries] if changed else []}
            entry = catalog.find(params)
            if method == "source.get_model":
                return copy.deepcopy(entry["inspection"])
            if method == "source.list_revisions":
                return {"items": [copy.deepcopy(entry["revision"])], "next_cursor": None, "snapshot_at": catalog.snapshot}
            if method == "source.get_manifest":
                return catalog.manifest(entry)
            if method == "source.estimate_size":
                return self._estimate(entry)
            raise fail("UNSUPPORTED_CAPABILITY", "method_not_supported")
        except ContractViolation:
            raise fail("VALIDATION_FAILED", "reference_document_invalid") from None
        except (OSError, sqlite3.Error):
            raise fail("RESOURCE_UNAVAILABLE", "reference_io_failed") from None

    def shutdown(self):
        if self.state is not None:
            self.state.close()
            self.state=None
