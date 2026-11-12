"""Mocked public Hub contract tests for the P047 worker boundary."""
from __future__ import annotations

import base64
import hashlib
import json
import re
import sys
import tempfile
import unittest
import uuid
from email.message import Message
from pathlib import Path
from urllib.parse import parse_qs, unquote, urlsplit

REPO = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO / "adapters/sdk/python"))
sys.path.insert(0, str(REPO / "adapters/source/modelscope"))

from adapter import ModelScopeAdapter  # noqa: E402
from larm_adapter_sdk import ContractViolation, Contracts, RpcError  # noqa: E402


SOURCE_ID = "019b7b7d-4b2a-7a11-8ac1-73677a0c81a2"
STORAGE_ID = "019b7b7d-4b2a-7a11-8ac1-73677a0c81a3"
COMMIT = "a" * 40
CONTENT = b"0123456789"
SHA = hashlib.sha256(CONTENT).hexdigest()
ETAG = '"fixture-v1"'


class Context:
    def __init__(self, cancelled=False):
        self.cancelled = cancelled

    def check_cancelled(self):
        if self.cancelled:
            raise RuntimeError("cancelled")


class FixtureState:
    def __init__(self):
        self.files = [
            {"Path": "weights/model.safetensors", "Type": "blob", "Size": len(CONTENT), "Sha256": SHA},
            {"Path": "weights/config.json", "Type": "blob", "Size": 5},
            {"Path": "README.md", "Type": "blob"},
        ]
        self.revision_rows = [{"Revision": "main", "CommitId": COMMIT}]
        self.mode = "ok"
        self.requests = []
        self.bytes_sent = 0

    def info(self):
        return {"id": "acme/demo", "Name": "Demo", "ModelType": "llama", "Task": "text-generation",
                "Revision": "main", "CreatedAt": "2025-01-02T03:04:05Z", "LastModified": "2026-01-02T03:04:05Z",
                "License": "apache-2.0", "private": False, "gated": False,
                "config": {"num_hidden_layers": 2, "max_position_embeddings": 128},
                "modelscope_api_token": "fixture-token-must-not-leak", "vendor_extension": {"safe": True}}


class FixtureResponse:
    def __init__(self, status, headers=None, body=b""):
        self.status = self.code = status
        self.headers = Message()
        for key, value in (headers or {}).items():
            self.headers.add_header(key, str(value))
        self.body = body

    def read(self, size=-1):
        return self.body if size is None or size < 0 else self.body[:size]

    def close(self):
        return None


class FixtureOpener:
    """HTTP-shaped in-memory transport; no socket access is required."""
    def __init__(self, state):
        self.state = state

    @staticmethod
    def response(status, payload=None, headers=None):
        body = b"" if payload is None else json.dumps(payload, separators=(",", ":")).encode()
        return FixtureResponse(status, {"Content-Type": "application/json", "Content-Length": len(body), **(headers or {})}, body)

    def open(self, request, timeout=8):
        del timeout
        method = request.get_method()
        parsed = urlsplit(request.full_url)
        path = unquote(parsed.path)
        query = parse_qs(parsed.query)
        headers = {key.lower(): value for key, value in request.header_items()}
        self.state.requests.append((method, path, query, headers))
        if self.state.mode == "rate_limit" and path.endswith("/openapi/v1/models"):
            return self.response(429, {"Code": 429, "Message": "rate limited"}, {"Retry-After": "15"})
        if self.state.mode == "timeout" and path.endswith("/openapi/v1/models"):
            raise TimeoutError("fixture timeout")
        if self.state.mode == "io_failure" and path.endswith("/openapi/v1/models"):
            raise OSError("fixture transport failure")
        if self.state.mode == "unavailable" and path.endswith("/openapi/v1/models"):
            return self.response(503, {"Code": 503, "Message": "temporarily unavailable"})
        if path.endswith("/openapi/v1/models"):
            return self.response(200, {"Code": 200, "Data": {"models": [{"id": "acme/demo"}], "total_count": 1}})
        if path == "/openapi/v1/models/acme/demo":
            return self.response(200, {"Code": 200, "Data": self.state.info()})
        if path == "/api/v1/models/acme/demo/revisions":
            return self.response(200, {"Code": 200, "Data": {"RevisionMap": {"Branches": self.state.revision_rows, "Tags": []}}})
        if path == "/api/v1/models/acme/demo/repo/files":
            revision = query.get("Revision", [None])[0]
            if revision not in (COMMIT, "main"):
                return self.response(404, {"Code": 404})
            return self.response(200, {"Code": 200, "Data": self.state.files})
        if path == "/api/v1/models/acme/demo/repo":
            if self.state.mode == "redirect_evil":
                return FixtureResponse(302, {"Location": "https://evil.invalid/file?X-Amz-Signature=secret"})
            match = re.fullmatch(r"bytes=(\d+)-(\d+)", headers.get("range", ""))
            if not match:
                return self.response(400, {"Code": 400})
            start, end = map(int, match.groups())
            if start >= len(CONTENT):
                return FixtureResponse(416, {"Content-Range": f"bytes */{len(CONTENT)}"})
            data = CONTENT[start:min(end + 1, len(CONTENT))]
            self.state.bytes_sent += len(data)
            return FixtureResponse(206, {"Content-Length": len(data), "Content-Range": f"bytes {start}-{start + len(data) - 1}/{len(CONTENT)}", "ETag": ETAG, "Content-Encoding": "identity"}, data)
        return self.response(404, {"Code": 404})


class AdapterCase(unittest.TestCase):
    def setUp(self):
        target = REPO / "target"
        target.mkdir(exist_ok=True)
        self.temp = tempfile.TemporaryDirectory(dir=target)
        self.root = Path(self.temp.name)
        self.work = self.root / "work"
        self.work.mkdir()
        self.state = FixtureState()
        self.base_url = "http://127.0.0.1:43121"
        (self.work / "profile.json").write_text(json.dumps({"source_id": SOURCE_ID, "base_url": self.base_url}), encoding="utf-8")
        self.contracts = Contracts(REPO / "docs")
        self.manifest = self.contracts.manifest({
            "id": "modelscope.source", "version": "1.0.0", "family": "source", "protocol_version": "1.0", "publisher": "LARM",
            "executable": {"relative_path": "python", "args": []}, "platforms": ["windows"], "architectures": ["x86_64"],
            "methods": ["core.handshake", "core.cancel", "core.heartbeat", "source.search", "source.get_model", "source.list_revisions", "source.get_manifest", "source.estimate_size", "source.download_plan", "source.download", "source.download_chunk", "source.sync", "source.auth_probe"],
            "permissions": {"read_roots": ["package", "work"], "write_roots": ["work"], "network_origins": [self.base_url], "process_allowlist": [], "secret_refs": []},
            "capabilities": [], "package_sha256": "0" * 64, "signature": None, "installation_strategies": [],
        })
        self.adapter = ModelScopeAdapter(REPO / "adapters/source/modelscope", self.work, self.manifest, self.contracts)
        self.adapter.http.opener = FixtureOpener(self.state)
        self.adapter.open_session()

    def tearDown(self):
        self.adapter.shutdown()
        self.temp.cleanup()

    def call(self, method, params, ctx=None):
        return self.adapter.call(method, params, ctx or Context())

    def ref(self):
        return {"source_id": SOURCE_ID, "upstream_namespace": "acme", "upstream_model_id": "demo"}

    def revision_ref(self, bundle="transformers:safetensors"):
        return {**self.ref(), "revision_key": "commit:" + COMMIT + "#bundle=" + base64.urlsafe_b64encode(bundle.encode()).decode().rstrip("=")}

    def test_public_search_maps_metadata_files_and_stable_ids(self):
        request = {"source_id": SOURCE_ID, "q": "demo", "capabilities": [], "cursor": None, "limit": 10}
        first = self.call("source.search", request)
        again = self.call("source.search", request)
        self.assertEqual(len(first["items"]), 1)
        self.assertEqual(first["items"][0]["id"], again["items"][0]["id"])
        normalized = first["items"][0]["metadata"]["source_v1"]["normalized"]
        self.assertEqual(normalized["repository_id"], "acme/demo")
        self.assertEqual(normalized["source_type"], "MODELSCOPE")
        self.assertEqual(normalized["commit"], COMMIT)
        reference = normalized["catalog_reference"]
        manifest = self.call("source.get_manifest", reference)
        self.assertEqual(reference["source_id"], SOURCE_ID)
        self.assertEqual(manifest["revision_key"], reference["revision_key"])
        self.assertEqual(first["items"][0]["format"], "SAFETENSORS")
        self.assertEqual(first["items"][0]["metadata"]["source_v1"]["original"]["modelscope_api_token"], "[REDACTED]")

    def test_search_cursor_is_query_bound_and_paged(self):
        request = {"source_id": SOURCE_ID, "q": "demo", "capabilities": [], "cursor": None, "limit": 1}
        page = self.call("source.search", request)
        self.assertIsNone(page["next_cursor"])
        invalid = request | {"cursor": base64.urlsafe_b64encode(json.dumps({"query_hash": "x", "page": 2, "snapshot_at": page["snapshot_at"]}).encode()).decode().rstrip("=")}
        with self.assertRaises(RpcError):
            self.call("source.search", invalid)

    def test_model_inspection_maps_sha256_and_metadata(self):
        inspection = self.call("source.get_model", self.ref())
        self.assertEqual(inspection["upstream_identity"], "modelscope:model:acme/demo")
        model_file = next(item for item in inspection["artifacts"] if item["relative_path"].endswith("safetensors"))
        self.assertEqual(model_file["size_bytes"], str(len(CONTENT)))
        self.assertEqual(model_file["sha256"], SHA)
        self.assertEqual(inspection["capabilities"], ["text.generate"])
        self.assertEqual(inspection["license"], "apache-2.0")

    def test_manifest_is_stable_and_sizes_distinguish_exact_high_unknown(self):
        manifest = self.call("source.get_manifest", self.revision_ref())
        repeated = self.call("source.get_manifest", self.revision_ref())
        self.assertEqual(manifest["manifest_hash"], repeated["manifest_hash"])
        by_path = {item["relative_path"]: item for item in manifest["artifacts"]}
        self.assertEqual(by_path["weights/model.safetensors"]["size_estimate"]["confidence"], "EXACT")
        self.assertEqual(by_path["weights/config.json"]["size_estimate"]["confidence"], "HIGH")
        unknown_revision = self.revision_ref()
        self.state.files = [{"Path": "weights/model.safetensors", "Type": "blob"}]
        unknown = self.call("source.get_manifest", unknown_revision)
        self.assertEqual(unknown["artifacts"][0]["size_estimate"]["confidence"], "UNKNOWN")
        self.assertIsNone(unknown["artifacts"][0]["size_estimate"]["bytes"])

    def test_revision_mapping_keeps_commit_and_reference_name(self):
        page = self.call("source.list_revisions", self.ref())
        self.assertEqual(len(page["items"]), 1)
        row = page["items"][0]
        self.assertEqual(row["upstream_commit"], COMMIT)
        self.assertIn("#bundle=", row["revision_key"])
        self.assertEqual(row["metadata"]["source_v1"]["ref_name"], "main")

    def test_ref_without_commit_is_visible_but_cannot_be_downloaded(self):
        self.state.revision_rows = [{"Revision": "main"}]
        page = self.call("source.list_revisions", self.ref())
        self.assertIsNone(page["items"][0]["upstream_commit"])
        self.assertTrue(page["items"][0]["revision_key"].startswith("ref:"))
        result = self.call("source.search", {"source_id": SOURCE_ID, "q": "demo", "capabilities": [], "cursor": None, "limit": 1})
        self.assertFalse(result["items"][0]["metadata"]["source_v1"]["normalized"]["revision_pinned"])

    def test_download_plan_is_preflight_only_and_uses_exact_manifest(self):
        item = self.call("source.search", {"source_id": SOURCE_ID, "q": "demo", "capabilities": [], "cursor": None, "limit": 10})["items"][0]
        request = {"source_id": SOURCE_ID, "variant_id": item["id"], "storage_location_id": STORAGE_ID,
                   "hard_budget_bytes": None, "priority": "NORMAL"}
        plan = self.call("source.download_plan", request)
        self.assertEqual(plan["operation"], "source.download")
        self.assertEqual(plan["storage_estimate"]["confidence"], "HIGH")
        self.assertEqual(plan["steps"][0]["request"]["source_download_v1"]["revision"]["revision_key"].split("#")[0], "commit:" + COMMIT)
        self.assertFalse(any(path.exists() for path in self.work.glob("*.safetensors")))
        with self.assertRaises(RpcError):
            self.call("source.download", {"plan_id": str(uuid.uuid7()), "plan_hash": "0" * 64, "approval_id": None})

    def test_public_binding_uses_central_uuid_and_rejects_changed_manifest_without_transfer(self):
        item = self.call("source.search", {"source_id": SOURCE_ID, "q": "demo", "capabilities": [], "cursor": None, "limit": 10})["items"][0]
        revision = item["metadata"]["source_v1"]["normalized"]["catalog_reference"]
        manifest = self.call("source.get_manifest", revision)
        binding = {"revision": revision, "manifest_hash": manifest["manifest_hash"], "artifacts": [
            {"artifact_id": str(uuid.uuid7()), "relative_path": row["relative_path"], "required": row["required"]}
            for row in sorted(manifest["artifacts"], key=lambda row: row["relative_path"])]}
        central_id = str(uuid.uuid7())
        request = {"variant_id": central_id, "storage_location_id": STORAGE_ID, "source_id": SOURCE_ID,
                   "priority": "NORMAL", "hard_budget_bytes": None, "optional_artifact_ids": [], "acquisition": binding}
        plan = self.call("source.download_plan", request)
        self.assertNotEqual(central_id, item["id"])
        payload = plan["steps"][0]["request"]
        self.assertEqual(payload["source_acquisition_v1"], binding)
        self.assertEqual(payload["source_download_v1"]["variant_id"], central_id)
        self.assertEqual(payload["source_download_v1"]["revision"], revision)
        self.assertEqual(self.state.bytes_sent, 0)
        changed = json.loads(json.dumps(request))
        changed["acquisition"]["manifest_hash"] = "d" * 64
        with self.assertRaises(RpcError):
            self.call("source.download_plan", changed)
        self.assertEqual(self.state.bytes_sent, 0)

    def test_range_chunks_resume_with_strong_etag_and_map_hash(self):
        request = {"source_id": SOURCE_ID, "job_id": str(uuid.uuid7()), "plan_id": str(uuid.uuid7()), "plan_hash": "1" * 64,
                   "approval_id": None, "revision": self.revision_ref(), "relative_path": "weights/model.safetensors",
                   "offset": "0", "max_bytes": "5", "validator": None}
        first = self.call("source.download_chunk", request)
        self.assertEqual(base64.b64decode(first["data_base64"]), CONTENT[:5])
        self.assertFalse(first["eof"])
        second = self.call("source.download_chunk", request | {"offset": "5", "validator": first["validator"]})
        self.assertEqual(base64.b64decode(second["data_base64"]), CONTENT[5:])
        self.assertTrue(second["eof"])
        self.assertEqual(second["total_bytes"], str(len(CONTENT)))
        self.assertEqual(self.state.bytes_sent, len(CONTENT))
        complete = self.call("source.download_chunk", request | {"offset": str(len(CONTENT)), "validator": second["validator"]})
        self.assertTrue(complete["eof"])
        self.assertEqual(complete["data_base64"], "")

    def test_chunk_rejects_missing_validator_bad_path_and_unpinned_ref(self):
        base = {"source_id": SOURCE_ID, "job_id": str(uuid.uuid7()), "plan_id": str(uuid.uuid7()), "plan_hash": "1" * 64,
                "approval_id": None, "revision": self.revision_ref(), "relative_path": "weights/model.safetensors",
                "offset": "3", "max_bytes": "5", "validator": None}
        with self.assertRaises(RpcError):
            self.call("source.download_chunk", base)
        with self.assertRaises(RpcError):
            self.call("source.download_chunk", base | {"relative_path": "../escape.safetensors", "offset": "0"})
        unpinned = self.revision_ref() | {"revision_key": "ref:" + base64.urlsafe_b64encode(b"main").decode().rstrip("=")}
        with self.assertRaises(RpcError):
            self.call("source.download_chunk", base | {"revision": unpinned, "offset": "0"})

    def test_private_source_requires_auth_before_download_planning(self):
        page = self.call("source.search", {"source_id": SOURCE_ID, "q": "demo", "capabilities": [], "cursor": None, "limit": 10})
        self.assertEqual(len(page["items"]), 1)
        self.adapter.identities.distribution("acme/demo", True, False)
        with self.assertRaises(RpcError):
            self.call("source.download_plan", {"source_id": SOURCE_ID, "variant_id": page["items"][0]["id"], "storage_location_id": STORAGE_ID, "hard_budget_bytes": None, "priority": "NORMAL"})

    def test_gated_source_requires_license_before_manifest_access(self):
        self.adapter.identities.distribution("acme/demo", False, True)
        with self.assertRaises(RpcError) as captured:
            self.call("source.get_manifest", self.revision_ref())
        self.assertEqual(captured.exception.data["code"], "LICENSE_REQUIRED")

    def test_invalid_request_schema_is_rejected_by_sdk(self):
        with self.assertRaises(ContractViolation):
            self.contracts.request("source.search", {"source_id": SOURCE_ID, "q": "demo", "capabilities": [], "cursor": None, "limit": 1, "unexpected": True})

    def test_auth_probe_never_claims_unresolved_credentials(self):
        result = self.call("source.auth_probe", {"source_id": SOURCE_ID, "auth_mode": "TOKEN", "account_id": str(uuid.uuid7()), "credential_reference_id": str(uuid.uuid7())})
        self.assertFalse(result["authenticated"])
        self.assertTrue(result["auth_required"])

    def test_rate_limit_is_retryable_and_diagnostic_is_redacted(self):
        self.state.mode = "rate_limit"
        with self.assertRaises(RpcError) as captured:
            self.call("source.search", {"source_id": SOURCE_ID, "q": "demo", "capabilities": [], "cursor": None, "limit": 10})
        self.assertEqual(captured.exception.data["code"], "UPSTREAM_UNAVAILABLE")
        self.assertTrue(captured.exception.data["retryable"])
        self.assertNotIn("token", json.dumps(captured.exception.data).lower())

    def test_timeout_and_io_failures_are_retryable_and_recover(self):
        request = {"source_id": SOURCE_ID, "q": "demo", "capabilities": [], "cursor": None, "limit": 1}
        for mode in ("timeout", "io_failure", "unavailable"):
            self.state.mode = mode
            with self.subTest(mode=mode), self.assertRaises(RpcError) as captured:
                self.call("source.search", request)
            self.assertEqual(captured.exception.data["code"], "UPSTREAM_UNAVAILABLE")
            self.assertTrue(captured.exception.data["retryable"])
        self.state.mode = "ok"
        self.assertEqual(len(self.call("source.search", request)["items"]), 1)

    def test_malformed_source_identity_and_unknown_method_fail_closed(self):
        with self.assertRaises(RpcError):
            self.call("source.get_model", self.ref() | {"source_id": str(uuid.uuid7())})
        with self.assertRaises(RpcError):
            self.call("source.not_real", {"source_id": SOURCE_ID})

    def test_cancel_is_checked_before_any_request(self):
        with self.assertRaises(RuntimeError):
            self.call("source.search", {"source_id": SOURCE_ID, "q": "demo", "capabilities": [], "cursor": None, "limit": 1}, Context(cancelled=True))
        self.assertEqual(self.state.requests, [])

    def test_network_origin_redirect_is_blocked_without_exposing_signed_url(self):
        self.state.mode = "redirect_evil"
        request = {"source_id": SOURCE_ID, "job_id": str(uuid.uuid7()), "plan_id": str(uuid.uuid7()), "plan_hash": "1" * 64,
                   "approval_id": None, "revision": self.revision_ref(), "relative_path": "weights/model.safetensors",
                   "offset": "0", "max_bytes": "5", "validator": None}
        with self.assertRaises(RpcError) as captured:
            self.call("source.download_chunk", request)
        self.assertEqual(captured.exception.data["code"], "FORBIDDEN")
        self.assertNotIn("X-Amz-Signature", json.dumps(captured.exception.data))


if __name__ == "__main__":
    unittest.main()
