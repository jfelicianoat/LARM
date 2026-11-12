#!/usr/bin/env python3
"""Genera schemas cerrados por firma, sin resolver recursos externos en runtime.

Reescribe únicamente referencias a los $defs canónicos alcanzables, preservando
todas las restricciones. No es un implementador propio de JSON Schema ni una
cualificación de workers. write genera; check exige bytes exactos y meta-schema.
"""
import copy
import hashlib
import json
import sys
from pathlib import Path

from jsonschema import Draft202012Validator

REPO = Path(__file__).resolve().parent.parent
CONTRACTS = REPO / "docs/contracts"
OUT = REPO / "docs/adapters/method-schemas"
RUST = REPO / "crates/larm-adapter-sdk/src/contracts/generated.rs"
FILES = ("larm.schema.json", "domain.schema.json")


def render(value):
    return json.dumps(value, ensure_ascii=False, sort_keys=True, indent=2) + "\n"


def sha(value):
    return hashlib.sha256(value).hexdigest()


def documents():
    return {name: json.loads((CONTRACTS / "schemas" / name).read_text(encoding="utf-8")) for name in FILES}


def closed_schema(reference, docs):
    definitions, pending = {}, []

    def rewrite_ref(value, current):
        document, marker, fragment = value.partition("#")
        document = document or current
        if document not in docs or not marker or not fragment.startswith("/$defs/"):
            raise ValueError("Referencia fuera de los contratos locales")
        name = fragment[len("/$defs/"):]
        if not name or "/" in name or name not in docs[document]["$defs"]:
            raise ValueError("Referencia no resuelta a un $def completo")
        key = document.removesuffix(".schema.json") + "__" + name
        if key not in definitions:
            definitions[key] = None  # reserva antes de recorrer posibles ciclos
            pending.append((document, name, key))
        return "#/$defs/" + key

    def rewrite(node, current):
        if isinstance(node, list):
            return [rewrite(item, current) for item in node]
        if not isinstance(node, dict):
            return copy.deepcopy(node)
        if any(key in node for key in ("$id", "$dynamicRef", "$recursiveRef", "$anchor", "$dynamicAnchor")):
            raise ValueError("Semántica de scope no contemplada: no se genera un schema permisivo")
        return {key: rewrite_ref(value, current) if key == "$ref" else rewrite(value, current) for key, value in node.items()}

    root_ref = rewrite_ref(reference, "larm.schema.json")
    while pending:
        document, name, key = pending.pop()
        definitions[key] = rewrite(docs[document]["$defs"][name], document)
    result = {"$schema": "https://json-schema.org/draft/2020-12/schema", "$ref": root_ref, "$defs": definitions}
    Draft202012Validator.check_schema(result)
    return result


def generated():
    docs = documents()
    table_path = CONTRACTS / "adapter-methods.json"
    table = json.loads(table_path.read_text(encoding="utf-8"))
    methods = table["methods"]
    if len(methods) != 40 or len({entry["method"] for entry in methods}) != 40:
        raise ValueError("La tabla publicada no contiene las 40 firmas únicas esperadas")
    output, entries = {}, []
    for entry in methods:
        files = {}
        for direction in ("request", "response"):
            name = f"{entry['method']}.{direction}.schema.json"
            text = render(closed_schema(entry[direction + "_schema"], docs))
            output[name] = text
            files[direction] = {"file": name, "sha256": sha(text.encode()), "canonical_ref": entry[direction + "_schema"]}
        entries.append({"method": entry["method"], "family": entry["family"], "required": entry["required"], **files})
    index = {"schema_version": "1.0", "scope": "generated method schemas only; no worker qualification or capability estimator evidence", "sources": {"adapter-methods.json": sha(table_path.read_bytes()), **{name: sha((CONTRACTS / "schemas" / name).read_bytes()) for name in FILES}}, "methods": entries}
    output["index.json"] = render(index)
    return output


def rust_source(output):
    index = json.loads(output["index.json"])
    lines = ["// Generado por tools/p026_schemas.py; no editar.", "#[rustfmt::skip]", "pub(super) const SCHEMAS: &[(&str, &str, &str)] = &["]
    for entry in index["methods"]:
        paths = [f'include_str!("../../../../docs/adapters/method-schemas/{entry[k]["file"]}")' for k in ("request", "response")]
        lines.append(f'    ("{entry["method"]}", {paths[0]}, {paths[1]}),')
    return "\n".join(lines + ["];", ""])


def main():
    expected = generated()
    mode = sys.argv[1] if len(sys.argv) == 2 else ""
    if mode == "write":
        OUT.mkdir(parents=True, exist_ok=True)
        for name, value in expected.items():
            (OUT / name).write_text(value, encoding="utf-8", newline="\n")
        RUST.parent.mkdir(parents=True, exist_ok=True)
        RUST.write_text(rust_source(expected), encoding="utf-8", newline="\n")
    elif mode != "check":
        raise ValueError("Uso: tools/p026_schemas.py write|check")
    actual = {path.name for path in OUT.glob("*.json")}
    if actual != set(expected) or any((OUT / name).read_bytes() != value.encode() for name, value in expected.items()):
        raise ValueError("Schemas ausentes, extra o desactualizados: regenerar, sin omitir ficheros")
    if not RUST.is_file() or RUST.read_bytes() != rust_source(expected).encode():
        raise ValueError("Índice Rust ausente o desactualizado")
    print("PASS: 40 firmas / 80 schemas autocontenidos, índice y hashes exactos; todas las restricciones originales conservadas")
    return 0


if __name__ == "__main__":
    sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    raise SystemExit(main())
