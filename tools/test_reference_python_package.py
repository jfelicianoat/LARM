"""Payload equivalence and actual isolated Windows imports for reference Python."""
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest
import zipfile

REPO = Path(__file__).resolve().parents[1]
RUNS = REPO / "docs/reports/P044_runs/remote-tcp-acquisition-20261029/stdlib-package-tests"
SPEC = importlib.util.spec_from_file_location("reference_packaging", REPO / "adapters/source/reference/package.py")
PACKAGE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(PACKAGE)
# Independent copy of the selection used by the original, filesystem package.
ORIGINAL_IGNORED = ("site-packages", "__pycache__", "test", "tests", "ensurepip", "idlelib", "turtledemo")


def owned_directory(owner):
    RUNS.mkdir(parents=True, exist_ok=True)
    temporary = tempfile.TemporaryDirectory(prefix="owned-", dir=RUNS)
    root = Path(temporary.name).resolve()
    if not root.is_relative_to(RUNS.resolve()):
        raise AssertionError("Temporary cleanup must stay within its owned run directory")
    owner.addCleanup(temporary.cleanup)
    return root


def payload(library, archive):
    result = {}
    if archive:
        with zipfile.ZipFile(archive) as source:
            for member in source.infolist():
                if not member.is_dir():
                    result[member.filename] = source.read(member)
    for file in library.rglob("*"):
        if file.is_file() and file.relative_to(library).parts[0] != "site-packages":
            name = file.relative_to(library).as_posix()
            if name in result:
                raise AssertionError(f"Duplicate logical stdlib file: {name}")
            result[name] = file.read_bytes()
    return result


class LayoutTests(unittest.TestCase):
    def test_complete_bytes_resources_native_groups_and_original_exclusions(self):
        root = owned_directory(self)
        source, destination = root / "source", root / "bin"
        source.mkdir()
        destination.mkdir()
        inputs = {"plain.py": b"VALUE=1\n", "pure/__init__.py": b"", "pure/área 模型.py": b"VALUE=2\n",
                  "native/__init__.py": b"VALUE=3\n", "native/extension.pyd": b"owned native fixture",
                  "resource/__init__.py": b"VALUE=4\n", "resource/data.json": b'{"all":"bytes"}',
                  "namespace/module.py": b"VALUE=5\n", "tests/excluded.py": b"original exclusion",
                  "pure/__pycache__/excluded.pyc": b"original exclusion"}
        for name, data in inputs.items():
            path = source / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(data)
        (source / "empty").mkdir()
        baseline = root / "baseline"
        shutil.copytree(source, baseline, ignore=shutil.ignore_patterns(*ORIGINAL_IGNORED))
        archive = PACKAGE.stdlib_archive(source, destination)
        self.assertEqual(payload(destination / "Lib", archive), payload(baseline, None))
        self.assertTrue((destination / "Lib/native/__init__.py").is_file())
        self.assertTrue((destination / "Lib/resource/data.json").is_file())
        self.assertTrue((destination / "Lib/empty").is_dir())
        self.assertFalse((destination / "Lib/plain.py").exists())
        with zipfile.ZipFile(archive) as bundle:
            self.assertIn("namespace/", bundle.namelist())
            self.assertTrue(all(item.compress_type == zipfile.ZIP_STORED for item in bundle.infolist()))
            self.assertTrue(all(item.date_time == (1980, 1, 1, 0, 0, 0) for item in bundle.infolist()))

    def test_archive_is_reproducible_despite_source_timestamps(self):
        root = owned_directory(self)
        source = root / "source"
        source.mkdir()
        (source / "module.py").write_bytes(b"PAYLOAD = 'complete'\n")
        first, second = root / "first", root / "second"
        first.mkdir()
        second.mkdir()
        archive = PACKAGE.stdlib_archive(source, first)
        os.utime(source / "module.py", (1_000_000_000, 1_000_000_000))
        self.assertEqual(archive.read_bytes(), PACKAGE.stdlib_archive(source, second).read_bytes())

    def test_all_shared_families_keep_isolated_archive_and_module_paths(self):
        root = owned_directory(self)
        source, destination = root / "source", root / "bin"
        source.mkdir()
        destination.mkdir()
        (source / "module.py").write_text("VALUE=1\n", encoding="utf-8")
        archive = PACKAGE.stdlib_archive(source, destination)
        config = destination / f"{archive.stem}._pth"
        for family in ("runtime", "source", "protocol", "transformation", "benchmark"):
            PACKAGE.module_paths(destination, family)
            self.assertEqual(config.read_text(encoding="utf-8").splitlines(),
                             [".", "DLLs", archive.name, "Lib", "Lib/site-packages", "../sdk", f"../{family}"])
        original = config.read_bytes()
        with self.assertRaises(ValueError):
            PACKAGE.module_paths(destination, "../outside")
        self.assertEqual(config.read_bytes(), original)


@unittest.skipUnless(sys.platform == "win32", "Actual pinned Windows interpreter required")
class NativeInterpreterTests(unittest.TestCase):
    def test_entire_original_stdlib_and_native_dependencies_remain_exact(self):
        root = owned_directory(self)
        destination = root / "bin"
        bill = PACKAGE.interpreter(REPO, destination)
        baseline = root / "original-Lib"
        base = Path(sys.base_prefix)
        shutil.copytree(base / "Lib", baseline, ignore=shutil.ignore_patterns(*ORIGINAL_IGNORED))
        archive = next(destination.glob("python*.zip"))
        actual, expected = payload(destination / "Lib", archive), payload(baseline, None)
        self.assertEqual(actual, expected)
        self.assertGreater(len(expected), 0)
        for original in (base / "python.exe", *base.glob("*.dll"), base / "LICENSE.txt"):
            self.assertEqual((destination / original.name).read_bytes(), original.read_bytes())
        for original in (base / "DLLs").rglob("*"):
            if original.is_file() and "__pycache__" not in original.relative_to(base / "DLLs").parts:
                self.assertEqual((destination / "DLLs" / original.relative_to(base / "DLLs")).read_bytes(), original.read_bytes())
        probe = r'''
import asyncio, concurrent.futures, ctypes, email, hashlib, importlib.metadata, json, ssl, sqlite3, sys, tomllib, urllib.request, venv, zipimport
from pathlib import Path
import attrs, attr, jsonschema, jsonschema_specifications, referencing, rpds, rfc3339_validator, rfc8785, six, typing_extensions
jsonschema.validate({"value": 1}, {"type": "object"})
assert len(rpds.HashTrieMap({"value": 1})) == 1
assert rfc3339_validator.validate_rfc3339("2026-10-29T00:00:00Z")
assert rfc8785.dumps({"value": 1}) == b'{"value":1}'
packages = ["jsonschema", "referencing", "attrs", "rpds-py", "jsonschema-specifications", "typing_extensions", "rfc3339-validator", "rfc8785", "six"]
print(json.dumps({"isolated": bool(sys.flags.isolated), "json_from_zip": isinstance(json.__loader__, zipimport.zipimporter),
"resource_packages_on_disk": all(Path(module.__file__).is_file() for module in (ctypes, email, tomllib, venv)),
"versions": {name: importlib.metadata.version(name) for name in packages}}))
'''
        completed = subprocess.run([str(destination / "python.exe"), "-I", "-B", "-c", probe],
                                   cwd=root, capture_output=True, text=True, encoding="utf-8", timeout=30,
                                   creationflags=subprocess.CREATE_NO_WINDOW)
        self.assertEqual(completed.returncode, 0, completed.stderr)
        result = json.loads(completed.stdout)
        self.assertTrue(result["isolated"] and result["json_from_zip"] and result["resource_packages_on_disk"])
        self.assertEqual(result["versions"], bill["packages"])
        print(json.dumps({"scope": "Entire original retained stdlib payload byte-for-byte, actual isolated Windows imports, native DLLs and pinned dependency versions",
                          "original_files": len(expected), "retained_bytes": sum(map(len, expected.values())),
                          "archive_sha256": hashlib.sha256(archive.read_bytes()).hexdigest(), "probe": result}))


if __name__ == "__main__":
    unittest.main()
