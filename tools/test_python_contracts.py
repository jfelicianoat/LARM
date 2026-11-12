#!/usr/bin/env python3
"""Mismo corpus que Rust y fallos cerrados de empaquetado, sin red ni fallback."""
import hashlib
import json
import shutil
import sys
import tempfile
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(REPO / "adapters/sdk/python"))
from larm_adapter_sdk import Contracts, ContractViolation  # noqa: E402


class MethodContracts(unittest.TestCase):
    def test_all_40_signatures_against_identical_rust_corpus(self):
        contracts = Contracts(REPO / "docs")
        corpus = json.loads((REPO / "tests/fixtures/adapters/method-schema-corpus.json").read_text(encoding="utf-8"))
        accepted, directions = 0, set()
        for case in corpus["cases"]:
            operation = contracts.request if case["direction"] == "request" else contracts.response
            try:
                operation(case["method"], case["value"])
                actual = True
            except ContractViolation:
                actual = False
            self.assertEqual(actual, case["valid"], (case["method"], case["direction"]))
            if actual:
                accepted += 1
                directions.add((case["method"], case["direction"]))
        # Mismo oráculo canónico que crates/larm-adapter-sdk/tests/contracts.rs.
        self.assertEqual((accepted, len(directions)), (1000, 80))

    def test_error_does_not_echo_value_or_allow_unknown_method(self):
        contracts = Contracts(REPO / "docs")
        marker = "private-p026-value-never-in-error"
        for method, value in (("benchmark.run", {"token": marker}), ("benchmark.unknown", {})):
            with self.assertRaises(ContractViolation) as error:
                contracts.request(method, value)
            self.assertNotIn(marker, str(error.exception))

    def test_missing_tampered_external_and_stale_sources_have_no_fallback(self):
        runs = REPO / "docs/reports/P026_runs"
        runs.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(prefix="contracts-", dir=runs) as temporary:
            folder = Path(temporary).resolve()
            self.assertTrue(folder.is_relative_to(runs.resolve()))
            docs = folder / "docs"
            for relative in ("contracts/schemas/larm.schema.json", "contracts/schemas/domain.schema.json", "contracts/adapter-methods.json", "adapters/schemas/frame.request.schema.json", "adapters/schemas/frame.response.schema.json", "adapters/schemas/frame.progress.schema.json"):
                target = docs / relative
                target.parent.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(REPO / "docs" / relative, target)
            shutil.copytree(REPO / "docs/adapters/method-schemas", docs / "adapters/method-schemas")
            contracts_path = docs / "adapters/method-schemas"
            index_path = contracts_path / "index.json"
            index_original = index_path.read_bytes()
            schema_path = contracts_path / "benchmark.run.request.schema.json"
            original = schema_path.read_bytes()
            schema_path.unlink()
            with self.assertRaises(ContractViolation):
                Contracts(docs)
            schema_path.write_bytes(b"{}")
            with self.assertRaises(ContractViolation):
                Contracts(docs)
            for reference in ("https://127.0.0.1:1/never", "file:///C:/never"):
                schema_path.write_text(json.dumps({"$ref": reference}), encoding="utf-8")
                index = json.loads(index_original)
                entry = next(e for e in index["methods"] if e["method"] == "benchmark.run")
                entry["request"]["sha256"] = hashlib.sha256(schema_path.read_bytes()).hexdigest()
                index_path.write_text(json.dumps(index), encoding="utf-8")
                with self.assertRaises(ContractViolation):
                    Contracts(docs)
            schema_path.write_bytes(original)
            index_path.write_bytes(index_original)
            table = docs / "contracts/adapter-methods.json"
            table.write_bytes(table.read_bytes() + b"\n")
            with self.assertRaises(ContractViolation):
                Contracts(docs)


if __name__ == "__main__":
    unittest.main(verbosity=2)
