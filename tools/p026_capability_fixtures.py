#!/usr/bin/env python3
"""Mismos descriptores y vectores JCS para ambos SDK; no cualifica una capacidad."""
import hashlib
import importlib.util
import json
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(REPO / "adapters/sdk/python"))
from larm_adapter_sdk.capabilities import digest  # noqa: E402
from p026_schemas import render  # noqa: E402

TARGET = REPO / "tests/fixtures/adapters/capability-registry.json"


def generated():
    spec = importlib.util.spec_from_file_location("protocol_package", REPO / "adapters/protocol/reference/package.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    echo = module.descriptor()
    schemas = REPO / "docs/adapters/capability-schemas"
    input_schema = json.loads((schemas / "embedding.create.input.schema.json").read_text(encoding="utf-8"))
    output_schema = json.loads((schemas / "embedding.create.output.schema.json").read_text(encoding="utf-8"))
    fixture = {"input":{"inputs":["fixture schema only"]},"output":{"dimension":2,"vectors":[[0.25,-0.5]],"normalized":False}}
    embedding = {"id":"embedding.create","version":"1.0","input_schema":input_schema,"output_schema":output_schema,"input_sha256":digest(input_schema),"output_sha256":digest(output_schema),"mapping":"fixture.schema.only","estimator_id":"fixture.unknown","fixture":fixture,"fixture_sha256":digest(fixture)}
    vectors = [
        ({"b":1,"a":[True,None,"x"],"😀":2,"€":3,"\r":4}, '{"\\r":4,"a":[true,null,"x"],"b":1,"€":3,"😀":2}'),
        ({"t":0.7,"p":1.0,"n":-7}, '{"n":-7,"p":1,"t":0.7}'),
        ({"value":9007199254740992}, '{"value":9007199254740992}'),
        ({"value":-9007199254740992}, '{"value":-9007199254740992}'),
        ({"numbers":[-0.0,1e-6,1e-7,1e20,1e21,1e23,333333333.3333333,0.1,0.002]}, '{"numbers":[0,0.000001,1e-7,100000000000000000000,1e+21,1e+23,333333333.3333333,0.1,0.002]}'),
        ({"value":1.7976931348623157e308}, '{"value":1.7976931348623157e+308}'),
        ({"value":5e-324}, '{"value":5e-324}'),
        ({"𐀀":1,"":2}, '{"𐀀":1,"":2}'),
    ]
    return render({"schema_version":"1.0","scope":"descriptor/schema/JCS corpus only, not inference qualification","echo":echo,"embedding":embedding,"jcs_vectors":[{"value":value,"canonical":text,"sha256":hashlib.sha256(text.encode()).hexdigest()} for value,text in vectors]})


def main():
    data = generated()
    mode = sys.argv[1] if len(sys.argv) == 2 else ""
    if mode == "write":
        TARGET.write_text(data, encoding="utf-8", newline="\n")
    elif mode != "check":
        raise ValueError("Uso: p026_capability_fixtures.py write|check")
    if not TARGET.is_file() or TARGET.read_bytes() != data.encode():
        raise ValueError("Corpus de capacidades ausente o desactualizado")
    print("PASS: dos descriptores compartidos y ocho vectores JCS independientes")
    return 0


if __name__ == "__main__":
    sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    raise SystemExit(main())
