#!/usr/bin/env python3
"""Checks representative CustomRules against the published v3 JSON Schema."""

from __future__ import annotations

import json
from pathlib import Path

from jsonschema import Draft202012Validator

REPO = Path(__file__).resolve().parents[1]
schema = json.loads((REPO / "docs/contracts/schemas/larm.schema.json").read_text(encoding="utf-8"))
validator = Draft202012Validator({"$defs": schema["$defs"], "$ref": "#/$defs/CustomRules"})


def rule(field: str, operator: str, value: object) -> dict:
    return {
        "id": "r1",
        "predicates": [{"field": field, "operator": operator, "value": value}],
        "effect": "deny",
        "limit_bytes": None,
        "target_storage_location_id": None,
    }


valid = [
    rule("format", "eq", "GGUF"),
    rule("architecture", "in", ["x86_64", "aarch64"]),
    rule("capability", "contains", "text.generate"),
    rule("backend", "contains", ["cuda", "vulkan"]),
    rule("os_build", "range", [22621, 26200]),
    rule("format", "exists", True),
    rule("quantization", "eq", ["Q4_K_M"]),
]
for item in valid:
    errors = list(validator.iter_errors({"schema_version": "1.0", "rules": [item]}))
    if errors:
        raise SystemExit(f"CustomRules permitido rechazado por schema: {errors[0].message}")

invalid = [
    rule("path", "eq", "C:/outside"),
    rule("format", "execute", "arbitrary"),
    {**rule("format", "eq", "GGUF"), "script": "run"},
]
for item in invalid:
    errors = list(validator.iter_errors({"schema_version": "1.0", "rules": [item]}))
    if not errors:
        raise SystemExit(f"CustomRules no permitido aceptado por schema: {item}")

print("CustomRules schema: 7 formas admitidas y 3 casos abiertos rechazados")
