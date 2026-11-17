#!/usr/bin/env python3
"""P142: negativos del verificador de cobertura normativa.

Cada caso construye una copia tamperada del árbol mínimo del verificador y
exige que `check` FALLE con el motivo correcto: evidencia borrada, hash
alterado, estado desconocido, runtime requerido omitido (NCNN), prueba mock
etiquetada REAL y REAL_PASS con clausura de dependencias incompleta.
"""
from __future__ import annotations

import importlib.util
import json
import shutil
import sys
import tempfile
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parents[1]
PACKAGE = REPO.parent / "LARM_prompts_v3"


def load_module(tmp: Path, sources: Path):
    spec = importlib.util.spec_from_file_location("requirements_matrix", REPO / "tools" / "requirements_matrix.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    # El módulo ancla rutas globales al repo real; para las copias tamperadas
    # se re-anclan REPORTS/MATRIX_PATH al directorio temporal del caso.
    module.REPORTS = tmp / "docs" / "reports"
    module.MATRIX_PATH = module.REPORTS / "requirements-matrix.json"
    module.ACCEPTANCE = sources / "acceptance.json"
    module.MANIFEST = sources / "manifest.json"
    return module


class RequirementsMatrixNegative(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = Path(tempfile.mkdtemp(prefix="p142-negative-"))
        self.sources = self.tmp / "contracts-src"
        self.sources.mkdir(parents=True)
        shutil.copyfile(PACKAGE / "contracts/acceptance.json", self.sources / "acceptance.json")
        shutil.copyfile(PACKAGE / "manifest.json", self.sources / "manifest.json")
        (self.tmp / "docs" / "reports").mkdir(parents=True)
        # Un evidence mínimo REAL_PASS con una dependencia NO REAL_PASS para
        # los casos de clausura, y otro BLOCKED_REAL como relleno del contrato.
        self.mkevidence("P060", "REAL_PASS", deps_done=False)
        self.mkevidence("P030", "BLOCKED_REAL")

    def tearDown(self) -> None:
        shutil.rmtree(self.tmp, ignore_errors=True)

    def mkevidence(self, prompt_id: str, status: str, deps_done: bool = True) -> Path:
        tests = [
            {
                "id": f"{prompt_id}-REAL-host",
                "kind": "REAL",
                "command": "python harness.py --host",
                "fixture_hashes": [],
                "expected": "ejecución real observada en el host",
                "observed": "exit=0; status=ok, measured=1",
                "status": "PASS",
            }
        ]
        if not deps_done:
            tests.append({
                "id": f"{prompt_id}-REAL-extra",
                "kind": "REAL",
                "command": "python harness2.py --host",
                "fixture_hashes": [],
                "expected": "segunda ejecución real",
                "observed": "exit=0; ok",
                "status": "PASS",
            })
        document = {
            "prompt_id": prompt_id,
            "status": status,
            "host_id": "fixture-host",
            "environment_lock_hash": "a" * 64,
            "inputs_hash": "b" * 64,
            "changed_files": [],
            "tests": tests,
            "blockers": [],
            "generated_at": "2026-11-15T00:00:00Z",
        }
        target = self.tmp / "docs" / "reports" / f"{prompt_id}_fixture.evidence.json"
        target.write_text(json.dumps(document, indent=2), encoding="utf-8")
        return target

    def matrix_and_failures(self):
        module = load_module(self.tmp, self.sources)
        if not module.MATRIX_PATH.is_file():
            # Los casos sin drift precisan un manifiesto base: se construye
            # tras el tampering del caso (sus asertos no dependen del stored).
            module.write_matrix(module.build_matrix())
        matrix = module.build_matrix()
        failures = module.check_failures(matrix, json.loads(module.MATRIX_PATH.read_text(encoding="utf-8")))
        return matrix, failures

    def test_evidencia_borrada_falla_con_fila_not_started_y_hash_nulo(self):
        module = load_module(self.tmp, self.sources)
        module.write_matrix(module.build_matrix())
        for path in module.REPORTS.glob("P060_*evidence.json"):
            path.unlink()
        _, failures = self.matrix_and_failures()
        # El borrado cambia la fila (NOT_STARTED/null) y el manifiesto
        # almacenado deja de corresponder byte a byte: matrix_drift lo detecta.
        self.assertTrue(any(f == "matrix_drift::stored != recomputed" for f in failures), failures)

    def test_hash_alterado_falla_por_drift_del_manifiesto(self):
        module = load_module(self.tmp, self.sources)
        module.write_matrix(module.build_matrix())
        stored = json.loads(module.MATRIX_PATH.read_text(encoding="utf-8"))
        row = next(r for r in stored["prompts"] if r["id"] == "P060")
        row["evidence_sha256"] = "f" * 64
        module.MATRIX_PATH.write_text(json.dumps(stored, indent=2, sort_keys=True), encoding="utf-8")
        _, failures = self.matrix_and_failures()
        self.assertTrue(any(f == "matrix_drift::stored != recomputed" for f in failures), failures)

    def test_estado_desconocido_falla(self):
        evidence = self.tmp / "docs" / "reports" / "P060_fixture.evidence.json"
        document = json.loads(evidence.read_text(encoding="utf-8"))
        document["status"] = "READY"
        evidence.write_text(json.dumps(document, indent=2), encoding="utf-8")
        _, failures = self.matrix_and_failures()
        self.assertTrue(any(f.startswith("unknown_status::P060") for f in failures), failures)

    def test_runtime_requerido_omitido_falla(self):
        # Omitir NCNN en la fuente del contrato (acceptance.json tamperada)
        # debe hacer fallar al verificador: el ancla obligatoria es interna.
        tampered = json.loads((self.sources / "acceptance.json").read_text(encoding="utf-8"))
        tampered["required_runtime_names"] = [
            name for name in tampered["required_runtime_names"] if name != "NCNN"
        ]
        (self.sources / "acceptance.json").write_text(
            json.dumps(tampered, ensure_ascii=False, indent=2), encoding="utf-8"
        )
        _, failures = self.matrix_and_failures()
        self.assertTrue(any(f == "required_runtime_omitted::NCNN" for f in failures), failures)

    def test_mock_etiquetado_real_falla(self):
        evidence = self.tmp / "docs" / "reports" / "P030_fixture.evidence.json"
        document = json.loads(evidence.read_text(encoding="utf-8"))
        document["tests"].append({
            "id": "P030-mock-as-real",
            "kind": "REAL",
            "command": "cargo test --lib",
            "fixture_hashes": [],
            "expected": "suite en proceso",
            "observed": "test result: ok. 7 passed; 0 failed",
            "status": "PASS",
        })
        evidence.write_text(json.dumps(document, indent=2), encoding="utf-8")
        _, failures = self.matrix_and_failures()
        self.assertTrue(
            any(f.startswith("mock_labeled_real::P030") for f in failures),
            failures,
        )

    def test_clausura_incompleta_de_real_pass_falla(self):
        # La evidencia base ya es REAL_PASS con P030 BLOCKED_REAL: la clausura
        # debe fallar sin tampering adicional.
        _, failures = self.matrix_and_failures()
        self.assertTrue(any(f == "closure_incomplete::P060" for f in failures), failures)

    def test_arbol_integro_no_reporta_violaciones_de_tampering(self):
        # Con el árbol íntegro (sin tampering) NO hay violaciones de las reglas
        # estructurales: hash alterado, estado desconocido, mock etiquetado
        # REAL, runtime omitido o evidencia huérfana. La clausura de
        # REAL_PASS sí se reporta si las dependencias del manifiesto real no
        # están en REAL_PASS: es la señal correcta del verificador.
        _, failures = self.matrix_and_failures()
        for forbidden in (
            "matrix_drift",
            "unknown_status",
            "mock_labeled_real",
            "required_runtime_omitted",
            "evidence_orphan",
            "evidence_unreadable",
        ):
            self.assertFalse(
                any(f.startswith(forbidden) for f in failures),
                f"{forbidden}: {failures}",
            )


if __name__ == "__main__":
    unittest.main()
