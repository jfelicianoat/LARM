from __future__ import annotations

import hashlib
import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
import venv
from pathlib import Path

from jsonschema import Draft202012Validator


REPO = Path(__file__).resolve().parents[4]
WORKER_DIR = Path(__file__).resolve().parents[1]
WORKER = WORKER_DIR / "worker.py"
SCHEMA = json.loads((WORKER_DIR / "protocol.schema.json").read_text(encoding="utf-8"))
VALIDATOR = Draft202012Validator(SCHEMA)
FIXTURE = WORKER_DIR / "fixtures" / "tensor_identity.onnx"
FIXTURE_HASH = "8de2df197dba94a7151d39371e464bf0bd10316ffa7d7006db81b08159210dde"


class OliveWorkerTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        temp_base = REPO / "target" / "larm-test-tmp"
        temp_base.mkdir(parents=True, exist_ok=True)
        cls.temp_root = Path(tempfile.mkdtemp(prefix="p065-", dir=temp_base))
        cls.python_root = cls.temp_root / "python"
        venv.EnvBuilder(with_pip=False, clear=True).create(cls.python_root)
        cls.python = cls.python_root / ("Scripts/python.exe" if os.name == "nt" else "bin/python")
        site_query = subprocess.run(
            [str(cls.python), "-c", "import sysconfig; print(sysconfig.get_paths()['purelib'])"],
            capture_output=True, text=True, encoding="utf-8", check=True,
            env={**os.environ, "PYTHONIOENCODING": "utf-8"},
        )
        cls.site_packages = Path(site_query.stdout.strip())
        cls.output_root = cls.temp_root / "managed-output"
        cls.output_root.mkdir()
        (cls.temp_root / "tmp").mkdir()
        cls.install_fake_packages("success")

    @classmethod
    def tearDownClass(cls) -> None:
        shutil.rmtree(cls.temp_root, ignore_errors=True)

    @classmethod
    def install_fake_packages(cls, mode: str) -> None:
        olive_source = f'''from pathlib import Path
import time

MODE = {mode!r}

def optimize(*, model_name_or_path, output_path, provider, precision, trust_remote_code):
    assert trust_remote_code is False
    assert provider == "CPUExecutionProvider"
    assert precision in {{"fp32", "fp16", "int8", "int4"}}
    target = Path(output_path) / "optimized.onnx"
    if MODE == "sleep":
        time.sleep(10)
    target.write_bytes(Path(model_name_or_path).read_bytes() + b"olive-output")
    if MODE == "fail":
        raise RuntimeError("fixture failure")
'''
        (cls.site_packages / "olive.py").write_text(olive_source, encoding="utf-8")
        (cls.site_packages / "onnxruntime.py").write_text(
            "def get_available_providers():\n    return ['CPUExecutionProvider']\n", encoding="utf-8"
        )
        for dirname, name, version in (
            ("olive_ai-1.0.0.dist-info", "olive-ai", "1.0.0"),
            ("onnxruntime-1.20.0.dist-info", "onnxruntime", "1.20.0"),
        ):
            info = cls.site_packages / dirname
            info.mkdir(exist_ok=True)
            (info / "METADATA").write_text(f"Metadata-Version: 2.1\nName: {name}\nVersion: {version}\n", encoding="utf-8")

    def setUp(self) -> None:
        self.output_root = self.temp_root / f"output-{self.id().split('.')[-1]}"
        self.output_root.mkdir(exist_ok=True)
        self.env = os.environ.copy()
        self.env.pop("PYTHONPATH", None)
        self.env.pop("LARM_PYTHON_ROOT", None)
        self.env.pop("LARM_OLIVE_OUTPUT_ROOT", None)
        self.env.update({
            "LARM_PYTHON_ROOT": str(self.python_root),
            "LARM_OLIVE_OUTPUT_ROOT": str(self.output_root),
            "TEMP": str(self.temp_root / "tmp"),
            "TMP": str(self.temp_root / "tmp"),
            "TMPDIR": str(self.temp_root / "tmp"),
        })

    def call_worker(self, request: dict, *, worker: Path = WORKER, env: dict | None = None) -> tuple[dict, int]:
        process = subprocess.run(
            [str(self.python), "-I", str(worker)],
            input=json.dumps(request, separators=(",", ":")),
            capture_output=True,
            text=True,
            encoding="utf-8",
            env=env or self.env,
            timeout=20,
            check=False,
        )
        response = json.loads(process.stdout)
        errors = list(VALIDATOR.iter_errors(response))
        self.assertFalse(errors, errors[0].message if errors else "respuesta inválida")
        return response, process.returncode

    @staticmethod
    def request(operation: str, **extra: object) -> dict:
        return {"protocol_version": "1.0", "operation": operation, **extra}

    def plan(self, provider: str = "CPUExecutionProvider") -> dict:
        response, code = self.call_worker(self.request(
            "plan", fixture_id="tensor_identity", provider_target=provider, precision="fp32"
        ))
        self.assertEqual(code, 0)
        self.assertTrue(response["ok"])
        return response["result"]

    def test_fake_probe_version_and_capabilities_report_isolated_versions(self) -> None:
        for operation in ("probe", "version", "capabilities"):
            response, code = self.call_worker(self.request(operation))
            self.assertEqual(code, 0)
            self.assertTrue(response["ok"])
            if operation == "probe":
                self.assertTrue(response["result"]["managed_python"])
                self.assertTrue(response["result"]["olive_ai_installed"])
                self.assertEqual(response["result"]["available_providers"], ["CPUExecutionProvider"])
            elif operation == "version":
                self.assertEqual(response["result"]["olive_ai"], "1.0.0")
                self.assertEqual(response["result"]["onnxruntime"], "1.20.0")
            else:
                self.assertTrue(response["result"]["execute_fixture_available"])

    def test_protocol_schema_accepts_closed_requests_and_responses(self) -> None:
        request = self.request("plan", fixture_id="tensor_identity", provider_target="CPUExecutionProvider", precision="fp32")
        self.assertFalse(list(VALIDATOR.iter_errors(request)))
        self.assertTrue(list(VALIDATOR.iter_errors({**request, "output_path": "C:/arbitrary"})))
        response, _ = self.call_worker(self.request("probe"))
        self.assertFalse(list(VALIDATOR.iter_errors(response)))

    def test_plan_is_read_only_repeatable_and_marks_output_size_unknown(self) -> None:
        first = self.plan()
        second = self.plan()
        self.assertEqual(first, second)
        self.assertTrue(first["eligible"])
        self.assertEqual(first["input_estimate"], {"bytes": "130", "confidence": "EXACT"})
        self.assertEqual(first["output_estimate"]["confidence"], "UNKNOWN")
        self.assertIsNone(first["output_estimate"]["bytes"])
        self.assertEqual(first["output_estimate"]["reason"], "NO_QUALIFIED_OLIVE_SIZE_ESTIMATOR")
        self.assertFalse(first["parameters"]["trust_remote_code"])
        self.assertEqual(list(self.output_root.iterdir()), [])

    def test_environment_absent_is_reported_without_global_package_use(self) -> None:
        unmanaged = self.env.copy()
        unmanaged.pop("LARM_PYTHON_ROOT", None)
        response, code = self.call_worker(self.request("probe"), env=unmanaged)
        self.assertEqual(code, 0)
        self.assertFalse(response["result"]["managed_python"])
        self.assertIsNone(response["result"]["olive_ai_installed"])
        plan_response, code = self.call_worker(self.request(
            "plan", fixture_id="tensor_identity", provider_target="CPUExecutionProvider", precision="fp32"
        ), env=unmanaged)
        self.assertEqual(code, 0)
        self.assertFalse(plan_response["result"]["eligible"])
        self.assertIn("MANAGED_PYTHON_REQUIRED", plan_response["result"]["blockers"])

    def test_invalid_schema_and_unlisted_fixture_fail_before_execution(self) -> None:
        response, code = self.call_worker(self.request("probe", unexpected=True))
        self.assertEqual(code, 2)
        self.assertEqual(response["error"]["code"], "INVALID_REQUEST")
        response, code = self.call_worker(self.request(
            "plan", fixture_id="outside-model", provider_target="CPUExecutionProvider", precision="fp32"
        ))
        self.assertEqual(code, 2)
        self.assertEqual(response["error"]["code"], "FIXTURE_NOT_AUTHORIZED")
        self.assertEqual(list(self.output_root.iterdir()), [])

    def test_unavailable_provider_keeps_plan_ineligible(self) -> None:
        plan = self.plan("CUDAExecutionProvider")
        self.assertFalse(plan["eligible"])
        self.assertFalse(plan["provider_target_available"])
        self.assertIn("PROVIDER_UNAVAILABLE", plan["blockers"])

    def test_fixture_hash_mismatch_is_rejected(self) -> None:
        copied = self.temp_root / "tampered-worker"
        shutil.copytree(WORKER_DIR, copied)
        fixture_copy = copied / "fixtures" / "tensor_identity.onnx"
        fixture_copy.write_bytes(fixture_copy.read_bytes() + b"tampered")
        response, code = self.call_worker(self.request(
            "plan", fixture_id="tensor_identity", provider_target="CPUExecutionProvider", precision="fp32"
        ), worker=copied / "worker.py")
        self.assertEqual(code, 2)
        self.assertEqual(response["error"]["code"], "FIXTURE_HASH_MISMATCH")

    def test_execute_creates_verified_candidate_and_provenance_without_touching_source(self) -> None:
        before = hashlib.sha256(FIXTURE.read_bytes()).hexdigest()
        plan = self.plan()
        response, code = self.call_worker(self.request(
            "execute_fixture", fixture_id="tensor_identity", provider_target="CPUExecutionProvider",
            precision="fp32", plan_hash=plan["plan_hash"],
        ))
        self.assertEqual(code, 0)
        self.assertTrue(response["ok"])
        result = response["result"]
        candidate = self.output_root / result["candidate_relative_path"]
        self.assertTrue(candidate.is_dir())
        provenance = json.loads((candidate / "provenance.json").read_text(encoding="utf-8"))
        self.assertEqual(provenance["input_sha256"], FIXTURE_HASH)
        self.assertEqual(provenance["provider_target"], "CPUExecutionProvider")
        self.assertFalse(provenance["parameters"]["trust_remote_code"])
        self.assertEqual(provenance["parameters"]["provider_target"], "CPUExecutionProvider")
        self.assertEqual(len(provenance["output_artifacts"]), 1)
        self.assertEqual(hashlib.sha256(FIXTURE.read_bytes()).hexdigest(), before)
        self.assertEqual(result["candidate_relative_path"], f"candidates/{plan['plan_hash']}")

    def test_stale_plan_mismatch_does_not_publish(self) -> None:
        response, code = self.call_worker(self.request(
            "execute_fixture", fixture_id="tensor_identity", provider_target="CPUExecutionProvider",
            precision="fp32", plan_hash="0" * 64,
        ))
        self.assertEqual(code, 2)
        self.assertEqual(response["error"]["code"], "PLAN_MISMATCH")
        self.assertEqual(list(self.output_root.iterdir()), [])

    def test_tool_failure_can_retry_without_publishing_partial_output(self) -> None:
        plan = self.plan()
        type(self).install_fake_packages("fail")
        request = self.request(
            "execute_fixture", fixture_id="tensor_identity", provider_target="CPUExecutionProvider",
            precision="fp32", plan_hash=plan["plan_hash"],
        )
        failed, code = self.call_worker(request)
        self.assertEqual(code, 2)
        self.assertEqual(failed["error"]["code"], "TOOL_FAILED")
        self.assertEqual(list((self.output_root / "candidates").iterdir()), [])
        type(self).install_fake_packages("success")
        succeeded, code = self.call_worker(request)
        self.assertEqual(code, 0)
        self.assertTrue(succeeded["ok"])

    def test_timeout_kills_tool_and_leaves_no_candidate(self) -> None:
        type(self).install_fake_packages("sleep")
        plan = self.plan()
        timed = self.env.copy()
        timed["LARM_OLIVE_TIMEOUT_SECONDS"] = "0.1"
        response, code = self.call_worker(self.request(
            "execute_fixture", fixture_id="tensor_identity", provider_target="CPUExecutionProvider",
            precision="fp32", plan_hash=plan["plan_hash"],
        ), env=timed)
        self.assertEqual(code, 2)
        self.assertEqual(response["error"]["code"], "TOOL_TIMEOUT")
        self.assertEqual(list((self.output_root / "candidates").iterdir()), [])
        type(self).install_fake_packages("success")

    def test_output_io_failure_fails_closed(self) -> None:
        file_root = self.temp_root / "output-is-file"
        file_root.write_text("occupied", encoding="utf-8")
        invalid = self.env.copy()
        invalid["LARM_OLIVE_OUTPUT_ROOT"] = str(file_root)
        plan = self.plan()
        response, code = self.call_worker(self.request(
            "execute_fixture", fixture_id="tensor_identity", provider_target="CPUExecutionProvider",
            precision="fp32", plan_hash=plan["plan_hash"],
        ), env=invalid)
        self.assertEqual(code, 2)
        self.assertEqual(response["error"]["code"], "OUTPUT_ROOT_UNAVAILABLE")
        self.assertEqual(file_root.read_text(encoding="utf-8"), "occupied")


if __name__ == "__main__":
    unittest.main()
