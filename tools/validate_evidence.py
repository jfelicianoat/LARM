#!/usr/bin/env python3
"""Valida cada docs/reports/*.evidence.json contra larm.schema.json#/$defs/PromptEvidence.

Uso: python tools/validate_evidence.py [ruta ...]
Sin argumentos valida todas las evidencias. Exit 0 solo si todas conforman.
"""
import json
import sys
from pathlib import Path

from jsonschema import Draft202012Validator
from referencing import Registry, Resource

REPO = Path(__file__).resolve().parent.parent
SCHEMAS = REPO / "docs/contracts/schemas"


def validator() -> Draft202012Validator:
    larm = json.loads((SCHEMAS / "larm.schema.json").read_text(encoding="utf-8"))
    resources = []
    for p in SCHEMAS.glob("*.schema.json"):
        doc = json.loads(p.read_text(encoding="utf-8"))
        res = Resource.from_contents(doc)
        resources.append((p.name, res))
        if "$id" in doc:
            resources.append((doc["$id"], res))
    registry = Registry().with_resources(resources)
    schema = {"$schema": "https://json-schema.org/draft/2020-12/schema",
              "$ref": "larm.schema.json#/$defs/PromptEvidence"}
    _ = larm
    return Draft202012Validator(schema, registry=registry)


def main(argv: list[str]) -> int:
    paths = [Path(a) for a in argv] or sorted((REPO / "docs/reports").glob("*.evidence.json"))
    v = validator()
    bad = 0
    for p in paths:
        doc = json.loads(p.read_text(encoding="utf-8"))
        errors = sorted(v.iter_errors(doc), key=lambda e: list(e.path))
        if errors:
            bad += 1
            print(f"FAIL {p.name}: {len(errors)} errores")
            for e in errors[:6]:
                print(f"   - {'/'.join(map(str, e.path)) or '<raiz>'}: {e.message[:140]}")
        else:
            print(f"PASS {p.name}")
    return 1 if bad else 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
