#!/usr/bin/env python3
"""Validate P072's protocol schema and its links to the public LARM contract."""
from __future__ import annotations

import json
from pathlib import Path
from urllib.parse import urljoin

from jsonschema import Draft202012Validator
from referencing import Registry, Resource


ROOT = Path(__file__).resolve().parents[1]
PROTOCOL_PATH = ROOT / "adapters/runtime/diffusers/protocol.schema.json"
CONTRACT_PATH = ROOT / "docs/contracts/schemas/larm.schema.json"


def refs(value: object):
    if isinstance(value, dict):
        ref = value.get("$ref")
        if isinstance(ref, str) and not ref.startswith("#"):
            yield ref
        for child in value.values():
            yield from refs(child)
    elif isinstance(value, list):
        for child in value:
            yield from refs(child)


def main() -> int:
    protocol = json.loads(PROTOCOL_PATH.read_text(encoding="utf-8"))
    contract = json.loads(CONTRACT_PATH.read_text(encoding="utf-8"))
    Draft202012Validator.check_schema(protocol)
    Draft202012Validator.check_schema(contract)

    contract_uri = urljoin(protocol["$id"], "../../../docs/contracts/schemas/larm.schema.json")
    registry = Registry().with_resource(contract_uri, Resource.from_contents(contract))
    resolver = registry.resolver(base_uri=protocol["$id"])
    external_refs = sorted(set(refs(protocol)))
    if not external_refs:
        raise AssertionError("P072 protocol schema must reference the public contract")
    for ref in external_refs:
        resolver.lookup(ref)

    request = {
        "lease_id": "0192aaaa-0000-7000-8000-000000000001",
        "request_id": "0192aaaa-0000-7000-8000-000000000002",
        "capability": "image.generate",
        "input": {
            "prompt": "tiny fixture",
            "negative_prompt": "",
            "width": 64,
            "height": 64,
            "steps": 1,
            "seed": None,
        },
        "generation": None,
        "stream": False,
    }
    invoke_schema = {
        "$schema": protocol["$schema"],
        "$id": protocol["$id"],
        "allOf": [protocol["$defs"]["protocol_invoke"]],
    }
    Draft202012Validator(invoke_schema, registry=registry).validate(request)

    output = {
        "images": [
            {
                "artifact_id": "0192aaaa-0000-7000-8000-000000000005",
                "sha256": "a" * 64,
                "media_type": "image/png",
                "size_bytes": "1",
            }
        ]
    }
    Draft202012Validator(contract["$defs"]["image_generate_output"]).validate(output)
    print(f"schemas Draft 2020-12 válidos; {len(external_refs)} referencias públicas resueltas; invoke y salida conformes")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
