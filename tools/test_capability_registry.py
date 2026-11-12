#!/usr/bin/env python3
"""Schemas/estimador/fixtures antes de anuncio; mismo corpus que Rust."""
import copy
import json
import math
import sys
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(REPO / "adapters/sdk/python"))
from larm_adapter_sdk import CapabilityRegistry, Contracts, DomainError  # noqa: E402
from larm_adapter_sdk.capabilities import canonical, digest  # noqa: E402


def estimate(value):
    result = {key:None for key in ("weights_bytes","kv_bytes","activation_bytes","workspace_bytes","overhead_bytes","total_bytes","multimodal_bytes","temporary_bytes","safety_margin_bytes")}
    result.update(workspace_bytes=str(len(canonical(value)) + 32768), per_pool=[], confidence="UNKNOWN", evidence_refs=["fixture:memory-unknown"])
    return result


class Registry(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.contracts = Contracts(REPO / "docs")
        cls.fixture = json.loads((REPO / "tests/fixtures/adapters/capability-registry.json").read_text(encoding="utf-8"))

    def registry(self, entries=None, estimators=None, mappings=None):
        entries = entries if entries is not None else [self.fixture["echo"],self.fixture["embedding"]]
        estimators = estimators if estimators is not None else {e["estimator_id"]:estimate for e in entries}
        mappings = mappings if mappings is not None else {e["mapping"] for e in entries}
        return CapabilityRegistry({"schema_version":"1.0","entries":entries}, self.contracts, estimators, mappings)

    def rejected(self, entry, reason, **options):
        with self.assertRaises(DomainError) as error:
            self.registry([entry], **options)
        self.assertEqual(error.exception.data["details"]["reason"], reason)

    def test_hashes_jcs_float_unicode_and_exact_integer_boundary(self):
        for vector in self.fixture["jcs_vectors"]:
            self.assertEqual(canonical(vector["value"]).decode(), vector["canonical"])
            self.assertEqual(digest(vector["value"]), vector["sha256"])
        for value in (9007199254740993, -9007199254740993, math.nan, math.inf, {1:"x"}, "\ud800"):
            with self.assertRaises((ValueError, TypeError)):
                canonical(value)

    def test_descriptors_are_unqualified_but_complete_and_aliases_read_only(self):
        registry = self.registry()
        registry.check_advertised(["vendor.reference.echo","embedding.create"])
        entry = registry.entry("embedding")
        self.assertEqual(entry["id"], "embedding.create")
        self.assertEqual(registry.schema_id("embedding", "input"), registry.schema_id("embedding.create", "input"))
        self.assertEqual(registry.estimate("embedding", entry["fixture"]["input"])["confidence"], "UNKNOWN")
        self.assertIsNone(registry.estimate(entry["id"], entry["fixture"]["input"])["total_bytes"])
        self.assertEqual(registry.profile_hash, self.registry(list(reversed([self.fixture["echo"],self.fixture["embedding"]]))).profile_hash)
        with self.assertRaises(DomainError): registry.check_advertised(["embedding"])
        with self.assertRaises(DomainError): registry.check_advertised(["vendor.not_registered"])

    def test_tampering_schema_fixture_mapping_estimator_and_builtin_rejected(self):
        good = self.fixture["echo"]
        bad = copy.deepcopy(good); bad["input_sha256"] = "b"*64
        self.rejected(bad, "capability_schema_hash_mismatch")
        bad = copy.deepcopy(good); bad["input_schema"] = {"$ref":"file:///C:/never"}; bad["input_sha256"] = digest(bad["input_schema"])
        self.rejected(bad, "capability_descriptor_invalid")
        bad = copy.deepcopy(good); bad["fixture"]["input"]["text"] = 4
        self.rejected(bad, "capability_fixture_invalid")
        bad = copy.deepcopy(good); bad["fixture_sha256"] = "b"*64
        self.rejected(bad, "capability_fixture_hash_mismatch")
        self.rejected(copy.deepcopy(good), "capability_estimator_missing", estimators={})
        self.rejected(copy.deepcopy(good), "capability_mapping_missing", mappings=set())
        bad = copy.deepcopy(good); bad["id"] = "embedding.create"
        self.rejected(bad, "capability_builtin_schema_mismatch")
        bad = copy.deepcopy(good); bad["id"] = "embedding"
        self.rejected(bad, "capability_descriptor_invalid")
        for invalid_version in ("١.٠", "1.0.0.0", "1.", "1.0\n"):
            bad = copy.deepcopy(good); bad["version"] = invalid_version
            self.rejected(bad, "capability_descriptor_invalid")
        with self.assertRaises(DomainError) as error: self.registry([good,good])
        self.assertEqual(error.exception.data["details"]["reason"], "capability_descriptor_duplicate")

    def test_input_before_estimator_bad_estimate_and_private_output(self):
        calls = []
        good = self.fixture["echo"]
        registry = self.registry([good], estimators={good["estimator_id"]:lambda value: calls.append(value) or estimate(value)})
        with self.assertRaises(DomainError): registry.estimate(good["id"], {"text":1})
        self.assertEqual(calls, [])
        registry.check_advertised([good["id"]]); self.assertEqual(len(calls), 1)
        marker = "private-fixture-value-no-error-echo"
        with self.assertRaises(DomainError) as error: registry.validate(good["id"], "output", {"echo":{"secret":marker}})
        self.assertNotIn(marker, json.dumps(error.exception.data))
        broken = self.registry([good], estimators={good["estimator_id"]:lambda _value: {}})
        with self.assertRaises(DomainError) as error: broken.check_advertised([good["id"]])
        self.assertEqual(error.exception.data["details"]["reason"], "capability_estimator_failed")

    def test_constructor_copies_descriptors_and_does_not_accept_extra_control_fields(self):
        entry = copy.deepcopy(self.fixture["echo"])
        registry = self.registry([entry])
        entry["input_schema"]["properties"]["text"]["maxLength"] = 5000
        with self.assertRaises(DomainError): registry.validate(entry["id"], "input", {"text":"x"*2049})
        bad = copy.deepcopy(self.fixture["echo"]); bad["qualified"] = True
        self.rejected(bad, "capability_descriptor_invalid")


if __name__ == "__main__":
    unittest.main(verbosity=2)
