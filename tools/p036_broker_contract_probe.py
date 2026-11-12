#!/usr/bin/env python3
"""Verify the canonical P101 read-only catalog operation consumed by P036."""
from __future__ import annotations

import json
from pathlib import Path

from jsonschema import Draft202012Validator
from referencing import Registry, Resource


ROOT = Path(__file__).resolve().parents[1]
CANONICAL = ROOT.parent / "LARM_prompts_v3"
OPERATION_ID = "P101_get__broker_catalog"
EXPECTED = {
    "id": OPERATION_ID,
    "method": "GET",
    "path": "/api/v1/broker/catalog",
    "scope": "read",
    "request_schema": None,
    "response_schema": "larm.schema.json#/$defs/BrokerCatalog",
    "owner": "P101",
    "mode": "sync",
    "success_status": 200,
    "path_parameters": {},
}


def load_json(path: Path) -> dict:
    return json.loads(path.read_text(encoding="utf-8"))


def find_operation(document: dict) -> dict:
    matches = [item for item in document["operations"] if item.get("id") == OPERATION_ID]
    if len(matches) != 1:
        raise AssertionError(f"{OPERATION_ID} expected exactly once, found {len(matches)}")
    return matches[0]


def main() -> int:
    canonical = find_operation(load_json(CANONICAL / "contracts/api-operations.json"))
    local = find_operation(load_json(ROOT / "docs/contracts/api-operations.json"))
    if canonical != EXPECTED or local != canonical:
        raise AssertionError("P101 broker catalog operation differs from its canonical contract")

    openapi = load_json(ROOT / "docs/api/openapi.json")
    route = openapi["paths"]["/api/v1/broker/catalog"]["get"]
    if route["operationId"] != OPERATION_ID or route["x-larm-contract-operation-id"] != OPERATION_ID:
        raise AssertionError("OpenAPI does not expose the canonical P101 catalog operation")

    schema = load_json(ROOT / "docs/contracts/schemas/larm.schema.json")
    domain = load_json(ROOT / "docs/contracts/schemas/domain.schema.json")
    registry = (
        Registry()
        .with_resource("larm.schema.json", Resource.from_contents(schema))
        .with_resource("domain.schema.json", Resource.from_contents(domain))
    )
    Draft202012Validator.check_schema(schema)
    Draft202012Validator({"$ref": "larm.schema.json#/$defs/BrokerCatalog"}, registry=registry).validate(
        {
            "contract_version": "1.0",
            "items": [],
            "snapshot_at": "2026-10-23T00:00:00.000Z",
        }
    )
    print("P101 broker catalog: canonical/local operation, scope, OpenAPI route and response schema match")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
