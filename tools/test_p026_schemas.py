#!/usr/bin/env python3
"""Contrasta la generación P026 con jsonschema sobre los contratos originales.

Los witnesses son datos de schema, nunca replies de un worker ni evidencia de
conducta. Cada uno se acepta primero contra el contrato original independiente.
"""
import copy
import hashlib
import json
import sys
import unittest
from pathlib import Path

from jsonschema import Draft202012Validator
from referencing import Registry, Resource

from p026_schemas import CONTRACTS, OUT, REPO, documents, generated
from p026_oracle import CanonicalValidator

UUID = "019a0000-0000-7000-8000-000000000001"
PATTERNS = {
    "^(0|[1-9][0-9]*)$": "0",
    "^[1-9][0-9]*$": "1",
    "^[0-9a-f]{64}$": "a" * 64,
    '^"[!#-~]*"$': '"fixture-v1"',
    "^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$": "AA==",
    "^[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$": UUID,
    "^[A-Za-z_][A-Za-z0-9_]*$": "FIELD",
    "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}\\.\\d{3}Z$": "2026-10-17T00:00:00.000Z",
    "^larm/": "larm/fixture",
}


def merge(left, right):
    result = left | right
    if "properties" in left or "properties" in right:
        result["properties"] = left.get("properties", {}) | right.get("properties", {})
    if "required" in left or "required" in right:
        result["required"] = list(dict.fromkeys(left.get("required", []) + right.get("required", [])))
    return result


def witness(node, root, depth=0):
    """Sólo construye corpus; el original determina si es un witness válido."""
    if depth > 80:
        raise ValueError("Witness recursivo sin base; hace falta fixture explícita")
    if "$ref" in node:
        target = root
        for segment in node["$ref"].removeprefix("#/").split("/"):
            target = target[segment.replace("~1", "/").replace("~0", "~")]
        return witness(merge(target, {k: v for k, v in node.items() if k != "$ref"}), root, depth + 1)
    if "const" in node:
        return copy.deepcopy(node["const"])
    if "enum" in node:
        return copy.deepcopy(node["enum"][0])
    if "anyOf" in node:
        base = {k: v for k, v in node.items() if k != "anyOf"}
        for branch in node["anyOf"]:
            candidate = witness(merge(base, branch), root, depth + 1)
            if Draft202012Validator(root).evolve(schema=node).is_valid(candidate):
                return candidate
        raise ValueError("Ninguna rama de witness satisface el schema original")
    kind = node.get("type")
    if isinstance(kind, list):
        return witness(node | {"type": kind[0]}, root, depth + 1)
    if kind == "object":
        properties = node.get("properties", {})
        value = {key: witness(properties.get(key, {}), root, depth + 1) for key in node.get("required", [])}
        for index in range(max(0, node.get("minProperties", 0) - len(value))):
            key = next((name for name in properties if name not in value), f"fixture_{index}")
            child = properties.get(key, node.get("additionalProperties", {}))
            if child is False:
                raise ValueError("Hace falta fixture explícita para minProperties")
            value[key] = witness({} if child is True else child, root, depth + 1)
        return value
    if kind == "array":
        return [witness(node.get("items", {}), root, depth + 1) for _ in range(node.get("minItems", 0))]
    if kind == "string":
        if "pattern" in node:
            return PATTERNS[node["pattern"]]
        return "x" * max(1, node.get("minLength", 0))
    if kind in ("integer", "number"):
        return node.get("minimum", 0)
    if kind == "boolean":
        return False
    return None


def mutants(value):
    # Mutaciones sobre el valor, sin consultar la implementación del generador.
    for replacement in (None, True, False, 0, -1, 1.5, 2**64, "", "x", [], {}, [None]):
        yield replacement
    if isinstance(value, dict):
        yield value | {"p026_extra_field": "not-in-contract"}
        for key, original in value.items():
            yield {name: item for name, item in value.items() if name != key}
            for replacement in (None, "", "not-a-uuid-or-hash", -1, {}, [None], "9223372036854775807", "9223372036854775808", "9999999999999999999", "18446744073709551616", "00001"):
                yield value | {key: replacement}
            if isinstance(original, dict):
                for changed in mutants(original):
                    yield value | {key: changed}
    elif isinstance(value, str):
        yield value + "extra"


class Schemas(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        docs = documents()
        cls.registry = Registry().with_resources([(name, Resource.from_contents(doc)) for name, doc in docs.items()])
        cls.index = json.loads((OUT / "index.json").read_text(encoding="utf-8"))
        cls.comparisons, cls.accepted, cls.rejected = 0, 0, 0

    def compare(self, reference, schema, values):
        canonical = CanonicalValidator({"$ref": reference}, registry=self.registry)
        emitted = CanonicalValidator(schema)
        for value in values:
            expected = canonical.is_valid(value)
            self.assertEqual(emitted.is_valid(value), expected, reference)
            type(self).comparisons += 1
            if expected:
                type(self).accepted += 1
            else:
                type(self).rejected += 1

    def test_01_regeneration_bytes_and_full_local_closure(self):
        expected = generated()
        self.assertEqual(set(expected), {p.name for p in OUT.glob("*.json")})
        self.assertEqual(len(self.index["methods"]), 40)
        for name, value in expected.items():
            self.assertEqual((OUT / name).read_bytes(), value.encode())
        for entry in self.index["methods"]:
            for direction in ("request", "response"):
                item = entry[direction]
                self.assertEqual(hashlib.sha256((OUT / item["file"]).read_bytes()).hexdigest(), item["sha256"])

    def test_02_every_signature_has_original_valid_witness_and_mutations(self):
        for entry in self.index["methods"]:
            for direction in ("request", "response"):
                item = entry[direction]
                schema = json.loads((OUT / item["file"]).read_text(encoding="utf-8"))
                value = witness(schema, schema)
                original = CanonicalValidator({"$ref": item["canonical_ref"]}, registry=self.registry)
                self.assertTrue(original.is_valid(value), f"Witness inválido {entry['method']} {direction}")
                self.compare(item["canonical_ref"], schema, [value, *mutants(value)])

    def test_03_identical_shared_source_and_common_fixtures(self):
        fixture = json.loads((REPO / "tests/fixtures/source/protocol.json").read_text(encoding="utf-8"))
        count = 0
        for entry in self.index["methods"]:
            for direction, section in (("request", "requests"), ("response", "responses")):
                item = entry[direction]
                schema = json.loads((OUT / item["file"]).read_text(encoding="utf-8"))
                if entry["method"] in fixture[section]:
                    self.compare(item["canonical_ref"], schema, [fixture[section][entry["method"]]])
                    count += 1
                target = item["canonical_ref"].split("/")[-1]
                for folder in ("valid", "invalid"):
                    for path in (REPO / "docs/adapters/examples" / folder).glob(target + "--*.json"):
                        value = json.loads(path.read_text(encoding="utf-8"))
                        original = CanonicalValidator({"$ref": item["canonical_ref"]}, registry=self.registry)
                        self.assertEqual(original.is_valid(value), folder == "valid", path.name)
                        self.compare(item["canonical_ref"], schema, [value])
                        count += 1
        self.assertGreaterEqual(count, 18)

    def test_04_public_acquisition_fixture_and_mutations(self):
        entry = next(row for row in self.index["methods"] if row["method"] == "source.download_plan")
        item = entry["request"]
        schema = json.loads((OUT / item["file"]).read_text(encoding="utf-8"))
        value = json.loads((REPO / "tests/fixtures/source-acquisition-plan.json").read_text(encoding="utf-8"))["request"]
        original = CanonicalValidator({"$ref": item["canonical_ref"]}, registry=self.registry)
        self.assertTrue(original.is_valid(value))
        self.assertFalse(original.is_valid(value | {"acquisition": None}))
        self.assertFalse(original.is_valid(value | {"storage_location_id": None}))
        self.compare(item["canonical_ref"], schema, [value, *mutants(value)])

    @classmethod
    def tearDownClass(cls):
        print(f"P026 generación: {cls.comparisons} comparaciones diferenciales; {cls.accepted} aceptadas y {cls.rejected} rechazadas por el contrato original. No cualifica workers.", flush=True)


if __name__ == "__main__":
    sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    unittest.main(verbosity=2)
