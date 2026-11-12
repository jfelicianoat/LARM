"""Independent Python binding/selection checks; no worker qualification."""
import copy
import json
import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "adapters/sdk/python"))
from larm_adapter_sdk import DomainError  # noqa: E402
from larm_adapter_sdk.source_acquisition import (  # noqa: E402
    select_source_acquisition, source_acquisition_payload, validate_source_acquisition,
)


class SourceAcquisition(unittest.TestCase):
    def setUp(self):
        self.fixture = json.loads((ROOT / "tests/fixtures/source-acquisition-plan.json").read_text(encoding="utf-8"))
        self.request = self.fixture["request"]
        self.manifest = self.fixture["manifest"]

    def test_public_selection_does_not_need_private_variant_id(self):
        result = select_source_acquisition(self.request, self.manifest)
        self.assertEqual([row["relative_path"] for row in result], self.fixture["selected_paths"])
        result[0]["role"] = "changed"
        self.assertNotEqual(result, self.manifest["artifacts"])
        del self.request["optional_artifact_ids"]
        self.assertEqual([row["relative_path"] for row in select_source_acquisition(self.request, self.manifest)], ["weights.bin"])

    def test_bad_binding_and_optional_selection_fail_before_plan(self):
        mutants = []
        for field, value in [("acquisition", None), ("source_id", None),
                             ("optional_artifact_ids", None), ("optional_artifact_ids", [self.request["acquisition"]["artifacts"][1]["artifact_id"]])]:
            bad = copy.deepcopy(self.request)
            bad[field] = value
            mutants.append(bad)
        for field, value in [("revision_key", " pending"), ("source_id", "0192aaaa-0000-7000-8000-000000000099"),
                             ("revision_key", "pending:0192aaaa-0000-7000-8000-000000000099")]:
            bad = copy.deepcopy(self.request)
            bad["acquisition"]["revision"][field] = value
            mutants.append(bad)
        bad = copy.deepcopy(self.request)
        bad["acquisition"]["artifacts"].reverse()
        mutants.append(bad)
        bad = copy.deepcopy(self.request)
        bad["acquisition"]["artifacts"][1]["artifact_id"] = bad["acquisition"]["artifacts"][0]["artifact_id"]
        mutants.append(bad)
        for bad in mutants:
            with self.subTest(bad=bad):
                with self.assertRaises(DomainError):
                    validate_source_acquisition(bad)

    def test_manifest_change_cannot_reuse_the_same_binding(self):
        for field, value in [("manifest_hash", "d" * 64), ("revision_key", "r2"), ("artifacts", [])]:
            bad = copy.deepcopy(self.manifest)
            bad[field] = value
            with self.subTest(field=field):
                with self.assertRaises(DomainError):
                    select_source_acquisition(self.request, bad)
        bad = copy.deepcopy(self.manifest)
        bad["artifacts"][0]["required"] = False
        with self.assertRaises(DomainError):
            select_source_acquisition(self.request, bad)

    def test_step_echoes_exact_binding_and_rejects_extra_unselected_file(self):
        spec = {key: self.request[key] for key in ("variant_id", "storage_location_id", "priority", "hard_budget_bytes")}
        spec["revision"] = self.request["acquisition"]["revision"]
        spec["artifacts"] = select_source_acquisition(self.request, self.manifest)
        payload = source_acquisition_payload(self.request, spec)
        self.assertEqual(payload["source_acquisition_v1"], self.request["acquisition"])
        required_only = copy.deepcopy(self.request)
        required_only["optional_artifact_ids"] = []
        with self.assertRaises(DomainError):
            source_acquisition_payload(required_only, spec)
        legacy = copy.deepcopy(self.request)
        del legacy["acquisition"]
        self.assertNotIn("source_acquisition_v1", source_acquisition_payload(legacy, spec))


if __name__ == "__main__":
    unittest.main()
