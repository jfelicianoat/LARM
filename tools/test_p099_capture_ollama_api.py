#!/usr/bin/env python3
"""Behavioral checks for the verified-listener Ollama API capture."""

import hashlib
import json
import shutil
import tempfile
import unittest
from pathlib import Path
from unittest.mock import Mock, patch

import p099_capture_ollama_api as capture_api
from p099_capture_ollama import CaptureError, EXPECTED_VERSION, SOURCE_SNAPSHOT


class OllamaApiCapture(unittest.TestCase):
    def setUp(self):
        (capture_api.REPO / "target" / "tmp").mkdir(parents=True, exist_ok=True)
        self.temp = tempfile.TemporaryDirectory(dir=capture_api.REPO / "target" / "tmp")
        self.root = Path(self.temp.name)
        catalog = self.root / "docs" / "contracts" / "runtime-profiles.json"
        catalog.parent.mkdir(parents=True)
        shutil.copy2(capture_api.REPO / "docs/contracts/runtime-profiles.json", catalog)
        snapshot = self.root / SOURCE_SNAPSHOT
        snapshot.parent.mkdir(parents=True)
        shutil.copy2(capture_api.REPO / SOURCE_SNAPSHOT, snapshot)
        self.observations = self.root / "api_observations"
        self.executable = self.root / "ollama.exe"
        self.executable.write_bytes(b"pinned test executable")
        self.executable_hash = hashlib.sha256(self.executable.read_bytes()).hexdigest()
        self.listener = {
            "pid": 4567,
            "executable": str(self.executable),
            "executable_sha256": self.executable_hash,
        }
        self.http_get = Mock(
            return_value=(200, json.dumps({"version": EXPECTED_VERSION}).encode("utf-8"))
        )
        self.listener_lookup = Mock(side_effect=[self.listener, self.listener])

    def tearDown(self):
        self.temp.cleanup()

    def capture(self, run_id="api-run-1"):
        with patch.object(
            capture_api, "EXPECTED_EXECUTABLE_SHA256", self.executable_hash
        ), patch.object(capture_api, "_windows_host", return_value={"os": "Windows 11"}):
            return capture_api.capture_ollama_api(
                run_id,
                repo_root=self.root,
                observation_root=self.observations,
                which=lambda name: str(self.executable) if name == "ollama" else None,
                http_get=self.http_get,
                listener_lookup=self.listener_lookup,
                environment={
                    "PATH": "C:\\Windows",
                    "OLLAMA_API_KEY": "must-not-pass",
                    "AI_BROKER_TOKEN": "must-not-pass",
                    "AGORA_TOKEN": "must-not-pass",
                },
            )

    def test_exact_response_is_hashed_and_listener_identity_is_recorded(self):
        result = self.capture()
        output_dir = self.observations / "api-run-1"
        record = json.loads((output_dir / "capture.json").read_text(encoding="utf-8"))
        recorded_hash = record.pop("capture_sha256")
        canonical = json.dumps(
            record, ensure_ascii=False, sort_keys=True, separators=(",", ":")
        ).encode("utf-8")
        self.assertEqual(result["status"], "PASS")
        self.assertEqual(hashlib.sha256(canonical).hexdigest(), recorded_hash)
        self.assertEqual(record["api_probe"]["host"], "127.0.0.1")
        self.assertEqual(record["api_probe"]["path"], "/api/version")
        self.assertEqual(
            record["api_probe"]["response_sha256"],
            hashlib.sha256((output_dir / "api.response.bin").read_bytes()).hexdigest(),
        )
        self.assertEqual(record["qualification"], "UNQUALIFIED")
        self.assertFalse(record["profile_lock_created"])
        self.assertEqual(record["service"]["ownership"], "PREEXISTING_VERIFIED")
        self.assertFalse(record["service"]["started_by_capture"])
        self.assertFalse(record["service"]["stopped_by_capture"])
        self.listener_lookup.assert_has_calls(
            [
                unittest.mock.call("127.0.0.1", 11434, {"PATH": "C:\\Windows"}),
                unittest.mock.call("127.0.0.1", 11434, {"PATH": "C:\\Windows"}),
            ]
        )
        self.http_get.assert_called_once_with(
            "127.0.0.1", 11434, "/api/version", capture_api.API_TIMEOUT_SECONDS
        )

    def test_wrong_version_or_response_shape_remains_unqualified(self):
        self.http_get.return_value = (200, b'{"version":"0.40.7"}')
        result = self.capture("api-run-2")
        record = json.loads(
            (self.observations / "api-run-2" / "capture.json").read_text(encoding="utf-8")
        )
        self.assertEqual(result["status"], "API_VERSION_MISMATCH")
        self.assertEqual(record["upstream_version"], "0.40.7")
        self.assertEqual(record["qualification"], "UNQUALIFIED")

        self.listener_lookup = Mock(side_effect=[self.listener, self.listener])
        self.http_get.return_value = (200, b"not json")
        result = self.capture("api-run-3")
        self.assertEqual(result["status"], "API_RESPONSE_INVALID")

    def test_missing_or_unpinned_listener_is_never_queried(self):
        self.listener_lookup = Mock(return_value=None)
        result = self.capture("api-run-4")
        self.assertEqual(result["status"], "API_UNAVAILABLE")
        self.http_get.assert_not_called()

        self.listener_lookup = Mock(
            return_value={**self.listener, "executable_sha256": "0" * 64}
        )
        result = self.capture("api-run-5")
        self.assertEqual(result["status"], "LISTENER_UNVERIFIED")
        self.http_get.assert_not_called()

    def test_listener_change_during_request_fails_closed(self):
        changed = {**self.listener, "pid": 9999}
        self.listener_lookup = Mock(side_effect=[self.listener, changed])
        result = self.capture("api-run-6")
        self.assertEqual(result["status"], "LISTENER_CHANGED")
        self.assertEqual(result["failure_reason"], "loopback_listener_changed_during_probe")

    def test_existing_run_id_cannot_be_overwritten(self):
        (self.observations / "api-run-7").mkdir(parents=True)
        with self.assertRaisesRegex(CaptureError, "already_exists"):
            self.capture("api-run-7")
        self.http_get.assert_not_called()


if __name__ == "__main__":
    unittest.main(verbosity=2)
