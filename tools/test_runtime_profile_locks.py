#!/usr/bin/env python3
"""Behavioral tests for deterministic P099 profile-lock acquisition."""

import copy
import hashlib
import io
import json
import sys
import tempfile
import unittest
from contextlib import redirect_stdout
from pathlib import Path
from unittest.mock import patch

import runtime_profile_locks as cli
from runtime_profile_locks import (
    ProfileLockError,
    REPO,
    acquire_lock,
    canonical,
    digest,
    load_catalog,
    verify_lock,
    write_immutable,
)


class RuntimeProfileLocks(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(dir=REPO / "target" / "tmp")
        self.root = Path(self.temp.name)
        self.snapshot = b"version-pinned runtime profile source capture\n"
        (self.root / "source.txt").write_bytes(self.snapshot)
        profile = load_catalog()["Ollama"]
        fields = {name: None for name in profile}
        fields.update(
            upstream_version="0.13.0",
            installation_strategies=["PREBUILT_BINARY"],
            fixture_hashes={
                "positive": "a" * 64,
                "negative": "b" * 64,
                "error": "c" * 64,
            },
            evidence_refs=["source-capture:source.txt", "probe:version-and-backend"],
        )
        self.request = {
            "runtime": "Ollama",
            "source": {
                "url": "https://example.test/docs/0.13.0",
                "revision": "0.13.0",
                "snapshot_path": "source.txt",
                "content_sha256": hashlib.sha256(self.snapshot).hexdigest(),
            },
            "upstream_version": "0.13.0",
            "os": "Windows",
            "os_build": "10.0.26100",
            "backend": "CPU",
            "protocol": "HTTP+JSON",
            "fields": fields,
        }

    def tearDown(self):
        self.temp.cleanup()

    def acquire(self, request=None):
        return acquire_lock(request or self.request, evidence_root=self.root)

    def test_catalog_has_twenty_runtimes_with_the_same_required_lock_fields(self):
        catalog = load_catalog()
        self.assertEqual(len(catalog), 20)
        self.assertEqual(len(set(catalog.values())), 1)
        self.assertEqual(len(next(iter(catalog.values()))), 22)

    def test_lock_hash_and_serialization_are_deterministic(self):
        first = self.acquire()
        reordered = {
            key: dict(reversed(list(value.items()))) if isinstance(value, dict) else value
            for key, value in reversed(list(self.request.items()))
        }
        second = self.acquire(reordered)
        self.assertEqual(first["qualification"], "UNQUALIFIED")
        self.assertEqual(first["lock_sha256"], second["lock_sha256"])
        self.assertEqual(canonical(first), canonical(second))
        self.assertEqual(verify_lock(first), first)

    def test_unknown_version_stays_unqualified_and_missing_or_extra_fields_reject(self):
        unknown = copy.deepcopy(self.request)
        unknown["upstream_version"] = "UNKNOWN"
        unknown["fields"]["upstream_version"] = None
        self.assertEqual(self.acquire(unknown)["qualification"], "UNQUALIFIED")

        missing = copy.deepcopy(self.request)
        missing["fields"].pop("protocol_mapping")
        with self.assertRaisesRegex(ProfileLockError, "required_fields_mismatch"):
            self.acquire(missing)
        extra = copy.deepcopy(self.request)
        extra["fields"]["enabled"] = True
        with self.assertRaisesRegex(ProfileLockError, "required_fields_mismatch"):
            self.acquire(extra)

    def test_source_revision_and_captured_bytes_must_match(self):
        unpinned = copy.deepcopy(self.request)
        unpinned["source"]["revision"] = "latest"
        with self.assertRaisesRegex(ProfileLockError, "source_revision_unpinned"):
            self.acquire(unpinned)
        mismatched = copy.deepcopy(self.request)
        mismatched["source"]["content_sha256"] = "d" * 64
        with self.assertRaisesRegex(ProfileLockError, "snapshot_hash_mismatch"):
            self.acquire(mismatched)
        outside = copy.deepcopy(self.request)
        outside["source"]["snapshot_path"] = "../outside.txt"
        with self.assertRaisesRegex(ProfileLockError, "snapshot_outside_root"):
            self.acquire(outside)

    def test_unannounced_install_strategy_is_rejected(self):
        bad = copy.deepcopy(self.request)
        bad["fields"]["install_strategy"] = "PYTHON_GLOBAL"
        with self.assertRaisesRegex(ProfileLockError, "install_strategy_invalid"):
            self.acquire(bad)

    def test_hash_and_unqualified_state_cannot_be_tampered(self):
        lock = self.acquire()
        tampered = copy.deepcopy(lock)
        tampered["fields"]["backend_probe"] = {"status": "verified"}
        with self.assertRaisesRegex(ProfileLockError, "lock_hash_mismatch"):
            verify_lock(tampered)

        promoted = copy.deepcopy(lock)
        promoted["qualification"] = "QUALIFIED"
        body = {key: value for key, value in promoted.items() if key != "lock_sha256"}
        promoted["lock_sha256"] = digest(body)
        with self.assertRaisesRegex(ProfileLockError, "lock_status_invalid"):
            verify_lock(promoted)

    def test_lock_creation_is_immutable_and_identical_replay_is_idempotent(self):
        lock = self.acquire()
        path = self.root / "profile-lock.json"
        self.assertEqual(write_immutable(path, lock), "CREATED")
        saved = path.read_bytes()
        self.assertEqual(write_immutable(path, lock), "REUSED")
        self.assertEqual(path.read_bytes(), saved)

        changed = copy.deepcopy(self.request)
        changed["backend"] = "VULKAN"
        replacement = self.acquire(changed)
        with self.assertRaisesRegex(ProfileLockError, "immutable_key_conflict"):
            write_immutable(path, replacement)
        self.assertEqual(path.read_bytes(), saved)
        self.assertEqual(verify_lock(json.loads(saved)), lock)

    def test_cli_acquires_and_verifies_the_saved_lock(self):
        request_path = self.root / "profile-input.json"
        request_path.write_text(json.dumps(self.request), encoding="utf-8")
        output_path = self.root / "profile-lock.json"
        stdout = io.StringIO()
        with patch.object(sys, "argv", ["runtime_profile_locks.py", "acquire", "--input", str(request_path), "--output", str(output_path)]):
            with redirect_stdout(stdout):
                self.assertEqual(cli.main(), 0)
        acquired = json.loads(stdout.getvalue())
        self.assertEqual(acquired["status"], "CREATED")
        self.assertEqual(acquired["qualification"], "UNQUALIFIED")

        stdout = io.StringIO()
        with patch.object(sys, "argv", ["runtime_profile_locks.py", "verify", "--lock", str(output_path)]):
            with redirect_stdout(stdout):
                self.assertEqual(cli.main(), 0)
        self.assertEqual(json.loads(stdout.getvalue())["status"], "PASS")


if __name__ == "__main__":
    unittest.main(verbosity=2)
