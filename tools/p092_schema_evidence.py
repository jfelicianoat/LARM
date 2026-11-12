"""Independent oracle for the existing evaluation_reference wire schema."""
import copy
import json
from pathlib import Path

from jsonschema import Draft202012Validator

ROOT = Path(__file__).resolve().parents[1]
domain = json.loads((ROOT / "docs/contracts/schemas/domain.schema.json").read_bytes())
schema = domain["$defs"]["evaluation_reference"]
validator = Draft202012Validator(schema)
fixture = json.loads((ROOT / "tests/fixtures/evaluation-reference.json").read_bytes())
validator.validate(fixture)
checks = 1
for nullable in ("score", "signature"):
    value = copy.deepcopy(fixture)
    value[nullable] = None
    validator.validate(value)
    checks += 1
for field in schema["required"]:
    value = copy.deepcopy(fixture)
    del value[field]
    assert list(validator.iter_errors(value)), field
    checks += 1
for path, value in [
    (("popularity_score",), 99), (("scale", "invented"), True),
    (("status",), "CERTIFIED"), (("scale", "direction"), "AUTO"),
    (("suite_hash",), "A" * 64), (("score",), "75"),
]:
    bad = copy.deepcopy(fixture)
    target = bad
    for part in path[:-1]:
        target = target[part]
    target[path[-1]] = value
    assert list(validator.iter_errors(bad)), path
    checks += 1
print(json.dumps({"status": "PASS", "checks": checks,
                  "scope": "Existing wire representation only; numerical scale and comparability are Rust behavior tests, no authenticated external import"}))
