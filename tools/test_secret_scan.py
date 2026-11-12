#!/usr/bin/env python3
"""Oráculos del escáner con datos propios; no lee Credential Manager."""
from pathlib import Path
from contextlib import contextmanager
import tempfile
import unittest

from check_integration_credentials import CredentialMatchError, scan_file, scan_files, scan_paths


@contextmanager
def owned_directory():
    repo = Path(__file__).resolve().parent.parent
    base = repo / '.local/p026-secret-scan-tests'
    assert base.resolve().is_relative_to(repo.resolve())
    base.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(dir=base) as folder:
        assert Path(folder).resolve().is_relative_to(base.resolve())
        yield folder


class SecretScanTests(unittest.TestCase):
    def test_utf8_utf16_and_patterns_longer_than_chunk_cross_boundaries(self):
        with owned_directory() as folder:
            path = Path(folder) / 'owned.txt'
            for encoding in ('utf-8', 'utf-16-le'):
                pattern = 'synthetic-own-private-marker'.encode(encoding)
                for chunk in (1, 3, 8, 31, 128):
                    for offset in (0, 1, chunk - 1, chunk, chunk + 1):
                        path.write_bytes(b'x' * offset + pattern + b'z' * 257)
                        with self.subTest(encoding=encoding, chunk=chunk, offset=offset):
                            with self.assertRaises(CredentialMatchError) as error:
                                scan_file(path, [pattern], chunk)
                            self.assertNotIn('synthetic-own-private-marker', str(error.exception))

    def test_safe_large_binary_empty_and_partial_prefix_are_fully_read(self):
        with owned_directory() as folder:
            path = Path(folder) / 'owned.sqlite'
            pattern = b'full-private-pattern'
            for value in (b'', b'full-private-patter', b'\x00\xff' * (1 << 20) + pattern[:-1]):
                path.write_bytes(value)
                before = path.read_bytes()
                self.assertEqual(scan_file(path, [pattern], 4096), len(value))
                self.assertEqual(path.read_bytes(), before)

    def test_parallel_scan_checks_every_file_and_reports_exact_bytes(self):
        with owned_directory() as folder:
            paths = []
            total = 0
            for n in range(40):
                path = Path(folder) / f'{n}.json'
                value = (b'own-safe-data' + bytes([n])) * (n + 1)
                path.write_bytes(value)
                total += len(value)
                paths.append(path)
            counts = []
            self.assertEqual(scan_files(iter(paths), [b'nonexistent-private-pattern'], workers=4, progress=lambda n, _: counts.append(n)), (40, total))
            self.assertEqual(counts, list(range(1, 41)))

    def test_parallel_match_or_missing_file_never_becomes_pass(self):
        with owned_directory() as folder:
            safe = Path(folder) / 'safe.md'
            bad = Path(folder) / 'bad.ndjson'
            safe.write_bytes(b'safe')
            bad.write_bytes(b'prefix synthetic-secret suffix')
            with self.assertRaises(CredentialMatchError):
                scan_files([safe, bad], [b'synthetic-secret'], workers=2)
            with self.assertRaises(FileNotFoundError):
                scan_files([safe, Path(folder) / 'missing'], [b'synthetic-secret'], workers=2)

    def test_inventory_preserves_build_exclusions_and_includes_raw_and_wal(self):
        with owned_directory() as folder:
            root = Path(folder)
            wanted = ('code.py', 'raw.NDJSON', 'state.sqlite-wal', 'store.db-shm', 'rows.csv')
            for name in wanted:
                (root / name).write_bytes(b'safe')
            for name in ('.git', 'target', 'node_modules', '.venv-tools', '__pycache__'):
                directory = root / name
                directory.mkdir()
                (directory / 'excluded.json').write_bytes(b'not in defined scope')
            (root / 'not-selected.exe').write_bytes(b'not in defined scope')
            self.assertEqual({p.name for p in scan_paths(root)}, set(wanted))
            with self.assertRaises(FileNotFoundError):
                list(scan_paths(root / 'missing'))

    def test_invalid_limits_and_empty_patterns_fail_closed(self):
        with owned_directory() as folder:
            path = Path(folder) / 'safe.txt'
            path.write_bytes(b'safe')
            for patterns, chunk in (([], 4), ([b''], 4), ([b'x'], 0)):
                with self.assertRaises(ValueError):
                    scan_file(path, patterns, chunk)
            for workers in (0, 33):
                with self.assertRaises(ValueError):
                    scan_files([path], [b'x'], workers=workers)
            with self.assertRaises(ValueError):
                scan_files([], [])


if __name__ == '__main__':
    unittest.main(verbosity=2)
