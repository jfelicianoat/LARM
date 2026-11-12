#!/usr/bin/env python3
"""Genera docs/reports/P107_...evidence.json con el schema PromptEvidence.

Reúne tres fuentes, sin inventar ninguna:
- las comprobaciones reales de docs/reports/P107_real/checks.json (las escribe
  el runner del Broker, scripts/larm_p107_real.py);
- los resultados de las pruebas de código pasados por --code-results (JSON con
  la salida resumida de cargo/pytest que se acaba de ejecutar);
- los hashes del entorno (environment.lock.json) y de las entradas del paquete
  P107 (Contexto_Broker_P107_v3).

El estado lo decide la regla del contrato 00: REAL_PASS solo si todas las
pruebas reales obligatorias pasaron y no queda ningún bloqueo; si hay bloqueos
de entorno o dependencias, BLOCKED_REAL con el trabajo de código en CODE_PASS.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import platform
import sys
from datetime import datetime, timezone
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
REPORT_STEM = "P107_cambio-companero-en-ai-broker-localruntimeresolver-real"
PACKAGE_INPUTS = [
    "P107.md", "SHARED_CONTRACT.md", "manifest.json", "LARM_SPEC_V1.md",
    "contracts/00_PACKAGE.md", "contracts/11_CONFORMIDAD_V3.md", "contracts/06_ECOSYSTEM.md",
    "contracts/07_API_EVENTS_MCP.md", "contracts/defaults.json", "contracts/persistence.sql",
    "contracts/api-operations.json", "contracts/adapter-methods.json", "contracts/capability-contracts.json",
    "schemas/domain.schema.json", "schemas/larm.schema.json", "fixtures/benchmark-suites.json",
    "contracts/acceptance.json", "contracts/external/Client_API_Broker_2.11.md",
]


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def now() -> str:
    moment = datetime.now(timezone.utc)
    return moment.strftime("%Y-%m-%dT%H:%M:%S.") + f"{moment.microsecond // 1000:03d}Z"


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--package", type=Path, required=True, help="Contexto_Broker_P107_v3")
    parser.add_argument("--code-results", type=Path, required=True)
    parser.add_argument("--changed-files", type=Path, required=True)
    parser.add_argument("--blockers", type=Path, required=True)
    args = parser.parse_args()

    inputs = {name: sha256(args.package / name) for name in PACKAGE_INPUTS}
    inputs_hash = hashlib.sha256(json.dumps(inputs, sort_keys=True).encode()).hexdigest()
    real_dir = REPO / "docs/reports/P107_real"
    real_checks = json.loads((real_dir / "checks.json").read_text(encoding="utf-8"))
    sums = json.loads((real_dir / "SHA256SUMS.json").read_text(encoding="utf-8"))
    code = json.loads(args.code_results.read_text(encoding="utf-8"))
    blockers = json.loads(args.blockers.read_text(encoding="utf-8"))

    tests = []
    for item in code:
        tests.append({
            "id": item["id"], "kind": item["kind"], "command": item["command"],
            "fixture_hashes": item.get("fixture_hashes", []), "expected": item["expected"],
            "observed": item["observed"], "result": item["result"], "evidence_paths": item.get("evidence_paths", []),
        })
    for check in real_checks:
        tests.append({
            "id": check["id"], "kind": "REAL",
            "command": "python scripts/larm_p107_real.py --larm-repo <larm> --model gemma4:12b --out docs/reports/P107_real",
            "fixture_hashes": [f"{name}:{sums[name]}" for name in check["evidence_paths"] if name in sums],
            "expected": check["expected"], "observed": check["observed"], "result": check["result"],
            "evidence_paths": [f"docs/reports/P107_real/{name}" for name in check["evidence_paths"]],
        })
    # Un rojo que ya existía antes de P107 (marcado `preexisting` en los
    # resultados de código) se publica tal cual y figura en `blockers`, pero no
    # decide el estado de este prompt; un BLOCKED de entorno tampoco es un FAIL.
    preexisting = {item["id"] for item in code if item.get("preexisting")}
    all_pass = all(t["result"] in {"PASS", "BLOCKED"} for t in tests if t["id"] not in preexisting)
    status = "REAL_PASS" if all_pass and not blockers else ("BLOCKED_REAL" if all_pass else "BLOCKED")
    evidence = {
        "prompt_id": "P107",
        "status": status,
        "host_id": f"{platform.node()} ({platform.platform()})",
        "environment_lock_hash": sha256(REPO / "environment.lock.json"),
        "inputs_hash": inputs_hash,
        "changed_files": json.loads(args.changed_files.read_text(encoding="utf-8")),
        "tests": tests,
        "blockers": blockers,
        "generated_at": now(),
    }
    validate(evidence, args.package)
    out = REPO / f"docs/reports/{REPORT_STEM}.evidence.json"
    out.write_text(json.dumps(evidence, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    (REPO / "docs/reports/P107_inputs.json").write_text(json.dumps(inputs, indent=2) + "\n", encoding="utf-8")
    print(f"{status}: {sum(t['result'] == 'PASS' for t in tests)}/{len(tests)} PASS, {len(blockers)} bloqueos -> {out}")
    return 0


def validate(evidence: dict, package: Path) -> None:
    try:
        from jsonschema import Draft202012Validator
        from referencing import Registry, Resource
    except ImportError:
        print("jsonschema no disponible: no se valida el schema", file=sys.stderr)
        raise SystemExit(2) from None
    registry = Registry()
    for name in ("larm.schema.json", "domain.schema.json"):
        registry = registry.with_resource(name, Resource.from_contents(json.loads((package / "schemas" / name).read_text(encoding="utf-8"))))
    validator = Draft202012Validator({"$ref": "larm.schema.json#/$defs/PromptEvidence"}, registry=registry)
    errors = list(validator.iter_errors(evidence))
    if errors:
        raise SystemExit(f"evidence.json no cumple PromptEvidence: {errors[0].message}")


if __name__ == "__main__":
    raise SystemExit(main())
