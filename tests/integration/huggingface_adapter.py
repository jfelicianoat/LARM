"""Mocked Hub integration tests for the P046 worker boundary."""
from __future__ import annotations

import base64
import hashlib
import json
import re
import sys
import tempfile
import threading
import unittest
import uuid
from email.message import Message
from pathlib import Path
from urllib.parse import parse_qs, unquote, urlsplit

REPO = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO / "adapters/sdk/python"))
sys.path.insert(0, str(REPO / "adapters/source/huggingface"))

from adapter import HuggingFaceAdapter  # noqa: E402
from larm_adapter_sdk import Contracts, DomainError  # noqa: E402


COMMIT = "a" * 40
COMMIT_2 = "b" * 40
SOURCE_ID = "019b7b7d-4b2a-7a11-8ac1-73677a0c81a2"
ETAG = '"fixture-etag-v1"'
CONTENT = b"0123456789"


class Context:
    def check_cancelled(self):
        return None


class FixtureState:
    def __init__(self):
        self.mode = "exact"
        self.search_status = None
        self.changed_revision = False
        self.pagination = False
        self.resolve_count = 0
        self.resolve_bytes_sent = 0
        self.head_count = 0
        self.requests = []
        self.lock = threading.Lock()

    def record(self, method, path, headers):
        with self.lock:
            self.requests.append((method, path, dict(headers)))

    def info(self, repo="acme/demo", revision=None):
        if self.changed_revision and revision:
            sha = COMMIT_2
        else:
            sha = revision or COMMIT
        if self.mode in ("unknown", "unknown_no_head"):
            sibling = {"rfilename": "model.safetensors"}
        elif self.mode == "high":
            sibling = {"rfilename": "model.safetensors", "size": len(CONTENT)}
        elif self.mode == "high_lfs":
            sibling = {"rfilename": "model.safetensors", "lfs": {"size": len(CONTENT), "oid": "sha1:" + "c" * 40}}
        else:
            sibling = {"rfilename": "model.safetensors", "size": len(CONTENT), "lfs": {"size": len(CONTENT), "sha256": hashlib.sha256(CONTENT).hexdigest()}}
        gated = self.mode == "gated"
        private = self.mode == "private"
        if self.mode == "gguf":
            siblings = [
                {"rfilename": "weights/model-Q4_K_M.gguf", "size": len(CONTENT)},
                {"rfilename": "weights/mmproj-model-f16.gguf", "size": 2},
                {"rfilename": "weights/model-Q5_K_M.gguf", "size": len(CONTENT)},
            ]
        elif self.mode == "gguf_pair":
            siblings = [
                {"rfilename": "weights/model-Q4_K_M.gguf", "size": len(CONTENT)},
                {"rfilename": "weights/mmproj-model-f16.gguf", "size": 2},
            ]
        elif self.mode == "transformers":
            siblings = [
                sibling,
                {"rfilename": "config.json", "size": 3},
                {"rfilename": "tokenizer.json", "size": 4},
                {"rfilename": "README.md", "size": 5},
            ]
        elif self.mode == "mixed_dirs":
            siblings = [
                {"rfilename": "a/model.safetensors", "size": 3},
                {"rfilename": "a/config.json", "size": 1},
                {"rfilename": "b/model.safetensors", "size": 4},
                {"rfilename": "b/config.json", "size": 1},
                {"rfilename": "pt/pytorch_model.bin", "size": 5},
                {"rfilename": "pt/config.json", "size": 1},
            ]
        else:
            siblings = [sibling]
        return {
            "id": repo, "sha": sha, "private": private, "gated": "manual" if gated else False,
            "pipeline_tag": "text-generation", "downloads": 7, "likes": 2,
            "createdAt": "2025-01-02T03:04:05Z", "lastModified": "2026-01-02T03:04:05Z",
            "cardData": {"license": "apache-2.0"},
            "config": {"model_type": "llama", "num_hidden_layers": 2, "max_position_embeddings": 128},
            "safetensors": {"total": 1234}, "vendor_extension": {"safe": True},
            "api_token": "fixture-token-must-not-leak",
            "download_url": "https://cdn.example.test/file?X-Amz-Signature=fixture-signature",
            "siblings": siblings,
        }


class FixtureResponse:
    def __init__(self, status, headers=None, body=b""):
        self.status = self.code = status
        self.headers = Message()
        for key, value in (headers or {}).items():
            self.headers.add_header(key, value)
        self.body = body

    def read(self, size=-1):
        if size is None or size < 0:
            return self.body
        return self.body[:size]

    def close(self):
        return None


class FixtureOpener:
    """HTTP-shaped in-memory transport; no socket access is required."""

    def __init__(self, state):
        self.state = state

    @staticmethod
    def _json(status, payload, headers=None):
        body = json.dumps(payload, ensure_ascii=False, separators=(",", ":")).encode("utf-8")
        response_headers = {"Content-Type": "application/json", "Content-Length": str(len(body)), **(headers or {})}
        return FixtureResponse(status, response_headers, body)

    @staticmethod
    def _empty(status, headers=None):
        return FixtureResponse(status, {"Content-Length": "0", **(headers or {})})

    def open(self, request, timeout=8):
        del timeout
        method = request.get_method()
        parsed = urlsplit(request.full_url)
        path = unquote(parsed.path)
        headers = {key.lower(): value for key, value in request.header_items()}
        self.state.record(method, path, headers)
        if path == "/api/models":
            if self.state.search_status:
                if self.state.search_status == 429:
                    return self._json(429, {"error": "rate limited"}, {
                        "RateLimit": '"api";r=0;t=20', "RateLimit-Policy": "api;q=100;w=300", "Retry-After": "20",
                    })
                return self._json(self.state.search_status, {"error": "rejected"})
            query = parse_qs(parsed.query)
            if query.get("cursor") == ["page2"]:
                return self._json(200, [self.state.info("acme/second")])
            response_headers = {}
            if self.state.pagination:
                response_headers["Link"] = '</api/models?limit=1&full=true&config=true&sort=downloads&search=demo&cursor=page2>; rel="next"'
            return self._json(200, [self.state.info()], response_headers)
        if path == "/api/models/acme/demo/refs":
            return self._json(200, {
                "branches": [{"name": "main", "targetCommit": COMMIT}],
                "tags": [{"name": "v1", "targetCommit": COMMIT_2}],
            })
        match = re.fullmatch(r"/api/models/(acme/demo)(?:/revision/([0-9a-f]{40,64}))?", path)
        if match:
            return self._json(200, self.state.info(match.group(1), match.group(2)))
        if path.startswith("/acme/demo/resolve/"):
            if method == "HEAD":
                self.state.head_count += 1
                if self.state.mode == "unknown_no_head":
                    return FixtureResponse(200, {"ETag": ETAG})
                return self._empty(200, {"Content-Length": str(len(CONTENT)), "ETag": ETAG})
            self.state.resolve_count += 1
            if self.state.mode == "evil_redirect":
                return self._empty(302, {"Location": "https://evil.invalid/file?X-Amz-Signature=fixture"})
            match = re.fullmatch(r"bytes=(\d+)-(\d+)", headers.get("range", ""))
            if not match:
                return self._json(400, {"error": "range required"})
            start, end = map(int, match.groups())
            if start >= len(CONTENT):
                return self._empty(416, {"Content-Range": f"bytes */{len(CONTENT)}"})
            if headers.get("if-range") not in (None, ETAG):
                self.state.resolve_bytes_sent += len(CONTENT)
                return FixtureResponse(200, {"Content-Length": str(len(CONTENT)), "ETag": ETAG}, CONTENT)
            payload = CONTENT[start:min(end + 1, len(CONTENT))]
            self.state.resolve_bytes_sent += len(payload)
            return FixtureResponse(206, {
                "Content-Length": str(len(payload)),
                "Content-Range": f"bytes {start}-{start + len(payload) - 1}/{len(CONTENT)}",
                "ETag": ETAG, "Content-Encoding": "identity",
            }, payload)
        return self._json(404, {"error": "not found"})


class AdapterCase(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(dir=REPO / "target")
        self.root = Path(self.temp.name)
        self.work = self.root / "work"
        self.work.mkdir()
        self.state = FixtureState()
        self.base_url = "https://huggingface.test"
        (self.work / "profile.json").write_text(json.dumps({"source_id": SOURCE_ID, "base_url": self.base_url}), encoding="utf-8")
        self.contracts = Contracts(REPO / "docs")
        self.manifest = self.contracts.manifest({
            "id": "huggingface.source", "version": "1.0.0", "family": "source",
            "protocol_version": "1.0", "publisher": "LARM",
            "executable": {"relative_path": "python", "args": []},
            "platforms": ["windows"], "architectures": ["x86_64"],
            "methods": ["core.handshake", "core.cancel", "core.heartbeat", "source.search", "source.get_model", "source.list_revisions", "source.get_manifest", "source.estimate_size", "source.download_plan", "source.download", "source.download_chunk", "source.sync", "source.auth_probe"],
            "permissions": {"read_roots": ["package", "work"], "write_roots": ["work"], "network_origins": [self.base_url], "process_allowlist": [], "secret_refs": []},
            "capabilities": [], "package_sha256": "0" * 64, "signature": None, "installation_strategies": [],
        })
        self.adapter = HuggingFaceAdapter(REPO / "adapters/source/huggingface", self.work, self.manifest, self.contracts)
        self.adapter.http.opener = FixtureOpener(self.state)
        self.adapter.open_session()

    def tearDown(self):
        self.adapter.shutdown()
        self.temp.cleanup()

    def invoke(self, method, params):
        self.contracts.request(method, params)
        result = self.adapter.call(method, params, Context())
        self.contracts.response(method, result)
        return result

    def search(self, q="demo", cursor=None, limit=1):
        return self.invoke("source.search", {"source_id": SOURCE_ID, "q": q, "capabilities": [], "cursor": cursor, "limit": limit})

    def revision_ref(self, variant=None):
        if variant is None:
            variant = self.search()["items"][0]
        reference = dict(variant["metadata"]["source_v1"]["normalized"]["catalog_reference"])
        self.assertEqual(reference["source_id"], SOURCE_ID)
        self.assertEqual(reference["revision_key"], variant["metadata"]["source_v1"]["normalized"]["revision_key"])
        return reference

    def chunk_request(self, ref, offset, maximum=4, validator=None):
        return {
            "source_id": SOURCE_ID, "job_id": str(uuid.uuid7()), "plan_id": str(uuid.uuid7()),
            "plan_hash": "d" * 64, "approval_id": None, "revision": ref,
            "relative_path": "model.safetensors", "offset": str(offset), "max_bytes": str(maximum),
            "validator": validator,
        }

    def test_search_paginates_and_preserves_redacted_upstream_metadata(self):
        self.state.pagination = True
        first = self.search()
        self.assertEqual(first["items"][0]["ownership"], "EXTERNAL")
        self.assertEqual(first["items"][0]["metadata"]["source_v1"]["original"]["api_token"], "[REDACTED]")
        self.assertEqual(first["items"][0]["metadata"]["source_v1"]["original"]["download_url"], "[REDACTED]")
        cursor = first["next_cursor"]
        self.assertIsInstance(cursor, str)
        second = self.search(cursor=cursor)
        self.assertEqual(second["items"][0]["metadata"]["display_name"], "acme/second")
        with self.assertRaises(DomainError) as mismatch:
            self.search(q="other", cursor=cursor)
        self.assertEqual(mismatch.exception.data["details"]["reason"], "hf_cursor_query_mismatch")

    def test_search_rejects_unsupported_capabilities_before_network(self):
        before = len(self.state.requests)
        with self.assertRaises(DomainError) as rejected:
            self.invoke("source.search", {"source_id": SOURCE_ID, "q": "demo", "capabilities": ["text.generate"], "cursor": None, "limit": 1})
        self.assertEqual(rejected.exception.data["code"], "UNSUPPORTED_CAPABILITY")
        self.assertEqual(len(self.state.requests), before)

    def test_source_profile_keeps_live_qualification_explicit(self):
        bundle = json.loads((REPO / "docs/contracts/source-profiles.json").read_text(encoding="utf-8"))
        profile = next(item for item in bundle["profiles"] if item["source_type"] == "HUGGING_FACE")
        self.assertEqual(profile["owner"], "P046")
        self.assertEqual(profile["status"], "REQUIRES_REAL_QUALIFICATION")
        self.assertIn("source.download_chunk", profile["methods"])
        self.assertTrue({
            "api_version_or_sdk_lock", "base_url_or_root", "revision_mapping", "search_mapping",
            "pagination_mapping", "manifest_mapping", "size_confidence_mapping", "checksum_mapping",
            "auth_flow_mapping", "rate_limits", "license_flow", "fixture_hashes", "error_mapping",
            "primary_source_revision",
        }.issubset(profile["required_profile_fields"]))

    def test_get_model_and_list_revisions_use_pinned_bundle_identities(self):
        self.state.mode = "transformers"
        variant = self.search()["items"][0]
        inspection = self.invoke("source.get_model", {"source_id": SOURCE_ID, "upstream_namespace": "acme", "upstream_model_id": "demo"})
        self.assertEqual(inspection["format"], "SAFETENSORS")
        self.assertTrue(any(a["relative_path"] == "config.json" for a in inspection["artifacts"]))
        self.assertNotIn("siblings", inspection["limits"]["source_v1"]["raw"])
        page = self.invoke("source.list_revisions", {"source_id": SOURCE_ID, "upstream_namespace": "acme", "upstream_model_id": "demo"})
        self.assertEqual(len(page["items"]), 2)
        kinds = {item["metadata"]["source_v1"]["ref_names"][0]["kind"] for item in page["items"]}
        self.assertEqual(kinds, {"branch", "tag"})
        self.assertEqual(len(variant["metadata"]["source_v1"]["normalized"]["repository_id"]), 9)

    def test_bundle_detection_separates_quantized_gguf(self):
        self.state.mode = "gguf"
        variants = self.search()["items"]
        self.assertEqual(len(variants), 2)
        self.assertEqual({item["quantization"] for item in variants}, {"Q4_K_M", "Q5_K_M"})

    def test_bundle_detection_pairs_single_gguf_projector(self):
        self.state.mode = "gguf_pair"
        paired = self.search(q="pair")["items"]
        self.assertEqual(len(paired), 1)
        ref = self.revision_ref(paired[0])
        manifest = self.invoke("source.get_manifest", ref)
        self.assertEqual({item["relative_path"] for item in manifest["artifacts"]}, {"weights/model-Q4_K_M.gguf", "weights/mmproj-model-f16.gguf"})

    def test_transformer_weight_directories_remain_separate_bundles(self):
        self.state.mode = "mixed_dirs"
        variants = self.search()["items"]
        self.assertEqual(len(variants), 3)
        self.assertEqual({item["format"] for item in variants}, {"SAFETENSORS", "PYTORCH"})
        keys = {item["metadata"]["source_v1"]["normalized"]["bundle_key"] for item in variants}
        self.assertEqual(keys, {"transformers:safetensors:a", "transformers:safetensors:b", "transformers:pytorch"})

    def test_exact_size_requires_pinned_sha256_and_size(self):
        exact_ref = self.revision_ref(self.search()["items"][0])
        exact_manifest = self.invoke("source.get_manifest", exact_ref)
        self.assertEqual(exact_manifest["artifacts"][0]["size_estimate"]["confidence"], "EXACT")
        self.assertEqual(self.invoke("source.estimate_size", exact_ref)["confidence"], "EXACT")

    def test_lfs_size_without_sha256_is_high_not_exact(self):
        self.state.mode = "high_lfs"
        high_ref = self.revision_ref(self.search(q="high")["items"][0])
        high_item = self.invoke("source.get_manifest", high_ref)["artifacts"][0]["size_estimate"]
        self.assertEqual(high_item["confidence"], "HIGH")
        self.assertEqual(high_item["source"], "GIT_LFS_METADATA")

    def test_unknown_size_uses_head_content_length_without_mutating_manifest_hash(self):
        self.state.mode = "unknown"
        unknown_ref = self.revision_ref(self.search(q="unknown")["items"][0])
        listed = self.invoke("source.list_revisions", {"source_id": SOURCE_ID, "upstream_namespace": "acme", "upstream_model_id": "demo"})
        listed_revision = next(row for row in listed["items"] if row["revision_key"] == unknown_ref["revision_key"])
        unknown_manifest = self.invoke("source.get_manifest", unknown_ref)
        self.assertEqual(unknown_manifest["manifest_hash"], listed_revision["manifest_hash"])
        estimate = unknown_manifest["artifacts"][0]["size_estimate"]
        self.assertEqual(estimate["confidence"], "HIGH")
        self.assertEqual(estimate["source"], "HTTP_HEAD")
        self.assertIsNone(unknown_manifest["artifacts"][0]["size_bytes"])
        self.assertEqual(self.invoke("source.estimate_size", unknown_ref)["confidence"], "HIGH")
        self.assertGreaterEqual(self.state.head_count, 2)

    def test_unavailable_head_keeps_size_unknown(self):
        self.state.mode = "unknown_no_head"
        ref = self.revision_ref(self.search(q="no-head")["items"][0])
        manifest = self.invoke("source.get_manifest", ref)
        self.assertEqual(manifest["artifacts"][0]["size_estimate"]["confidence"], "UNKNOWN")
        self.assertEqual(self.invoke("source.estimate_size", ref)["confidence"], "UNKNOWN")

    def test_gated_repository_requires_license_before_manifest_or_transfer(self):
        self.state.mode = "gated"
        variant = self.search(q="gated")["items"][0]
        ref = self.revision_ref(variant)
        with self.assertRaises(DomainError) as rejected:
            self.invoke("source.get_manifest", ref)
        self.assertEqual(rejected.exception.data["code"], "LICENSE_REQUIRED")
        self.assertEqual(self.state.resolve_count, 0)

    def test_private_repository_requires_auth_before_manifest_or_transfer(self):
        self.state.mode = "private"
        variant = self.search(q="private")["items"][0]
        ref = self.revision_ref(variant)
        with self.assertRaises(DomainError) as rejected:
            self.invoke("source.get_manifest", ref)
        self.assertEqual(rejected.exception.data["code"], "AUTH_REQUIRED")
        self.assertEqual(self.state.resolve_count, 0)

    def test_changed_manifest_for_same_pinned_commit_is_integrity_failure(self):
        self.search()
        self.state.mode = "high"
        with self.assertRaises(DomainError) as rejected:
            self.search(q="changed")
        self.assertEqual(rejected.exception.data["code"], "INTEGRITY_FAILED")
        self.assertEqual(rejected.exception.data["details"]["reason"], "hf_pinned_revision_manifest_changed")

    def test_chunk_ranges_are_bounded_resumable_and_eof_is_exact(self):
        ref = self.revision_ref()
        first = self.invoke("source.download_chunk", self.chunk_request(ref, 0))
        self.assertEqual(base64.b64decode(first["data_base64"], validate=True), CONTENT[:4])
        self.assertFalse(first["eof"])
        self.assertEqual(first["validator"], {"kind": "STRONG_ETAG", "value": ETAG})
        second = self.invoke("source.download_chunk", self.chunk_request(ref, 4, validator=first["validator"]))
        self.assertEqual(base64.b64decode(second["data_base64"], validate=True), CONTENT[4:8])
        final = self.invoke("source.download_chunk", self.chunk_request(ref, 8, validator=first["validator"]))
        self.assertEqual(base64.b64decode(final["data_base64"], validate=True), CONTENT[8:])
        self.assertTrue(final["eof"])
        self.assertEqual(final["total_bytes"], str(len(CONTENT)))
        self.assertEqual(self.state.resolve_bytes_sent, len(CONTENT))
        self.assertTrue(all("authorization" not in headers and "cookie" not in headers for _method, _path, headers in self.state.requests))
        end = self.invoke("source.download_chunk", self.chunk_request(ref, len(CONTENT), validator=first["validator"]))
        self.assertTrue(end["eof"])
        self.assertEqual(end["data_base64"], "")
        with self.assertRaises(DomainError):
            self.invoke("source.download_chunk", self.chunk_request(ref, 0, maximum=524289))

    def test_gated_probes_and_direct_download_execution_fail_closed(self):
        probe = self.invoke("source.auth_probe", {
            "source_id": SOURCE_ID, "auth_mode": "TOKEN", "account_id": str(uuid.uuid7()),
            "credential_reference_id": str(uuid.uuid7()),
        })
        self.assertTrue(probe["auth_required"])
        with self.assertRaises(DomainError) as unsupported:
            self.invoke("source.download", {"plan_id": str(uuid.uuid7()), "plan_hash": "e" * 64, "approval_id": None})
        self.assertEqual(unsupported.exception.data["code"], "UNSUPPORTED_CAPABILITY")

    def test_cross_origin_signed_redirect_is_rejected_without_leaking_url(self):
        self.state.mode = "evil_redirect"
        ref = self.revision_ref()
        with self.assertRaises(DomainError) as rejected:
            self.invoke("source.download_chunk", self.chunk_request(ref, 0))
        self.assertEqual(rejected.exception.data["code"], "FORBIDDEN")
        self.assertNotIn("X-Amz-Signature", json.dumps(rejected.exception.data))

    def test_rate_limit_maps_to_retryable_source_error(self):
        self.state.search_status = 429
        with self.assertRaises(DomainError) as rejected:
            self.search()
        self.assertEqual(rejected.exception.data["code"], "UPSTREAM_UNAVAILABLE")
        self.assertTrue(rejected.exception.data["retryable"])
        self.assertEqual(rejected.exception.data["details"]["reason"], "source_rate_limited")
        self.assertEqual(rejected.exception.data["details"]["original_error"]["system"], "source")
        self.assertEqual(json.loads(rejected.exception.data["details"]["original_error"]["message"])["retry_after_seconds"], "20")

    def test_plan_preview_is_bound_to_selected_variant_and_destination(self):
        variant = self.search()["items"][0]
        plan = self.invoke("source.download_plan", {
            "variant_id": variant["id"], "storage_location_id": str(uuid.uuid7()),
            "hard_budget_bytes": None, "source_id": SOURCE_ID, "priority": "NORMAL",
            "optional_artifact_ids": [],
        })
        self.assertEqual(plan["operation"], "source.download")
        self.assertFalse(plan["approval_required"])
        self.assertEqual(plan["steps"][0]["request"]["source_download_v1"]["revision"]["source_id"], SOURCE_ID)
        self.assertEqual(plan["storage_estimate"]["download_bytes"], str(len(CONTENT)))

    def test_public_binding_selects_central_uuid_and_pinned_bundle_without_file_transfer(self):
        self.state.mode = "mixed_dirs"
        variants = self.search()["items"]
        self.assertGreater(len(variants), 1)
        variant = next(item for item in variants if item["metadata"]["source_v1"]["normalized"]["bundle_key"].endswith(":b"))
        revision = variant["metadata"]["source_v1"]["normalized"]["catalog_reference"]
        manifest = self.invoke("source.get_manifest", revision)
        binding = {"revision": revision, "manifest_hash": manifest["manifest_hash"], "artifacts": [
            {"artifact_id": str(uuid.uuid7()), "relative_path": row["relative_path"], "required": row["required"]}
            for row in sorted(manifest["artifacts"], key=lambda row: row["relative_path"])]}
        central_id = str(uuid.uuid7())
        request = {"variant_id": central_id, "storage_location_id": str(uuid.uuid7()), "source_id": SOURCE_ID,
                   "priority": "NORMAL", "hard_budget_bytes": None, "optional_artifact_ids": [], "acquisition": binding}
        plan = self.invoke("source.download_plan", request)
        self.assertNotEqual(central_id, variant["id"])
        payload = plan["steps"][0]["request"]
        self.assertEqual(payload["source_acquisition_v1"], binding)
        self.assertEqual(payload["source_download_v1"]["variant_id"], central_id)
        self.assertEqual(payload["source_download_v1"]["revision"], revision)
        self.assertEqual({row["relative_path"] for row in payload["source_download_v1"]["artifacts"]},
                         {row["relative_path"] for row in manifest["artifacts"]})
        self.assertEqual(self.state.resolve_bytes_sent, 0)
        changed = json.loads(json.dumps(request))
        changed["acquisition"]["manifest_hash"] = "d" * 64
        with self.assertRaises(DomainError):
            self.invoke("source.download_plan", changed)
        self.assertEqual(self.state.resolve_bytes_sent, 0)


if __name__ == "__main__":
    unittest.main(verbosity=2)
