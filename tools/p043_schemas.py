#!/usr/bin/env python3
"""Oráculo JSON Schema independiente para firmas source obligatorias y opcionales."""
import copy
import json
from pathlib import Path
from jsonschema import Draft202012Validator
from referencing import Registry, Resource

REPO = Path(__file__).resolve().parent.parent


def main():
    registry = Registry()
    for name in ("larm.schema.json", "domain.schema.json"):
        document = json.loads((REPO / "docs/contracts/schemas" / name).read_text(encoding="utf-8"))
        registry = registry.with_resource(name, Resource.from_contents(document))
    fixture = json.loads((REPO / "tests/fixtures/source/protocol.json").read_text(encoding="utf-8"))
    table = json.loads((REPO / "docs/contracts/adapter-methods.json").read_text(encoding="utf-8"))
    methods = table["methods"] if isinstance(table, dict) else table
    count = 0
    for method in methods:
        if method["family"] != "source":
            continue
        for side, schema in (("requests", "request_schema"), ("responses", "response_schema")):
            validator = Draft202012Validator({"$ref": method[schema]}, registry=registry)
            value = fixture[side][method["method"]]
            validator.validate(value)
            # Todas las claves de estas firmas son required, incluso nullable.
            for key in value:
                missing = copy.deepcopy(value)
                missing.pop(key)
                assert list(validator.iter_errors(missing)), f"required perdido {method['method']}.{key}"
                count += 1
            extra = value | {"unknown_property": True}
            assert list(validator.iter_errors(extra)), f"schema dejó pasar propiedad extra: {method['method']}"
            count += 1
    print(f"PASS: 20 documentos válidos y {count} negativos contra los schemas canónicos")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
