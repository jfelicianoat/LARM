"""Contract and failure tests for the five independent P049 source profiles."""
from __future__ import annotations

import copy
import json
import sys
import tempfile
import unittest
import urllib.error
import uuid
from email.message import Message
from pathlib import Path
from urllib.parse import parse_qs, urlsplit

REPO = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO / "adapters/sdk/python"))
sys.path.insert(0, str(REPO / "adapters/source/p049"))

from adapter import CivitaiAdapter, create_adapter  # noqa: E402
from larm_adapter_sdk import Contracts, DomainError  # noqa: E402
from package import PROVIDERS, create, package_hash  # noqa: E402


SOURCE_ID = "019b7b7d-4b2a-7a11-8ac1-73677a0c81a2"
VERSION_KEY = "version:67890"
ORIGIN = "https://civitai.com:443"
FIXTURES = REPO / "tests/fixtures/p049"


class Context:
    def __init__(self, cancelled=False):
        self.cancelled = cancelled

    def check_cancelled(self):
        if self.cancelled:
            raise DomainError("RESOURCE_UNAVAILABLE", "Operación cancelada", "operation_cancelled")


class FixtureResponse:
    def __init__(self, status, payload=None, *, raw=None):
        self.status = self.code = status
        self.headers = Message()
        self.headers["Content-Type"] = "application/json"
        self.body = raw if raw is not None else json.dumps(payload, separators=(",", ":")).encode()

    def read(self, size=-1):
        return self.body if size < 0 else self.body[:size]

    def close(self):
        return None

    def __enter__(self):
        return self

    def __exit__(self, *_args):
        self.close()


class FixtureOpener:
    def __init__(self, model, page, status=None, error=None):
        self.model = model
        self.page = page
        self.status = status
        self.error = error
        self.requests = []

    def open(self, request, timeout=12):
        del timeout
        self.requests.append(request.full_url)
        if self.error:
            raise self.error
        if self.status:
            payload = json.loads((FIXTURES / ("unauthorized.json" if self.status == 401 else "rate-limited.json")).read_text())
            return FixtureResponse(self.status, payload)
        parsed = urlsplit(request.full_url)
        query = parse_qs(parsed.query)
        if parsed.path == "/api/v1/models":
            if query.get("cursor"):
                second = copy.deepcopy(self.model)
                second["id"] = 54321
                second["name"] = "Second fixture model"
                second["modelVersions"][0]["id"] = 98765
                return FixtureResponse(200, {"items": [second], "metadata": {}})
            return FixtureResponse(200, self.page)
        if parsed.path == "/api/v1/models/12345":
            return FixtureResponse(200, self.model)
        return FixtureResponse(404, {"error": "not found"})


def load_fixture(name):
    return json.loads((FIXTURES / name).read_text(encoding="utf-8"))


class P049AdapterTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.contracts = Contracts(REPO / "docs")
        cls.page = load_fixture("civitai-model-page.json")
        cls.model = load_fixture("civitai-model-detail.json")

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="larm-p049-test-")
        self.root = Path(self.temp.name)
        self.package, self.work = self.root / "package", self.root / "work"
        self.package.mkdir()
        self.work.mkdir()
        (self.work / "profile.json").write_text(json.dumps({"provider": "CIVITAI", "source_id": SOURCE_ID}), encoding="utf-8")
        self.manifest = {"permissions": {"network_origins": [ORIGIN]}}
        self.adapter = CivitaiAdapter(self.package, self.work, self.manifest, self.contracts)
        self.transport = FixtureOpener(self.model, self.page)
        self.adapter.opener = self.transport

    def tearDown(self):
        self.temp.cleanup()

    def search(self, *, query="demo", cursor=None, limit=20, adapter=None):
        params = {"source_id": SOURCE_ID, "q": query, "capabilities": [], "cursor": cursor, "limit": limit}
        self.contracts.request("source.search", params)
        return (adapter or self.adapter).call("source.search", params, Context())

    def upstream(self, method, revision=VERSION_KEY):
        params = {"source_id": SOURCE_ID, "upstream_namespace": "civitai", "upstream_model_id": "12345"}
        if method in ("source.get_manifest", "source.estimate_size"):
            params["revision_key"] = revision
        self.contracts.request(method, params)
        return self.adapter.call(method, params, Context())

    def test_catalog_pagination_is_cursor_bound_and_keeps_stable_uuidv7(self):
        first = self.search()
        second = self.search(cursor=first["next_cursor"])
        again = self.search()
        self.assertEqual(len(first["items"]), 1)
        self.assertEqual(first["items"][0]["id"], again["items"][0]["id"])
        self.assertEqual(second["items"][0]["metadata"]["source_v1"]["model_id"], "54321")
        self.assertEqual(uuid.UUID(first["items"][0]["id"]).version, 7)
        query = parse_qs(urlsplit(self.transport.requests[0]).query)
        self.assertEqual(query["nsfw"], ["false"])
        self.assertEqual(query["limit"], ["20"])
        self.assertEqual(query["query"], ["demo"])
        self.assertIn("cursor", parse_qs(urlsplit(self.transport.requests[1]).query))
        with self.assertRaises(DomainError) as raised:
            self.search(query="different", cursor=first["next_cursor"])
        self.assertEqual(raised.exception.data["code"], "VALIDATION_FAILED")

    def test_get_model_revisions_manifest_and_size_confidence(self):
        model = self.upstream("source.get_model")
        self.contracts.response("source.get_model", model)
        self.assertEqual(model["license"], "CreativeML Open RAIL-M")
        self.assertEqual(model["limits"]["nsfw"], False)
        self.assertEqual(model["limits"]["nsfw_level"], 1)
        revisions = self.upstream("source.list_revisions")
        self.contracts.response("source.list_revisions", revisions)
        self.assertEqual(revisions["items"][0]["revision_key"], VERSION_KEY)
        self.assertEqual(revisions["items"][0]["metadata"]["source_v1"]["license"], "CreativeML Open RAIL-M")

        manifest = self.upstream("source.get_manifest")
        self.contracts.response("source.get_manifest", manifest)
        self.assertEqual(len(manifest["artifacts"]), 3)  # traversal path was discarded
        estimates = {item["relative_path"]: item["size_estimate"] for item in manifest["artifacts"]}
        estimated = estimates["files/demo.safetensors"]
        self.assertEqual(estimated["confidence"], "ESTIMATED")
        self.assertIsNone(estimated["bytes"])
        self.assertEqual((estimated["lower_bound_bytes"], estimated["upper_bound_bytes"]), ("1024", "2048"))
        self.assertEqual(estimates["files/NaN.safetensors"]["confidence"], "UNKNOWN")

        unknown_total = self.upstream("source.estimate_size")
        self.contracts.response("source.estimate_size", unknown_total)
        self.assertEqual(unknown_total["confidence"], "UNKNOWN")
        self.assertIsNone(unknown_total["upper_bound_bytes"])

        estimated_model = copy.deepcopy(self.model)
        estimated_model["modelVersions"][0]["files"] = [estimated_model["modelVersions"][0]["files"][0]]
        self.transport.model = estimated_model
        total = self.upstream("source.estimate_size")
        self.contracts.response("source.estimate_size", total)
        self.assertEqual(total["confidence"], "ESTIMATED")
        self.assertEqual(total["download_bytes"], "1536")
        self.assertEqual(total["upper_bound_bytes"], "2048")
        self.assertIsNone(total["required_free_bytes"])

    def test_auth_rate_limit_timeout_malformed_json_and_cancel_are_fail_closed(self):
        self.transport.status = 401
        with self.assertRaises(DomainError) as unauthorized:
            self.search()
        self.assertEqual(unauthorized.exception.data["code"], "AUTH_REQUIRED")

        self.transport.status = 429
        with self.assertRaises(DomainError) as limited:
            self.search()
        self.assertEqual(limited.exception.data["code"], "RESOURCE_UNAVAILABLE")
        self.assertTrue(limited.exception.data["retryable"])

        self.transport.status = None
        self.transport.error = TimeoutError("fixture timeout")
        with self.assertRaises(DomainError) as timeout:
            self.search()
        self.assertEqual(timeout.exception.data["code"], "RESOURCE_UNAVAILABLE")
        self.assertTrue(timeout.exception.data["retryable"])

        self.transport.error = None
        self.transport.open = lambda *_args, **_kwargs: FixtureResponse(200, raw=b'{"items":[],"items":[]}')
        with self.assertRaises(DomainError) as malformed:
            self.search()
        self.assertEqual(malformed.exception.data["code"], "PROTOCOL_ERROR")

        before = len(self.transport.requests)
        with self.assertRaises(DomainError) as cancelled:
            self.adapter.call("source.search", {"source_id": SOURCE_ID, "q": "demo", "capabilities": [], "cursor": None, "limit": 20}, Context(cancelled=True))
        self.assertEqual(cancelled.exception.data["code"], "RESOURCE_UNAVAILABLE")
        self.assertEqual(len(self.transport.requests), before)

    def test_auth_probe_is_public_metadata_only_and_download_is_not_enabled(self):
        params = {"source_id": SOURCE_ID, "auth_mode": "NONE", "account_id": None, "credential_reference_id": None}
        self.contracts.request("source.auth_probe", params)
        result = self.adapter.call("source.auth_probe", params, Context())
        self.contracts.response("source.auth_probe", result)
        self.assertFalse(result["auth_required"])
        self.assertIn("downloads require", result["reason"])
        with self.assertRaises(DomainError) as download:
            self.adapter.call("source.download_plan", {}, Context())
        self.assertEqual(download.exception.data["code"], "UNSUPPORTED_CAPABILITY")

    def test_each_other_provider_isolated_and_fails_closed_without_network(self):
        for provider, slug in PROVIDERS.items():
            if provider == "CIVITAI":
                continue
            with self.subTest(provider=provider), tempfile.TemporaryDirectory() as temporary:
                base = Path(temporary)
                package, work = base / "package", base / "work"
                package.mkdir(); work.mkdir()
                (work / "profile.json").write_text(json.dumps({"provider": provider, "source_id": SOURCE_ID}), encoding="utf-8")
                config = json.loads((REPO / "adapters/source" / slug / "provider.json").read_text(encoding="utf-8"))
                self.assertEqual(config["adapter_id"], f"larm.source.{slug}")
                adapter = create_adapter(provider, package, work, {"permissions": {"network_origins": []}}, self.contracts)
                params = {"source_id": SOURCE_ID, "q": "demo", "capabilities": [], "cursor": None, "limit": 20}
                with self.assertRaises(DomainError) as raised:
                    adapter.call("source.search", params, Context())
                self.assertEqual(raised.exception.data["code"], "UNSUPPORTED_CAPABILITY")
                auth_params = {"source_id": SOURCE_ID, "auth_mode": "NONE", "account_id": None, "credential_reference_id": None}
                auth = adapter.call("source.auth_probe", auth_params, Context())
                self.contracts.response("source.auth_probe", auth)
                self.assertTrue(auth["auth_required"])

    def test_package_builder_generates_five_distinct_manifests_and_origins(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            seen = set()
            for provider, slug in PROVIDERS.items():
                package, work = root / f"{slug}-package", root / f"{slug}-work"
                manifest = create(REPO, package, work, provider, portable=False)
                checked = self.contracts.manifest(json.loads((package / "manifest.json").read_text(encoding="utf-8")))
                self.assertEqual(checked["id"], f"larm.source.{slug}")
                self.assertEqual(checked["package_sha256"], package_hash(package))
                seen.add(checked["id"])
                capability = json.loads((package / "source/capabilities.json").read_text(encoding="utf-8"))
                self.assertEqual(capability["provider"], provider)
                self.assertEqual(checked["permissions"]["network_origins"], [ORIGIN] if provider == "CIVITAI" else [])
            self.assertEqual(len(seen), 5)


if __name__ == "__main__":
    unittest.main(verbosity=2)
