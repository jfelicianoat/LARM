#!/usr/bin/env python3
"""Behavioral checks for the configured-root llama.cpp inventory probe."""

import hashlib
import importlib.util
import json
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest.mock import Mock


REPO = Path(__file__).resolve().parent.parent
PROBE_PATH = REPO / "adapters/runtime/llama-cpp/probe.py"
SPEC = importlib.util.spec_from_file_location("llama_cpp_probe", PROBE_PATH)
assert SPEC and SPEC.loader
probe = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(probe)


def completed(args, code=0, stdout=b"", stderr=b""):
    return subprocess.CompletedProcess(args, code, stdout, stderr)


class LlamaCppProbe(unittest.TestCase):
    def setUp(self):
        (REPO / "target" / "tmp").mkdir(parents=True, exist_ok=True)
        self.temp = tempfile.TemporaryDirectory(dir=REPO / "target" / "tmp")
        self.root = Path(self.temp.name) / "configured"
        self.root.mkdir()

    def tearDown(self):
        self.temp.cleanup()

    def add_build(self, name, version="7000 (abcdef0123456789)", device="CUDA0: Fixture GPU"):
        build = self.root / name
        bin_dir = build / "build" / "bin" / "Release"
        bin_dir.mkdir(parents=True)
        for executable in probe.EXECUTABLES.values():
            (bin_dir / executable).write_bytes(f"fixture:{name}:{executable}".encode())
        cli = bin_dir / probe.EXECUTABLES["llama-cli"]
        (bin_dir / probe.EXECUTABLES["llama-server"]).write_bytes(f"fixture:{name}:server".encode())
        (build / "convert").mkdir()
        converter = build / "convert" / "convert_hf_to_gguf.py"
        converter.write_text("# fake converter fixture\n", encoding="utf-8")
        return build, cli, version, device

    @staticmethod
    def runner_for(build_versions):
        def run(args, **kwargs):
            executable = Path(args[0])
            build_name = next(name for name in build_versions if name in str(executable))
            version, device = build_versions[build_name]
            if args[-1] == "--version":
                return completed(args, stdout=f"version: {version}\n".encode())
            if args[-1] == "--help":
                return completed(args, stdout=b"Usage: llama-cli [options]\n --list-devices\n")
            if args[-1] == "--list-devices":
                return completed(args, stdout=f"Available devices:\n  - {device}\n".encode())
            return completed(args, 2, stderr=b"unexpected arguments")
        return run

    def test_discovers_two_side_by_side_builds_and_records_exact_probes(self):
        build_a, _, _, _ = self.add_build("build-a", "7000 (abcdef0123456789)", "CUDA0: Fixture GPU")
        build_b, _, _, _ = self.add_build("build-b", "7101 (fedcba9876543210)", "Vulkan0: Fixture GPU")
        runner = self.runner_for({
            "build-a": ("7000 (abcdef0123456789)", "CUDA0: Fixture GPU"),
            "build-b": ("7101 (fedcba9876543210)", "Vulkan0: Fixture GPU"),
        })

        result = probe.discover([self.root], run=runner, environment={
            "SystemRoot": "C:\\Windows",
            "PATH": "must-not-inherit",
            "AI_BROKER_TOKEN": "must-not-inherit",
            "AGORA_TOKEN": "must-not-inherit",
        })

        self.assertEqual(result["schema_version"], probe.SCHEMA_VERSION)
        self.assertEqual(len(result["installations"]), 2)
        by_path = {Path(item["build_path"]).name: item for item in result["installations"]}
        self.assertEqual(by_path["build-a"]["version"]["version_string"], "7000")
        self.assertEqual(by_path["build-a"]["version"]["commit"], "abcdef0123456789")
        self.assertEqual(by_path["build-a"]["backend_probe"]["backends"], ["CUDA"])
        self.assertEqual(by_path["build-b"]["backend_probe"]["backends"], ["Vulkan"])
        self.assertEqual(by_path["build-a"]["qualification"], "UNQUALIFIED")
        self.assertFalse(by_path["build-a"]["profile_lock_created"])
        cli_path = by_path["build-a"]["tools"]["llama-cli"]["path"]
        expected_cli_hash = hashlib.sha256(Path(cli_path).read_bytes()).hexdigest()
        self.assertEqual(by_path["build-a"]["tools"]["llama-cli"]["sha256"], expected_cli_hash)
        self.assertIn("convert_hf_to_gguf.py", by_path["build-a"]["converters"])
        self.assertEqual(build_a.name, "build-a")
        self.assertEqual(build_b.name, "build-b")

    def test_does_not_recurse_beyond_configured_root_and_one_side_by_side_level(self):
        hidden = self.root / "one" / "nested" / "build"
        bin_dir = hidden / "bin"
        bin_dir.mkdir(parents=True)
        (bin_dir / "llama-server.exe").write_bytes(b"hidden fixture")
        result = probe.discover([self.root], run=Mock())
        self.assertEqual(result["installations"], [])

    def test_unknown_or_alias_version_is_not_promoted_to_a_release(self):
        stdout = b"warning: startup output\nversion: latest\n"
        record = probe._version_from_output(stdout, b"")
        self.assertEqual(record["status"], "UNKNOWN")
        self.assertIsNone(record["version_string"])
        self.assertEqual(record["channel"], "custom")
        self.assertEqual(record["stdout_sha256"], hashlib.sha256(stdout).hexdigest())

    def test_version_parser_keeps_unknown_build_custom_and_rejects_ambiguity(self):
        unknown = probe._version_from_output(b"version: vendor-preview-42\n", b"")
        ambiguous = probe._version_from_output(b"version: 1\nversion: 2\n", b"")
        self.assertEqual(unknown["status"], "OBSERVED")
        self.assertEqual(unknown["channel"], "custom")
        self.assertEqual(ambiguous["status"], "UNKNOWN")
        self.assertIsNone(ambiguous["version_string"])

    def test_backend_probe_runs_only_when_the_exact_help_option_is_advertised(self):
        build, _, _, _ = self.add_build("build-a")
        calls = []

        def run(args, **kwargs):
            calls.append((args, kwargs))
            if args[-1] == "--version":
                return completed(args, stdout=b"version: 7000 (abcdef0123456789)\n")
            if args[-1] == "--help":
                return completed(args, stdout=b"Usage: llama-cli\n")
            return completed(args, 2)

        result = probe.inspect_build(self.root.resolve(), build.resolve(), run=run)
        self.assertEqual(result["backend_probe"]["status"], "UNSUPPORTED")
        self.assertEqual([args[-1] for args, _ in calls], ["--version", "--help"])

    def test_process_is_bounded_noninteractive_and_receives_no_user_credentials(self):
        build, _, _, _ = self.add_build("build-a")
        calls = []

        def run(args, **kwargs):
            calls.append((args, kwargs))
            if args[-1] == "--version":
                return completed(args, stdout=b"version: 7000 (abcdef0123456789)\n")
            if args[-1] == "--help":
                return completed(args, stdout=b"--list-devices\n")
            return completed(args, stdout=b"CPU: Fixture CPU\n")

        result = probe.inspect_build(self.root.resolve(), build.resolve(), run=run, environment={
            "PATH": "must-not-inherit",
            "SystemRoot": "C:\\Windows",
            "OLLAMA_API_KEY": "must-not-inherit",
            "AI_BROKER_TOKEN": "must-not-inherit",
            "AGORA_TOKEN": "must-not-inherit",
        })
        self.assertEqual(result["backend_probe"]["backends"], ["CPU"])
        for args, kwargs in calls:
            self.assertFalse(kwargs["shell"])
            self.assertIs(kwargs["stdin"], subprocess.DEVNULL)
            self.assertEqual(kwargs["timeout"], probe.PROBE_TIMEOUT_SECONDS)
            self.assertNotIn("must-not-inherit", str(args))
            self.assertNotIn("must-not-inherit", kwargs["env"]["PATH"])
            self.assertNotIn("AI_BROKER_TOKEN", kwargs["env"])
            self.assertNotIn("AGORA_TOKEN", kwargs["env"])
            self.assertIn("PATH", kwargs["env"])
            self.assertIn(args[-1], {"--version", "--help", "--list-devices"})

    def test_unavailable_root_is_reported_without_scanning_the_machine(self):
        result = probe.discover([self.root / "missing"], run=Mock())
        self.assertEqual(result["installations"], [])
        self.assertEqual(result["unrecognized"], ["configured_root_unavailable"])


if __name__ == "__main__":
    unittest.main(verbosity=2)
