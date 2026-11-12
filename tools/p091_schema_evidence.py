#!/usr/bin/env python3
"""Validates P091's scalar CompatibilityRecord contract and closed fields."""

from __future__ import annotations

import copy
import json
import secrets
import time
import uuid
from datetime import datetime, timezone
from pathlib import Path

from jsonschema import Draft202012Validator
from referencing import Registry, Resource

REPO = Path(__file__).resolve().parent.parent
PACKAGE = REPO.parent / "LARM_prompts_v3"
GATES = (
    "format", "architecture", "ops", "quantization", "isa", "os", "drivers",
    "execution_provider", "license_trust", "bundle_integrity", "memory", "protocol",
    "backend_operable",
)


def uuid7() -> str:
    millis = int(time.time() * 1000)
    value = (millis << 80) | (7 << 76) | (secrets.randbits(12) << 64)
    value |= (2 << 62) | secrets.randbits(62)
    return str(uuid.UUID(int=value))


def main() -> int:
    schema = json.loads((PACKAGE / "schemas/domain.schema.json").read_text(encoding="utf-8"))
    registry = Registry().with_resource("domain.schema.json", Resource.from_contents(schema))
    validator = Draft202012Validator(
        {"$ref": "domain.schema.json#/$defs/compatibility_record"}, registry=registry
    )
    now_value = datetime.now(timezone.utc)
    now = now_value.strftime("%Y-%m-%dT%H:%M:%S.") + f"{now_value.microsecond // 1000:03d}Z"
    ids = [uuid7() for _ in range(4)]
    record = {
        "id": ids[0], "created_at": now, "updated_at": now, "row_version": 1,
        "metadata": {}, "variant_id": ids[1], "runtime_version_id": ids[2],
        "backend_id": ids[3], "hardware_fingerprint": "a" * 64,
        "launch_profile_hash": "b" * 64, "status": "COMPATIBLE",
        "hard_gates": [
            {"name": name, "status": "PASS", "reason": f"{name} confirmado",
             "evidence_refs": [f"fixture:{name}"]}
            for name in GATES
        ],
        "limitations": [], "technical_fit": 63.5,
        "evidence_refs": [f"fixture:{name}" for name in GATES],
    }
    if list(validator.iter_errors(record)):
        raise SystemExit("FAIL: el CompatibilityRecord con TechnicalFit no cumple el schema")

    invalid_cases = []
    extra_diagnostics = copy.deepcopy(record)
    extra_diagnostics["breakdown"] = []
    invalid_cases.append(extra_diagnostics)
    extra_confidence = copy.deepcopy(record)
    extra_confidence["confidence"] = "EXACT"
    invalid_cases.append(extra_confidence)
    invalid_score = copy.deepcopy(record)
    invalid_score["technical_fit"] = 100.1
    invalid_cases.append(invalid_score)
    if any(not list(validator.iter_errors(case)) for case in invalid_cases):
        raise SystemExit("FAIL: el schema aceptó score fuera de rango o campos públicos no contratados")
    print("PASS: el score escalar válido se acepta; breakdown/confidence públicos y score >100 se rechazan")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
