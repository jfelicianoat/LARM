#!/usr/bin/env python3
"""Comprueba casos positivos/negativos contra el schema canónico de P090."""

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
        "limitations": [], "technical_fit": None,
        "evidence_refs": [f"fixture:{name}" for name in GATES],
    }
    if list(validator.iter_errors(record)):
        raise SystemExit("FAIL: un CompatibilityRecord completo no cumple el schema canónico")

    invalid_cases = []
    extra = copy.deepcopy(record)
    extra["engine_version"] = "inventado"
    invalid_cases.append(extra)
    bad_enum = copy.deepcopy(record)
    bad_enum["hard_gates"][0]["status"] = "MISSING"
    invalid_cases.append(bad_enum)
    missing_required = copy.deepcopy(record)
    missing_required.pop("backend_id")
    invalid_cases.append(missing_required)
    bad_hash = copy.deepcopy(record)
    bad_hash["hardware_fingerprint"] = "A" * 64
    invalid_cases.append(bad_hash)
    if any(not list(validator.iter_errors(case)) for case in invalid_cases):
        raise SystemExit("FAIL: el schema canónico aceptó un caso inválido")
    print("PASS: CompatibilityRecord válido y 4 mutaciones inválidas rechazadas por schema v3")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
