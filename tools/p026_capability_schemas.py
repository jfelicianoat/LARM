#!/usr/bin/env python3
"""Schemas de las 18 capacidades publicadas, con hashes JCS para descriptores."""
import hashlib
import json
import sys

from p026_schemas import CONTRACTS, REPO, closed_schema, documents, render

sys.path.insert(0, str(REPO / "adapters/sdk/python"))
from larm_adapter_sdk.capabilities import digest  # noqa: E402

OUT = REPO / "docs/adapters/capability-schemas"
RUST = REPO / "crates/larm-adapter-sdk/src/capabilities/builtin.rs"


def generated():
    docs = documents()
    table = json.loads((CONTRACTS / "capability-contracts.json").read_text(encoding="utf-8"))
    if len(table["contracts"]) != 18 or len({c["id"] for c in table["contracts"]}) != 18:
        raise ValueError("Se requieren las 18 capacidades canónicas únicas")
    files, entries = {}, []
    for item in table["contracts"]:
        entry = {"id":item["id"],"version":item["version"]}
        for direction in ("input", "output"):
            schema = closed_schema(item[direction + "_schema"], docs)
            name = item["id"] + "." + direction + ".schema.json"
            files[name] = render(schema)
            entry[direction + "_sha256"] = digest(schema)
            entry[direction + "_file"] = name
        entries.append(entry)
    sources = {"capability-contracts.json":hashlib.sha256((CONTRACTS / "capability-contracts.json").read_bytes()).hexdigest(), **{name:hashlib.sha256((CONTRACTS / "schemas" / name).read_bytes()).hexdigest() for name in docs}}
    files["index.json"] = render({"schema_version":"1.0","sources":sources,"capabilities":entries})
    rust = "// Generado por tools/p026_capability_schemas.py; no editar.\n#[rustfmt::skip]\npub(super) const BUILTIN: &[(&str, &str, &str, &str)] = &[\n" + "".join(f'    ("{e["id"]}", "{e["version"]}", "{e["input_sha256"]}", "{e["output_sha256"]}"),\n' for e in entries) + "];\n"
    return files, rust


def main():
    files, rust = generated()
    mode = sys.argv[1] if len(sys.argv) == 2 else ""
    if mode == "write":
        OUT.mkdir(parents=True, exist_ok=True)
        for name, value in files.items():
            (OUT / name).write_text(value, encoding="utf-8", newline="\n")
        RUST.parent.mkdir(parents=True, exist_ok=True)
        RUST.write_text(rust, encoding="utf-8", newline="\n")
    elif mode != "check":
        raise ValueError("Uso: p026_capability_schemas.py write|check")
    if {p.name for p in OUT.glob("*.json")} != set(files) or any((OUT / name).read_bytes() != value.encode() for name, value in files.items()) or not RUST.is_file() or RUST.read_bytes() != rust.encode():
        raise ValueError("Schemas/índice de capacidad ausentes, extra o desactualizados")
    print("PASS: 18 capacidades / 36 schemas con hashes RFC8785 e índice Rust exactos")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
