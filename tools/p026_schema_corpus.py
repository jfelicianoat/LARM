#!/usr/bin/env python3
"""Corpus compartido para implementaciones independientes Rust/Python, no workers."""
import hashlib
import json
import sys

from jsonschema import Draft202012Validator
from referencing import Registry, Resource

from p026_schemas import OUT, REPO, documents, render
from test_p026_schemas import mutants, witness
from p026_oracle import CanonicalValidator

TARGET = REPO / "tests/fixtures/adapters/method-schema-corpus.json"


def generated():
    registry = Registry().with_resources([(name, Resource.from_contents(doc)) for name, doc in documents().items()])
    index = json.loads((OUT / "index.json").read_text(encoding="utf-8"))
    fixture = json.loads((REPO / "tests/fixtures/source/protocol.json").read_text(encoding="utf-8"))
    cases = []
    for entry in index["methods"]:
        for direction, section in (("request", "requests"), ("response", "responses")):
            item = entry[direction]
            schema = json.loads((OUT / item["file"]).read_text(encoding="utf-8"))
            original = CanonicalValidator({"$ref": item["canonical_ref"]}, registry=registry)
            good = witness(schema, schema)
            if not original.is_valid(good):
                raise ValueError("El witness no cumple el contrato original")
            values = [good, *mutants(good)]
            if entry["method"] in fixture[section]:
                values.append(fixture[section][entry["method"]])
            if entry["method"] == "source.download_plan" and direction == "request":
                binding = json.loads((REPO / "tests/fixtures/source-acquisition-plan.json").read_text(encoding="utf-8"))["request"]
                if not original.is_valid(binding):
                    raise ValueError("El fixture público de adquisición no cumple el contrato")
                values.extend([binding, *mutants(binding)])
            target = item["canonical_ref"].split("/")[-1]
            for folder in ("valid", "invalid"):
                for path in sorted((REPO / "docs/adapters/examples" / folder).glob(target + "--*.json")):
                    values.append(json.loads(path.read_text(encoding="utf-8")))
            for value in values:
                cases.append({"method": entry["method"], "direction": direction, "value": value, "valid": original.is_valid(value)})
    return render({"schema_version": "1.0", "scope": "schema data only, not worker qualification", "index_sha256": hashlib.sha256((OUT / "index.json").read_bytes()).hexdigest(), "cases": cases})


def main():
    data = generated()
    mode = sys.argv[1] if len(sys.argv) == 2 else ""
    if mode == "write":
        TARGET.write_text(data, encoding="utf-8", newline="\n")
    elif mode != "check":
        raise ValueError("Uso: p026_schema_corpus.py write|check")
    if not TARGET.is_file() or TARGET.read_bytes() != data.encode():
        raise ValueError("Corpus compartido ausente o desactualizado")
    cases = json.loads(data)["cases"]
    print(f"PASS: corpus independiente compartido, {len(cases)} casos de las 80 direcciones; no cualifica workers")
    return 0


if __name__ == "__main__":
    sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    raise SystemExit(main())
