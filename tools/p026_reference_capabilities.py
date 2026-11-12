#!/usr/bin/env python3
"""Descriptores de referencia desde contratos/fixtures; no son cualificación."""
import importlib.util
import json
import sys
from pathlib import Path

REPO=Path(__file__).resolve().parent.parent
sys.path.insert(0,str(REPO/"adapters/sdk/python"))
from larm_adapter_sdk import Contracts  # noqa: E402
from larm_adapter_sdk.capabilities import digest  # noqa: E402

BINDINGS={
    "source.search":("source.search","reference.source.search.v1"),
    "source.revisions":("source.list_revisions","reference.source.revisions.v1"),
    "source.resume":("source.download","reference.source.range-download.v1"),
    "source.sha256":("source.get_manifest","reference.source.sha256-manifest.v1"),
    "source.sync":("source.sync","reference.source.sync.v1"),
}


def generated():
    contracts=Contracts(REPO/"docs")
    fixture=json.loads((REPO/"tests/fixtures/source/protocol.json").read_text(encoding="utf-8"))
    entries=[]
    for identity,(method,mapping) in BINDINGS.items():
        schemas=[json.loads((REPO/"docs/adapters/method-schemas"/f"{method}.{direction}.schema.json").read_text(encoding="utf-8")) for direction in ("request","response")]
        witness={"input":fixture["requests"][method],"output":fixture["responses"][method]}
        contracts.request(method,witness["input"]); contracts.response(method,witness["output"])
        entries.append({"id":identity,"version":"1.0","input_schema":schemas[0],"output_schema":schemas[1],"input_sha256":digest(schemas[0]),"output_sha256":digest(schemas[1]),"mapping":mapping,"estimator_id":"reference.source.buffers.v1","fixture":witness,"fixture_sha256":digest(witness)})
    spec=importlib.util.spec_from_file_location("reference_protocol_descriptor",REPO/"adapters/protocol/reference/package.py")
    protocol=importlib.util.module_from_spec(spec); spec.loader.exec_module(protocol)
    return {
        REPO/"adapters/source/reference/capabilities.json":{"schema_version":"1.0","entries":entries},
        REPO/"adapters/runtime/reference/capabilities.json":{"schema_version":"1.0","entries":[protocol.descriptor()]},
    }


def main():
    mode=sys.argv[1] if len(sys.argv)==2 else ""
    if mode not in ("write","check"): raise ValueError("Uso: p026_reference_capabilities.py write|check")
    for path,document in generated().items():
        expected=(json.dumps(document,ensure_ascii=False,indent=2,sort_keys=True)+"\n").encode()
        if mode=="write": path.write_bytes(expected)
        if not path.is_file() or path.read_bytes()!=expected: raise ValueError("Descriptores de referencia ausentes o desactualizados")
    print("PASS: seis descriptores fijados (runtime echo y cinco operaciones source); fixtures sólo de contrato")
    return 0


if __name__=="__main__": raise SystemExit(main())
