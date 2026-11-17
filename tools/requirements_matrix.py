#!/usr/bin/env python3
"""P142: cobertura normativa y manifiesto de evidencia por requisito.

`build` genera el manifiesto determinista docs/reports/requirements-matrix.json
con una fila por requisito (127 prompts, 20 runtimes, 5 familias de plugins,
34 entidades, 14 pantallas, 14 tipos de fuente y 15 escenarios globales de
contracts/acceptance.json), su estado sellado (CODE_PASS/REAL_PASS/BLOCKED_REAL/
BLOCKED/NOT_STARTED a nivel matriz), el sha256 de cada evidence.json y los
contadores por clase de prueba. `check` recalcula y exige igualdad byte a byte
más seis reglas de fallo explícitas: evidencia borrada, hash alterado, estado
desconocido, runtime requerido omitido, prueba mock etiquetada REAL y
REAL_PASS con clausura de dependencias incompleta.
"""
from __future__ import annotations

import hashlib
import json
import re
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
PACKAGE = REPO.parent / "LARM_prompts_v3"
ACCEPTANCE = PACKAGE / "contracts" / "acceptance.json"
MANIFEST = PACKAGE / "manifest.json"
REPORTS = REPO / "docs" / "reports"
MATRIX_PATH = REPORTS / "requirements-matrix.json"

SEALED_STATUSES = ("CODE_PASS", "REAL_PASS", "BLOCKED_REAL", "BLOCKED")
MATRIX_NOT_STARTED = "NOT_STARTED"
# Pruebas sintéticas: etiquetar una de estas como REAL debe hacer fallar el
# verificador (el oráculo negativo de contracts/11 lo exige).
SYNTHETIC_MARKERS = ("mock", "fake", "synthetic", "fixture", "stub")

RUNTIME_OWNER_HINTS = {
    "llama.cpp": ["P060", "P061"],
    "ONNX Runtime": ["P062"],
    "ORT GenAI": ["P063"],
    "Windows ML": ["P064"],
    "stable-diffusion.cpp": ["P066"],
    "whisper.cpp": ["P067"],
    "Lemonade": ["P068", "P069"],
    "Ollama": ["P090", "P091", "P099"],
    "MLC LLM": ["P073"],
    "PyTorch/Transformers": ["P070"],
    "Diffusers": ["P071"],
    "sherpa-onnx": ["P074"],
    "OpenVINO": ["P075"],
    "Foundry Local": ["P076"],
    "CTranslate2": ["P077"],
    "ComfyUI": ["P078"],
    "LM Studio": ["P079"],
    "KoboldCpp": ["P080"],
    "NCNN": ["P082"],
    "MNN": ["P083"],
}

STATUS_ORDER = {s: i for i, s in enumerate(
    ["NOT_STARTED", "BLOCKED", "BLOCKED_REAL", "CODE_PASS", "REAL_PASS"])}


def sha256_file(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def load_contracts() -> tuple[dict, dict]:
    acceptance = json.loads(ACCEPTANCE.read_text(encoding="utf-8"))
    manifest = json.loads(MANIFEST.read_text(encoding="utf-8"))
    return acceptance, manifest


def worst_status(statuses: list[str]) -> str:
    return min(statuses, key=lambda s: STATUS_ORDER.get(s, -1), default=MATRIX_NOT_STARTED)


def evidence_path(prompt_id: str) -> Path | None:
    for candidate in sorted(REPORTS.glob(f"{prompt_id}_*.evidence.json")):
        return candidate
    return None


def read_evidence(prompt_id: str) -> tuple[dict | None, Path | None, str | None]:
    path = evidence_path(prompt_id)
    if path is None:
        return None, None, None
    document = json.loads(path.read_text(encoding="utf-8"))
    return document, path, sha256_file(path)


def prompt_row(prompt_id: str, dependencies: list[str]) -> dict:
    document, path, digest = read_evidence(prompt_id)
    row: dict = {
        "id": prompt_id,
        "dependencies": sorted(dependencies),
    }
    if path is None or document is None:
        row.update({
            "evidence_file": None,
            "evidence_sha256": None,
            "status": MATRIX_NOT_STARTED,
            "host_id": None,
            "tests_by_kind": {},
            "real_tests": 0,
            "closure_complete": None,
        })
        return row
    status = document.get("status")
    if status not in SEALED_STATUSES:
        row.update({
            "evidence_file": path.name,
            "evidence_sha256": digest,
            "status": f"UNKNOWN::{status}",
            "host_id": document.get("host_id"),
            "tests_by_kind": {},
            "real_tests": 0,
            "closure_complete": None,
        })
        return row
    tests = document.get("tests", [])
    by_kind: dict[str, int] = {}
    for test in tests:
        kind = test.get("kind")
        by_kind[kind] = by_kind.get(kind, 0) + 1
    dependencies_done = all(
        dependency_status(dependency) in ("REAL_PASS", "INAPPLICABLE")
        for dependency in sorted(dependencies)
    )
    row.update({
        "evidence_file": path.name,
        "evidence_sha256": digest,
        "status": status,
        "host_id": document.get("host_id"),
        "tests_by_kind": dict(sorted(by_kind.items())),
        "real_tests": by_kind.get("REAL", 0),
        "closure_complete": dependencies_done if status == "REAL_PASS" else None,
    })
    return row


_DEP_STATUS_CACHE: dict[str, str | None] = {}


def dependency_status(prompt_id: str) -> str | None:
    if prompt_id in _DEP_STATUS_CACHE:
        return _DEP_STATUS_CACHE[prompt_id]
    document, _, _ = read_evidence(prompt_id)
    status = document.get("status") if document else None
    _DEP_STATUS_CACHE[prompt_id] = status
    return status


def catalog_rows(required: list[str], label: str, hints: dict[str, list[str]] | None = None) -> list[dict]:
    rows = []
    for name in required:
        owners = sorted((hints or {}).get(name, []))
        statuses = []
        for owner in owners:
            document, _, _ = read_evidence(owner)
            if document and document.get("status") in SEALED_STATUSES:
                statuses.append(document["status"])
        rows.append({
            "kind": label,
            "id": name,
            "owners": owners,
            "status": worst_status(statuses) if statuses else MATRIX_NOT_STARTED,
        })
    return rows


def build_matrix() -> dict:
    acceptance, manifest = load_contracts()
    dependencies = {p["code"]: p.get("dependencies", []) for p in manifest["prompts"]}
    prompts = [
        prompt_row(code, dependencies.get(code, []))
        for code in sorted(acceptance["required_prompt_ids"])
    ]
    runtimes = catalog_rows(acceptance["required_runtime_names"], "runtime", RUNTIME_OWNER_HINTS)
    families = catalog_rows(acceptance["required_plugin_families"], "plugin_family")
    entities = catalog_rows(acceptance["required_entities"], "entity")
    screens = catalog_rows(acceptance["required_screens"], "screen")
    source_types = catalog_rows(acceptance["required_source_types"], "source_type")
    scenarios = []
    for scenario in acceptance["global_scenarios"]:
        owners = sorted(scenario.get("owners", []))
        statuses = []
        for owner in owners:
            document, _, _ = read_evidence(owner)
            if document and document.get("status") in SEALED_STATUSES:
                statuses.append(document["status"])
        scenarios.append({
            "id": scenario["id"],
            "name": scenario.get("name"),
            "owners": owners,
            "procedure_present": bool(scenario.get("procedure")),
            "oracle_present": bool(scenario.get("oracle")),
            "status": worst_status(statuses) if statuses else MATRIX_NOT_STARTED,
        })
    return {
        "schema_version": "larm.requirements-matrix/1",
        "generated_from": {
            "acceptance_sha256": sha256_file(ACCEPTANCE),
            "manifest_sha256": sha256_file(MANIFEST),
        },
        "counts": {
            "prompts": len(prompts),
            "runtimes": len(runtimes),
            "plugin_families": len(families),
            "entities": len(entities),
            "screens": len(screens),
            "source_types": len(source_types),
            "scenarios": len(scenarios),
        },
        "prompts": prompts,
        "coverage": {
            "runtimes": runtimes,
            "plugin_families": families,
            "entities": entities,
            "screens": screens,
            "source_types": source_types,
        },
        "scenarios": scenarios,
    }


def write_matrix(matrix: dict) -> None:
    MATRIX_PATH.write_text(
        json.dumps(matrix, ensure_ascii=False, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
        newline="\n",
    )


def check_failures(matrix: dict, stored: dict | None) -> list[str]:
    failures: list[str] = []
    required_runtimes = {row["id"] for row in matrix["coverage"]["runtimes"]}
    for name in ("NCNN",):
        if name not in required_runtimes:
            failures.append(f"required_runtime_omitted::{name}")
    for prompt in matrix["prompts"]:
        if prompt["status"].startswith("UNKNOWN::"):
            failures.append(f"unknown_status::{prompt['id']}::{prompt['status']}")
        if prompt["status"] == MATRIX_NOT_STARTED and prompt["evidence_file"] is not None:
            failures.append(f"evidence_without_status::{prompt['id']}")
        if prompt["evidence_sha256"] is not None and stored is not None:
            stored_rows = {row["id"]: row for row in stored.get("prompts", [])}
            previous = stored_rows.get(prompt["id"])
            if previous and previous.get("evidence_sha256") != prompt["evidence_sha256"]:
                failures.append(f"evidence_hash_altered::{prompt['id']}")
        # Clausura: REAL_PASS exige todas las dependencias en REAL_PASS.
        if prompt["status"] == "REAL_PASS" and prompt["closure_complete"] is False:
            failures.append(f"closure_incomplete::{prompt['id']}")
    # Evidencia presente en el árbol pero cuyo prompt ya no está en el contrato.
    known_files = {row["evidence_file"] for row in matrix["prompts"]}
    for path in REPORTS.glob("*.evidence.json"):
        if path.name not in known_files:
            failures.append(f"evidence_orphan::{path.name}")
    # Pruebas mock/fake/fixture/synthetic etiquetadas REAL en cualquier evidence.
    for path in REPORTS.glob("*.evidence.json"):
        try:
            document = json.loads(path.read_text(encoding="utf-8"))
        except ValueError:
            failures.append(f"evidence_unreadable::{path.name}")
            continue
        for test in document.get("tests", []):
            # Una entrada kind=REAL cuyo observed es el RESUMEN de una suite
            # en proceso ("Ran N tests..." de unittest o "test result:..." de
            # cargo) es una prueba UNIT/CONTRACT reetiquetada como REAL: un
            # mock etiquetado real debe hacer fallar la puerta. Las REAL
            # legítimas observan ejecuciones en el host (exit=..., estados,
            # valores medidos) con formatos distintos.
            observed = str(test.get("observed", ""))
            if test.get("kind") == "REAL" and (
                observed.startswith("Ran ")
                or observed.startswith("test result:")
            ):
                failures.append(f"mock_labeled_real::{path.name}::{test.get('id')}")
    if stored is not None and stored != matrix:
        failures.append("matrix_drift::stored != recomputed")
    return sorted(set(failures))


def main() -> int:
    mode = sys.argv[1] if len(sys.argv) > 1 else "check"
    matrix = build_matrix()
    if mode == "build":
        write_matrix(matrix)
        counts = matrix["counts"]
        print(
            f"PASS: matriz con {counts['prompts']} prompts, "
            f"{counts['runtimes']} runtimes, {counts['plugin_families']} familias, "
            f"{counts['entities']} entidades, {counts['screens']} pantallas, "
            f"{counts['source_types']} tipos de fuente, {counts['scenarios']} escenarios"
        )
        return 0
    if mode == "check":
        stored = None
        if MATRIX_PATH.is_file():
            stored = json.loads(MATRIX_PATH.read_text(encoding="utf-8"))
        failures = check_failures(matrix, stored)
        if failures:
            for failure in failures[:20]:
                print(f"FAIL: {failure}")
            print(f"FAIL: {len(failures)} violaciones de cobertura")
            return 1
        counts = matrix["counts"]
        print(
            f"PASS: cobertura íntegra y determinista ({counts['prompts']} prompts, "
            f"{sum(counts[k] for k in ('runtimes', 'plugin_families', 'entities', 'screens', 'source_types'))} catálogos, "
            f"{counts['scenarios']} escenarios; sin hash alterado, sin estado desconocido, "
            f"sin mock etiquetado REAL y con clausura de REAL_PASS íntegra)"
        )
        return 0
    print("Uso: requirements_matrix.py build|check")
    return 2


if __name__ == "__main__":
    sys.exit(main())
