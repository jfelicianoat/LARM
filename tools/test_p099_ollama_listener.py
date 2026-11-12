#!/usr/bin/env python3
"""Tests for Windows listener attribution used by P099 captures."""

import hashlib
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest.mock import Mock, patch

import p099_ollama_listener as listener
from p099_ollama_listener import ListenerInspectionError, listener_identity


class ListenerIdentity(unittest.TestCase):
    def setUp(self):
        (Path.cwd() / "target" / "tmp").mkdir(parents=True, exist_ok=True)
        self.temp = tempfile.TemporaryDirectory(dir=Path.cwd() / "target" / "tmp")
        self.executable = Path(self.temp.name) / "ollama.exe"
        self.executable.write_bytes(b"pinned fake ollama")
        self.powershell = Mock(
            return_value=subprocess.CompletedProcess(
                args=["powershell.exe"], returncode=0, stdout="6864\r\n", stderr=""
            )
        )

    def tearDown(self):
        self.temp.cleanup()

    def inspect(self, stdout="6864\r\n"):
        self.powershell.return_value.stdout = stdout
        with patch.object(listener.platform, "system", return_value="Windows"), patch.object(
            listener.shutil, "which", return_value="C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe"
        ):
            return listener_identity(
                "127.0.0.1",
                11434,
                environment={"PATH": "C:\\Windows", "AGORA_TOKEN": "must-not-pass"},
                run=self.powershell,
                image_path=lambda _pid: self.executable,
            )

    def test_listener_identity_includes_pid_and_executable_hash(self):
        identity = self.inspect()
        self.assertEqual(identity["pid"], 6864)
        self.assertEqual(
            identity["executable_sha256"], hashlib.sha256(self.executable.read_bytes()).hexdigest()
        )
        kwargs = self.powershell.call_args.kwargs
        self.assertFalse(kwargs["shell"])
        self.assertNotIn("AGORA_TOKEN", kwargs["env"])

    def test_no_listener_returns_none(self):
        self.assertIsNone(self.inspect(""))

    def test_multiple_or_unreadable_listener_owners_fail_closed(self):
        with self.assertRaisesRegex(ListenerInspectionError, "multiple_owners"):
            self.inspect("100\r\n200\r\n")
        with patch.object(listener.platform, "system", return_value="Windows"), patch.object(
            listener.shutil, "which", return_value="powershell.exe"
        ):
            self.powershell.return_value = subprocess.CompletedProcess(
                args=["powershell.exe"], returncode=1, stdout="", stderr="denied"
            )
            with self.assertRaisesRegex(ListenerInspectionError, "inspection_failed"):
                listener_identity("127.0.0.1", 11434, run=self.powershell)

    def test_target_must_be_loopback(self):
        with self.assertRaisesRegex(ListenerInspectionError, "target_invalid"):
            listener_identity("example.com", 11434)


if __name__ == "__main__":
    unittest.main(verbosity=2)
