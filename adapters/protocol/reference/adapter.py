"""Referencia protocol completa para echo JSON real; sin inferencia ni jobs Core."""
import json
import threading
import time
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path

from larm_adapter_sdk import CapabilityRegistry, DomainError
from larm_adapter_sdk.access import NoRedirect, origin, path_allowed
from larm_adapter_sdk.capabilities import canonical, digest

CAPABILITY = "vendor.reference.echo"
MAPPING = "reference.http.echo.v1"
ESTIMATOR = "reference.echo.buffers.v1"


def fail(code, reason, *, retryable=False):
    return DomainError(code, "Operación de protocolo de referencia no disponible", reason, retryable=retryable)


def buffers(value):
    # Bytes de buffers acotados por política; memoria total del proceso desconocida.
    result = {key:None for key in ("weights_bytes","kv_bytes","activation_bytes","workspace_bytes","overhead_bytes","total_bytes","multimodal_bytes","temporary_bytes","safety_margin_bytes")}
    result.update(workspace_bytes=str(len(canonical(value)) + (32 << 10)), per_pool=[], confidence="UNKNOWN", evidence_refs=["reference:bounded-http-buffers:process-memory-unknown"])
    return result


def unique(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError("Duplicate key")
        result[key] = value
    return result


def nonfinite(_value):
    raise ValueError("Non-finite JSON")


class Adapter:
    def __init__(self, package, work, manifest, contracts):
        raw_package, raw_work = Path(package).absolute(), Path(work).absolute()
        self.package = path_allowed(raw_package, [raw_package])
        self.work = path_allowed(raw_work, [raw_work])
        self.manifest, self.contracts = manifest, contracts
        roots = {"reference-package":self.package,"reference-protocol-work":self.work}
        permissions = manifest["permissions"]
        if set(permissions["read_roots"]) != set(roots) or permissions["write_roots"] or permissions["process_allowlist"] or permissions["secret_refs"]:
            raise fail("FORBIDDEN", "reference_protocol_permissions_invalid")
        self.read_roots = list(roots.values())
        self.policy = self.read_json(self.package / "reference-protocol.json")
        if set(self.policy) != {"base_url","mapping","max_request_bytes","max_response_bytes","timeout_seconds"} or self.policy["mapping"] != MAPPING or self.policy["max_request_bytes"] != 16 << 10 or self.policy["max_response_bytes"] != 32 << 10 or self.policy["timeout_seconds"] != 2:
            raise fail("VALIDATION_FAILED", "reference_protocol_policy_invalid")
        self.base = self.policy["base_url"]
        parsed = urllib.parse.urlsplit(self.base)
        if parsed.scheme != "http" or parsed.hostname not in ("127.0.0.1", "::1") or parsed.path or parsed.query or parsed.fragment or parsed.username is not None or parsed.password is not None or not parsed.port:
            raise fail("FORBIDDEN", "reference_protocol_loopback_required")
        if len(permissions["network_origins"]) != 1 or origin(permissions["network_origins"][0]) != origin(self.base):
            raise fail("FORBIDDEN", "reference_protocol_origin_not_granted")
        self.profile = self.read_json(self.work / "profile.json")
        if set(self.profile) != {"endpoint_id","variant_id","lease_id"}:
            raise fail("VALIDATION_FAILED", "reference_protocol_profile_invalid")
        contracts.request("protocol.describe", {"endpoint_id":self.profile["endpoint_id"]})
        contracts.request("protocol.qualify", {"endpoint_id":self.profile["endpoint_id"],"variant_id":self.profile["variant_id"],"suite_id":MAPPING,"fixture_hash":"a"*64})
        contracts.request("protocol.invoke", {"lease_id":self.profile["lease_id"],"request_id":self.profile["endpoint_id"],"capability":CAPABILITY,"input":{},"generation":None,"stream":False})
        bundle = self.read_json(self.package / "reference-capabilities.json")
        self.registry = CapabilityRegistry(bundle, contracts, {ESTIMATOR:buffers}, {MAPPING})
        if set(manifest["capabilities"]) != {CAPABILITY}:
            raise fail("UNSUPPORTED_CAPABILITY", "reference_protocol_capability_not_supported")
        self.profile_hash = digest({"policy":self.policy,"profile":self.profile,"registry_hash":self.registry.profile_hash})
        self.active, self.lock = {}, threading.Lock()

    def read_json(self, path):
        with path_allowed(path, self.read_roots).open("rb") as source:
            raw = source.read((1 << 20) + 1)
        if len(raw) > 1 << 20:
            raise fail("VALIDATION_FAILED", "reference_protocol_document_too_large")
        return json.loads(raw.decode("utf-8"), object_pairs_hook=unique, parse_constant=nonfinite)

    def fingerprint(self, observation=None):
        def component(value, status, evidence):
            return {"value":value,"status":status,"evidence_refs":[evidence]}
        components = {"scope":component("controlled_reference_http_transport","DECLARED","reference:no-inference-qualification"),"profile":component(self.profile_hash,"VERIFIED","reference:profile-rfc8785"),"adapter_package":component(self.manifest["package_sha256"],"DECLARED","reference:host-owns-package-verification"),"endpoint":component(self.base,"DECLARED","reference:fixed-loopback-origin"),"backend_identity":component(None,"UNAVAILABLE","reference:backend-binary-not-observed"),"observation":component(digest(observation) if observation is not None else None,"VERIFIED" if observation is not None else "UNAVAILABLE","reference:actual-http-response")}
        return {"hash":digest(components),"components":components}

    def http(self, method, path, payload, ctx):
        ctx.check_cancelled()
        body = canonical(payload) if payload is not None else None
        if body is not None and len(body) > self.policy["max_request_bytes"]:
            raise fail("RESOURCE_UNAVAILABLE", "reference_protocol_request_budget_exceeded")
        url = self.base + path
        if origin(url) != origin(self.base):
            raise fail("FORBIDDEN", "reference_protocol_origin_not_granted")
        request = urllib.request.Request(url, data=body, method=method, headers={"Content-Type":"application/json","Accept-Encoding":"identity"})
        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), NoRedirect())
        ctx.progress("PROTOCOL_HTTP", 0, None, "reference_request_started")
        try:
            with opener.open(request, timeout=self.policy["timeout_seconds"]) as response:
                ctx.check_cancelled()
                if response.status != 200 or response.headers.get_content_type() != "application/json":
                    raise fail("PROTOCOL_ERROR", "reference_protocol_http_reply_invalid")
                length = response.headers.get("Content-Length")
                if length is not None and (not length.isascii() or not length.isdigit() or len(length) > 5 or int(length) > self.policy["max_response_bytes"]):
                    raise fail("RESOURCE_UNAVAILABLE", "reference_protocol_response_budget_exceeded")
                raw = bytearray()
                while True:
                    ctx.check_cancelled()
                    chunk = response.read(min(1024, self.policy["max_response_bytes"] + 1 - len(raw)))
                    ctx.check_cancelled()
                    if not chunk:
                        break
                    raw.extend(chunk)
                    if len(raw) > self.policy["max_response_bytes"]:
                        raise fail("RESOURCE_UNAVAILABLE", "reference_protocol_response_budget_exceeded")
                if length is not None and len(raw) != int(length):
                    raise fail("PROTOCOL_ERROR", "reference_protocol_reply_length_invalid")
                try:
                    value = json.loads(raw.decode("utf-8"), object_pairs_hook=unique, parse_constant=nonfinite)
                except (ValueError, UnicodeError, RecursionError):
                    raise fail("PROTOCOL_ERROR", "reference_protocol_json_invalid") from None
                if not isinstance(value, dict):
                    raise fail("PROTOCOL_ERROR", "reference_protocol_json_invalid")
                return value
        except urllib.error.HTTPError as error:
            ctx.check_cancelled()
            error.close()
            if error.code in (401,403):
                raise fail("AUTH_REQUIRED", "reference_protocol_auth_required") from None
            if 300 <= error.code < 400:
                raise fail("FORBIDDEN", "reference_protocol_redirect_denied") from None
            if error.code == 429:
                retry = error.headers.get("Retry-After")
                retry = retry if retry and retry.isascii() and retry.isdigit() and len(retry) <= 18 else None
                result = fail("UPSTREAM_UNAVAILABLE", "reference_protocol_rate_limited", retryable=True)
                result.data["details"]["original_error"] = {"system":"protocol","code":"429","message":json.dumps({"retry_after_seconds":retry})}
                raise result from None
            raise fail("UPSTREAM_UNAVAILABLE", "reference_protocol_http_error", retryable=error.code >= 500 or error.code == 429) from None
        except (OSError, urllib.error.URLError):
            ctx.check_cancelled()
            raise fail("UPSTREAM_UNAVAILABLE", "reference_protocol_network_failed", retryable=True) from None

    def call(self, method, params, ctx):
        ctx.check_cancelled()
        if method in ("protocol.describe", "protocol.qualify") and params["endpoint_id"] != self.profile["endpoint_id"]:
            raise fail("FORBIDDEN", "reference_protocol_endpoint_not_bound")
        if method == "protocol.describe":
            return {"protocol":MAPPING,"profile_hash":self.profile_hash,"capabilities":[CAPABILITY],"streaming":False,"tools":False,"request_schema_id":self.registry.schema_id(CAPABILITY,"input"),"response_schema_id":self.registry.schema_id(CAPABILITY,"output")}
        if method == "protocol.cancel":
            with self.lock:
                event = self.active.get(params["request_id"])
                if event:
                    event.set()
            return {"cancelled":event is not None}
        if method == "protocol.qualify":
            entry = self.registry.entry(CAPABILITY)
            if params["variant_id"] != self.profile["variant_id"] or params["fixture_hash"] != entry["fixture_sha256"] or params["suite_id"] != MAPPING:
                raise fail("VALIDATION_FAILED", "reference_protocol_fixture_not_bound")
            tests, observation = [], None
            try:
                self.registry.estimate(CAPABILITY, entry["fixture"]["input"])
                tests.append({"id":"reference.resource-estimator","status":"PASS","reason":"buffers_bounded_process_memory_unknown","evidence_refs":["reference:memory-admission-owned-by-core"]})
                health = self.http("GET", "/health", None, ctx)
                if health.get("status") != "ok":
                    raise fail("PROTOCOL_ERROR", "reference_protocol_health_invalid")
                observation = self.http("POST", "/echo", entry["fixture"]["input"], ctx)
                self.registry.validate(CAPABILITY, "output", observation)
                if observation != entry["fixture"]["output"]:
                    raise fail("INTEGRITY_FAILED", "reference_protocol_fixture_output_mismatch")
                tests.append({"id":"reference.http-fixture","status":"PASS","reason":"actual_http_echo_matches_pinned_fixture","evidence_refs":["sha256:"+entry["fixture_sha256"]]})
            except DomainError as error:
                ctx.check_cancelled()
                tests.append({"id":"reference.http-fixture","status":"FAIL","reason":error.data["details"]["reason"],"evidence_refs":[]})
            return {"qualified":all(t["status"]=="PASS" for t in tests),"tests":tests,"fingerprint":self.fingerprint(observation)}
        if method != "protocol.invoke":
            raise fail("UNSUPPORTED_CAPABILITY", "method_not_announced")
        if params["lease_id"] != self.profile["lease_id"]:
            raise fail("FORBIDDEN", "reference_protocol_lease_not_bound")
        if params["stream"] or params["generation"] is not None:
            raise fail("UNSUPPORTED_CAPABILITY", "reference_protocol_options_not_supported")
        entry = self.registry.entry(params["capability"])
        if entry["id"] != CAPABILITY:
            raise fail("UNSUPPORTED_CAPABILITY", "reference_protocol_capability_not_supported")
        self.registry.estimate(CAPABILITY, params["input"])
        event = threading.Event()
        with self.lock:
            if params["request_id"] in self.active:
                raise fail("CONFLICT", "reference_protocol_request_active")
            self.active[params["request_id"]] = event
        outer = ctx
        class Invocation:
            def check_cancelled(self):
                outer.check_cancelled()
                if event.is_set():
                    raise fail("RESOURCE_UNAVAILABLE", "operation_cancelled")
            def progress(self, *args):
                outer.progress(*args)
        try:
            start = time.monotonic_ns()
            output = self.http("POST", "/echo", params["input"], Invocation())
            self.registry.validate(CAPABILITY, "output", output)
            return {"request_id":params["request_id"],"capability":CAPABILITY,"output":output,"fingerprint":self.fingerprint(output),"usage":{"tokens_input":None,"tokens_output":None,"latency_ms":(time.monotonic_ns()-start)/1_000_000}}
        finally:
            with self.lock:
                self.active.pop(params["request_id"], None)

    def shutdown(self):
        with self.lock:
            for event in self.active.values():
                event.set()
