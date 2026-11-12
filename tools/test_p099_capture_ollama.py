#!/usr/bin/env python3
"""Behavioral checks for the allowlisted Ollama observation capture."""

import hashlib
import json
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest.mock import Mock, patch

import p099_capture_ollama as capture
from p099_capture_ollama import CaptureError, capture_ollama


class OllamaObservationCapture(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(dir=capture.REPO / "target" / "tmp")
        self.root = Path(self.temp.name)
        self.catalog = self.root / "docs" / "contracts" / "runtime-profiles.json"
        self.catalog.parent.mkdir(parents=True)
        shutil.copy2(capture.CATALOG, self.catalog)
        self.snapshot = self.root / capture.SOURCE_SNAPSHOT
        self.snapshot.parent.mkdir(parents=True)
        shutil.copy2(capture.REPO / capture.SOURCE_SNAPSHOT, self.snapshot)
        self.observations = self.root / "observations"
        self.executable = self.root / "ollama.exe"
        self.executable.write_bytes(b"pinned test executable")
        self.executable_hash = hashlib.sha256(self.executable.read_bytes()).hexdigest()
        self.run = Mock(
            return_value=subprocess.CompletedProcess(
                args=["ollama.exe", "--version"],
                returncode=0,
                stdout=b"Warning: cannot reach local instance\nclient version is 0.40.6\n",
                stderr=b"",
            )
        )

    def tearDown(self):
        self.temp.cleanup()

    def capture(self, run_id="run-1"):
        with patch.object(capture, "EXPECTED_EXECUTABLE_SHA256", self.executable_hash), patch.object(
            capture, "_windows_host", return_value={"os": "Windows 11", "architecture": "AMD64"}
        ):
            return capture_ollama(
                run_id,
                repo_root=self.root,
                observation_root=self.observations,
                which=lambda name: str(self.executable) if name == "ollama" else None,
                run=self.run,
                environment={"PATH": "C:\\Windows", "OLLAMA_API_KEY": "must-not-pass"},
            )

    def test_capture_hashes_raw_probe_and_pinned_source_without_qualifying(self):
        result = self.capture()
        output_dir = self.observations / "run-1"
        record = json.loads((output_dir / "capture.json").read_text(encoding="utf-8"))
        recorded_capture_hash = record.pop("capture_sha256")
        canonical_record = json.dumps(
            record, ensure_ascii=False, sort_keys=True, separators=(",", ":")
        ).encode("utf-8")
        self.assertEqual(result["status"], "PASS")
        self.assertEqual(hashlib.sha256(canonical_record).hexdigest(), recorded_capture_hash)
        self.assertEqual(record["upstream_version"], "0.40.6")
        self.assertEqual(record["qualification"], "UNQUALIFIED")
        self.assertFalse(record["profile_lock_created"])
        self.assertEqual(
            record["probe"]["stdout_sha256"],
            hashlib.sha256((output_dir / "probe.stdout.bin").read_bytes()).hexdigest(),
        )
        self.assertEqual(record["source"]["content_sha256"], capture.SOURCE_SHA256)
        self.assertNotIn("OLLAMA_API_KEY", self.run.call_args.kwargs["env"])
        self.assertFalse(self.run.call_args.kwargs["shell"])
        self.assertEqual(self.run.call_args.kwargs["stdin"], subprocess.DEVNULL)

    def test_unpinned_executable_is_rejected_before_it_runs(self):
        self.executable.write_bytes(b"changed executable")
        with patch.object(capture, "_windows_host", return_value={}):
            with self.assertRaisesRegex(CaptureError, "executable_hash_not_allowlisted"):
                capture_ollama(
                    "run-2",
                    repo_root=self.root,
                    observation_root=self.observations,
                    which=lambda _name: str(self.executable),
                    run=self.run,
                )
        self.run.assert_not_called()

    def test_catalog_probe_drift_is_rejected_before_execution(self):
        catalog = json.loads(self.catalog.read_text(encoding="utf-8"))
        ollama = next(item for item in catalog["profiles"] if item["runtime"] == "Ollama")
        ollama["version_probe"]["args"] = ["run", "model"]
        self.catalog.write_text(json.dumps(catalog), encoding="utf-8")
        with patch.object(capture, "_windows_host", return_value={}):
            with self.assertRaisesRegex(CaptureError, "probe_not_allowlisted"):
                capture_ollama(
                    "run-3",
                    repo_root=self.root,
                    observation_root=self.observations,
                    which=lambda _name: str(self.executable),
                    run=self.run,
                )
        self.run.assert_not_called()

    def test_source_snapshot_hash_mismatch_is_rejected_before_execution(self):
        self.snapshot.write_bytes(b"changed source")
        with patch.object(capture, "EXPECTED_EXECUTABLE_SHA256", self.executable_hash), patch.object(
            capture, "_windows_host", return_value={}
        ):
            with self.assertRaisesRegex(CaptureError, "source_snapshot_hash_mismatch"):
                capture_ollama(
                    "run-4",
                    repo_root=self.root,
                    observation_root=self.observations,
                    which=lambda _name: str(self.executable),
                    run=self.run,
                )
        self.run.assert_not_called()

    def test_version_mismatch_is_saved_as_unqualified_and_returns_nonpass(self):
        self.run.return_value = subprocess.CompletedProcess(
            args=["ollama.exe", "--version"],
            returncode=0,
            stdout=b"client version is 0.40.7\n",
            stderr=b"",
        )
        result = self.capture("run-5")
        record = json.loads(
            (self.observations / "run-5" / "capture.json").read_text(encoding="utf-8")
        )
        self.assertEqual(result["status"], "VERSION_OR_PROBE_MISMATCH")
        self.assertEqual(record["qualification"], "UNQUALIFIED")
        self.assertEqual(record["upstream_version"], "0.40.7")

    def test_existing_run_id_cannot_be_overwritten(self):
        (self.observations / "run-6").mkdir(parents=True)
        with self.assertRaisesRegex(CaptureError, "capture_already_exists"):
            self.capture("run-6")
        self.run.assert_not_called()


if __name__ == "__main__":
    unittest.main(verbosity=2)
