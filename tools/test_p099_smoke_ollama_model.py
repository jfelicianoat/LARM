#!/usr/bin/env python3
"""Behavioral checks for the fixed Ollama model smoke."""

import hashlib
import json
import shutil
import tempfile
import unittest
from pathlib import Path
from unittest.mock import Mock, patch

import p099_smoke_ollama_model as smoke
from p099_capture_ollama import CaptureError, EXPECTED_VERSION, SOURCE_SNAPSHOT


class OllamaModelSmoke(unittest.TestCase):
    def setUp(self):
        (smoke.REPO / "target" / "tmp").mkdir(parents=True, exist_ok=True)
        self.temp = tempfile.TemporaryDirectory(dir=smoke.REPO / "target" / "tmp")
        self.root = Path(self.temp.name)
        catalog = self.root / "docs" / "contracts" / "runtime-profiles.json"
        catalog.parent.mkdir(parents=True)
        shutil.copy2(smoke.REPO / "docs/contracts/runtime-profiles.json", catalog)
        source = self.root / SOURCE_SNAPSHOT
        source.parent.mkdir(parents=True)
        shutil.copy2(smoke.REPO / SOURCE_SNAPSHOT, source)
        self.output = self.root / "model_smokes"
        self.executable = self.root / "ollama.exe"
        self.executable.write_bytes(b"pinned test executable")
        self.executable_hash = hashlib.sha256(self.executable.read_bytes()).hexdigest()
        self.owner = {
            "pid": 9876,
            "executable": str(self.executable),
            "executable_sha256": self.executable_hash,
        }
        self.lookup = Mock(return_value=self.owner)
        self.responses = [
            (200, json.dumps({"version": EXPECTED_VERSION}).encode()),
            (200, json.dumps({"models": [{"name": smoke.EXPECTED_MODEL}]}).encode()),
            (
                200,
                json.dumps(
                    {
                        "model": smoke.EXPECTED_MODEL,
                        "response": "LARM_OK",
                        "done": True,
                        "done_reason": "stop",
                        "eval_count": 1,
                    }
                ).encode(),
            ),
        ]
        self.http_request = Mock(side_effect=self.responses)

    def tearDown(self):
        self.temp.cleanup()

    def capture(self, run_id="model-run-1"):
        with patch.object(
            smoke, "EXPECTED_EXECUTABLE_SHA256", self.executable_hash
        ), patch.object(smoke, "_windows_host", return_value={"os": "Windows 11"}):
            return smoke.capture_ollama_model_smoke(
                run_id,
                repo_root=self.root,
                observation_root=self.output,
                which=lambda name: str(self.executable) if name == "ollama" else None,
                listener_lookup=self.lookup,
                http_request=self.http_request,
                environment={
                    "PATH": "C:\\Windows",
                    "OLLAMA_API_KEY": "must-not-pass",
                    "AI_BROKER_TOKEN": "must-not-pass",
                    "AGORA_TOKEN": "must-not-pass",
                },
            )

    def test_generation_uses_fixed_model_and_records_preexisting_listener(self):
        result = self.capture()
        output_dir = self.output / "model-run-1"
        record = json.loads((output_dir / "capture.json").read_text(encoding="utf-8"))
        recorded_hash = record.pop("capture_sha256")
        canonical = json.dumps(
            record, ensure_ascii=False, sort_keys=True, separators=(",", ":")
        ).encode("utf-8")
        self.assertEqual(result["status"], "PASS")
        self.assertEqual(hashlib.sha256(canonical).hexdigest(), recorded_hash)
        self.assertTrue(result["response_contains_expected_marker"])
        self.assertEqual(record["qualification"], "UNQUALIFIED")
        self.assertFalse(record["profile_lock_created"])
        self.assertEqual(record["service"]["ownership"], "PREEXISTING_VERIFIED")
        self.assertFalse(record["service"]["started_by_capture"])
        self.assertFalse(record["service"]["stopped_by_capture"])
        self.assertEqual(record["service"]["pid"], 9876)
        self.assertEqual(
            [call.args[0:4] for call in self.http_request.call_args_list],
            [
                ("GET", "127.0.0.1", 11434, "/api/version"),
                ("GET", "127.0.0.1", 11434, "/api/tags"),
                ("POST", "127.0.0.1", 11434, "/api/generate"),
            ],
        )
        generation = self.http_request.call_args_list[-1].args[4]
        self.assertEqual(generation["model"], smoke.EXPECTED_MODEL)
        self.assertEqual(generation["keep_alive"], 0)
        self.assertFalse(generation["think"])
        self.assertEqual(generation["options"]["num_predict"], smoke.NUM_PREDICT)
        self.assertEqual(generation["options"]["num_ctx"], 512)
        self.assertTrue(all(call.args[0] == "127.0.0.1" for call in self.lookup.call_args_list))
        safe_environment = self.lookup.call_args.args[2]
        for name in ("OLLAMA_API_KEY", "AI_BROKER_TOKEN", "AGORA_TOKEN"):
            self.assertNotIn(name, safe_environment)

    def test_model_missing_does_not_send_generation_request(self):
        self.http_request.side_effect = [
            (200, json.dumps({"version": EXPECTED_VERSION}).encode()),
            (200, json.dumps({"models": []}).encode()),
        ]
        result = self.capture("model-run-2")
        record = json.loads(
            (self.output / "model-run-2" / "capture.json").read_text(encoding="utf-8")
        )
        self.assertEqual(result["status"], "MODEL_NOT_INSTALLED")
        self.assertEqual(record["failure_reason"], "allowlisted_model_not_installed")
        self.assertEqual(self.http_request.call_count, 2)

    def test_listener_change_fails_closed_before_generation(self):
        self.lookup.side_effect = [self.owner, self.owner, None]
        result = self.capture("model-run-3")
        record = json.loads(
            (self.output / "model-run-3" / "capture.json").read_text(encoding="utf-8")
        )
        self.assertEqual(result["status"], "LISTENER_CHANGED")
        self.assertEqual(record["failure_reason"], "loopback_listener_changed_after_tags")
        self.assertEqual(self.http_request.call_count, 2)
        self.assertEqual(record["qualification"], "UNQUALIFIED")

    def test_unpinned_listener_is_never_queried(self):
        self.lookup.return_value = {
            **self.owner,
            "executable_sha256": "0" * 64,
        }
        result = self.capture("model-run-4")
        self.assertEqual(result["status"], "LISTENER_UNVERIFIED")
        self.http_request.assert_not_called()

    def test_generation_failure_does_not_qualify_profile(self):
        self.http_request.side_effect = [
            (200, json.dumps({"version": EXPECTED_VERSION}).encode()),
            (200, json.dumps({"models": [{"name": smoke.EXPECTED_MODEL}]}).encode()),
            (500, b'{"error":"model unavailable"}'),
        ]
        result = self.capture("model-run-5")
        record = json.loads(
            (self.output / "model-run-5" / "capture.json").read_text(encoding="utf-8")
        )
        self.assertEqual(result["status"], "MODEL_HTTP_ERROR")
        self.assertEqual(record["qualification"], "UNQUALIFIED")
        self.assertFalse(record["profile_lock_created"])

    def test_source_executable_and_output_replay_are_guarded(self):
        self.executable.write_bytes(b"changed executable")
        with patch.object(smoke, "_windows_host", return_value={}):
            with self.assertRaisesRegex(CaptureError, "executable_hash_not_allowlisted"):
                smoke.capture_ollama_model_smoke(
                    "model-run-6",
                    repo_root=self.root,
                    observation_root=self.output,
                    which=lambda _name: str(self.executable),
                    http_request=self.http_request,
                    listener_lookup=self.lookup,
                )
        self.http_request.assert_not_called()

        self.executable.write_bytes(b"pinned test executable")
        self.executable_hash = hashlib.sha256(self.executable.read_bytes()).hexdigest()
        self.owner["executable_sha256"] = self.executable_hash
        self.lookup.return_value = self.owner
        self.capture("model-run-7")
        with self.assertRaisesRegex(CaptureError, "already_exists"):
            self.capture("model-run-7")


if __name__ == "__main__":
    unittest.main(verbosity=2)
