#!/usr/bin/env python3
"""P020 - Genera docs/adapters/schemas desde los schemas del paquete y valida los ejemplos.

  python tools/p020_schemas.py write   # regenera los schemas
  python tools/p020_schemas.py check   # al día + ejemplos válidos pasan e inválidos fallan

Los tipos salen de docs/contracts/schemas/larm.schema.json (fuente única): no se
redacta a mano ningún DTO. El frame NDJSON/JSON-RPC 2.0 se describe aquí con las
reglas del contrato 03 (jsonrpc 2.0, id UUIDv7, una respuesta por id, error con
code y data = LarmError).
"""
import copy
import json
import sys
from pathlib import Path

import jsonschema

REPO = Path(__file__).resolve().parent.parent
LARM = REPO / "docs/contracts/schemas/larm.schema.json"
DOMAIN = REPO / "docs/contracts/schemas/domain.schema.json"
OUT = REPO / "docs/adapters/schemas"
EXAMPLES = REPO / "docs/adapters/examples"
TYPES = ["AdapterManifest", "Handshake", "HandshakeResult", "CancelRequest", "CancelResult", "Heartbeat"]
UUID7 = "^[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$"


def larm_defs() -> dict:
    return json.loads(LARM.read_text(encoding="utf-8"))["$defs"]


def frame_schemas() -> dict:
    error = copy.deepcopy(larm_defs()["Error"])
    return {
        "frame.request": {
            "type": "object", "additionalProperties": False,
            "properties": {"jsonrpc": {"const": "2.0"}, "id": {"type": "string", "pattern": UUID7},
                           "method": {"type": "string", "minLength": 1}, "params": {}},
            "required": ["jsonrpc", "id", "method"],
        },
        "frame.response": {
            "type": "object", "additionalProperties": False,
            "properties": {
                "jsonrpc": {"const": "2.0"},
                "id": {"anyOf": [{"type": "string", "pattern": UUID7}, {"type": "null"}]},
                "result": {},
                "error": {"type": "object", "additionalProperties": False,
                          "properties": {"code": {"type": "integer", "enum": [-32700, -32600, -32601, -32602, -32000]},
                                         "message": {"type": "string"}, "data": error},
                          "required": ["code", "message"]},
            },
            "required": ["jsonrpc", "id"],
            "oneOf": [{"required": ["result"], "not": {"required": ["error"]}},
                      {"required": ["error"], "not": {"required": ["result"]}}],
        },
        "frame.progress": {
            "type": "object", "additionalProperties": False,
            "properties": {
                "jsonrpc": {"const": "2.0"}, "method": {"const": "core.progress"},
                "params": {"type": "object", "additionalProperties": False,
                           "properties": {"request_id": {"type": "string", "pattern": UUID7},
                                          "phase": {"type": "string", "minLength": 1},
                                          "completed_bytes": {"type": ["integer", "null"], "minimum": 0},
                                          "total_bytes": {"type": ["integer", "null"], "minimum": 0},
                                          "message_code": {"type": ["string", "null"]}},
                           "required": ["request_id", "phase", "completed_bytes", "total_bytes", "message_code"]},
            },
            "required": ["jsonrpc", "method", "params"],
        },
    }


def generated() -> dict:
    defs = larm_defs()
    out = {}
    for t in TYPES:
        schema = copy.deepcopy(defs[t])
        schema["$schema"] = "https://json-schema.org/draft/2020-12/schema"
        schema["$id"] = f"https://larm.local/adapters/{t}.schema.json"
        schema["description"] = f"{t} (generado desde docs/contracts/schemas/larm.schema.json; no editar)"
        out[f"{t}.schema.json"] = schema
    for name, schema in frame_schemas().items():
        schema = dict(schema)
        schema["$schema"] = "https://json-schema.org/draft/2020-12/schema"
        schema["$id"] = f"https://larm.local/adapters/{name}.schema.json"
        out[f"{name}.schema.json"] = schema
    return out


def render(schema: dict) -> str:
    return json.dumps(schema, indent=2, ensure_ascii=False, sort_keys=True) + "\n"


def validator(schema: dict) -> jsonschema.protocols.Validator:
    return jsonschema.Draft202012Validator(schema)


def write() -> str:
    OUT.mkdir(parents=True, exist_ok=True)
    for name, schema in generated().items():
        (OUT / name).write_text(render(schema), encoding="utf-8", newline="\n")
    return f"{len(generated())} schemas escritos en {OUT.relative_to(REPO)}"


def check() -> str:
    stale = [n for n, s in generated().items() if not (OUT / n).exists() or (OUT / n).read_text(encoding="utf-8") != render(s)]
    if stale:
        raise AssertionError(f"schemas desactualizados: {stale}; ejecutar tools/p020_schemas.py write")
    checked = 0
    for kind in ("valid", "invalid"):
        for f in sorted((EXAMPLES / kind).glob("*.json")):
            target = f.name.split("--")[0]
            schema = json.loads((OUT / f"{target}.schema.json").read_text(encoding="utf-8"))
            errors = list(validator(schema).iter_errors(json.loads(f.read_text(encoding="utf-8"))))
            if kind == "valid" and errors:
                raise AssertionError(f"{f.name} debería ser válido: {errors[0].message}")
            if kind == "invalid" and not errors:
                raise AssertionError(f"{f.name} debería ser inválido")
            checked += 1
    if checked < 10:
        raise AssertionError(f"pocos ejemplos ({checked})")
    return f"{len(generated())} schemas al día; {checked} ejemplos (válidos pasan, inválidos fallan)"


if __name__ == "__main__":
    sys.stdout.reconfigure(encoding="utf-8")
    try:
        print("PASS " + {"write": write, "check": check}[sys.argv[1]]())
        raise SystemExit(0)
    except (AssertionError, KeyError, OSError, ValueError) as e:
        print(f"FAIL {type(e).__name__}: {e}")
        raise SystemExit(1)
