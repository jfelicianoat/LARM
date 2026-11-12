#!/usr/bin/env python3
"""Acquire and verify immutable, explicitly unqualified runtime profile locks.

This tool records reviewed source snapshots and exact profile inputs. It never
qualifies or enables a runtime: that requires the version-specific probes and
real smoke evidence owned by P099 and the runtime adapter prompts.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import sys
from pathlib import Path
from urllib.parse import urlsplit

REPO = Path(__file__).resolve().parent.parent
SDK = REPO / "adapters" / "sdk" / "python"
sys.path.insert(0, str(SDK))

from larm_adapter_sdk.capabilities import canonical, digest  # noqa: E402

LOCK_SCHEMA = "larm.runtime-profile-lock/1"
HASH = re.compile(r"^[0-9a-f]{64}$")
INSTALL_STRATEGIES = {
    "PREBUILT_BINARY",
    "GITHUB_RELEASE",
    "SOURCE_BUILD",
    "WINGET",
    "PIP",
    "UV",
    "CONDA",
    "MAMBA",
    "NUGET",
    "WSL",
    "CONTAINER",
    "CUSTOM",
    "MSI_MANAGED",
    "PYTHON_VENV",
    "EXTERNAL_ADOPTION",
}
REQUEST_KEYS = {
    "runtime",
    "source",
    "upstream_version",
    "os",
    "os_build",
    "backend",
    "protocol",
    "fields",
}
SOURCE_KEYS = {"url", "revision", "snapshot_path", "content_sha256"}
LOCK_KEYS = {"schema_version", "key", "fields", "qualification", "lock_sha256"}
KEY_KEYS = {
    "runtime",
    "source_url",
    "source_revision",
    "source_sha256",
    "upstream_version",
    "os",
    "os_build",
    "backend",
    "protocol",
}


class ProfileLockError(ValueError):
    """Invalid or conflicting runtime profile lock input."""


def load_catalog(path: Path | None = None) -> dict[str, tuple[str, ...]]:
    catalog_path = path or REPO / "docs" / "contracts" / "runtime-profiles.json"
    try:
        document = json.loads(catalog_path.read_text(encoding="utf-8"))
        profiles = document["profiles"]
        result: dict[str, tuple[str, ...]] = {}
        for profile in profiles:
            name = profile["runtime"]
            fields = profile["profile_required_fields"]
            if (
                not isinstance(name, str)
                or not name.strip()
                or not isinstance(fields, list)
                or not fields
                or any(not isinstance(field, str) or not field for field in fields)
                or len(set(fields)) != len(fields)
                or name in result
            ):
                raise ValueError("catalog entry")
            result[name] = tuple(fields)
        if len(result) != 20:
            raise ValueError("catalog must contain 20 unique runtimes")
        field_sets = {fields for fields in result.values()}
        if len(field_sets) != 1:
            raise ValueError("runtime profiles must share the declared required-field set")
        return result
    except (OSError, json.JSONDecodeError, KeyError, TypeError, ValueError) as exc:
        raise ProfileLockError("runtime_profile_catalog_invalid") from exc


def _nonempty_text(value: object, reason: str) -> str:
    if not isinstance(value, str) or not value.strip() or any(ord(c) < 32 for c in value):
        raise ProfileLockError(reason)
    return value


def _source_snapshot(request: dict, evidence_root: Path) -> tuple[dict, bytes]:
    source = request["source"]
    if not isinstance(source, dict) or set(source) != SOURCE_KEYS:
        raise ProfileLockError("runtime_profile_source_invalid")
    url = _nonempty_text(source["url"], "runtime_profile_source_invalid")
    parts = urlsplit(url)
    if parts.scheme != "https" or not parts.netloc or parts.username or parts.password or parts.fragment:
        raise ProfileLockError("runtime_profile_source_url_invalid")
    revision = _nonempty_text(source["revision"], "runtime_profile_source_revision_missing")
    if revision.casefold() in {"main", "master", "head", "latest", "stable"}:
        raise ProfileLockError("runtime_profile_source_revision_unpinned")
    expected_hash = source["content_sha256"]
    if not isinstance(expected_hash, str) or not HASH.fullmatch(expected_hash):
        raise ProfileLockError("runtime_profile_source_hash_invalid")
    snapshot_name = _nonempty_text(source["snapshot_path"], "runtime_profile_source_snapshot_missing")
    relative_snapshot = Path(snapshot_name)
    if relative_snapshot.is_absolute() or ".." in relative_snapshot.parts:
        raise ProfileLockError("runtime_profile_source_snapshot_outside_root")
    try:
        root = evidence_root.resolve(strict=True)
        snapshot = (root / relative_snapshot).resolve(strict=True)
    except OSError as exc:
        raise ProfileLockError("runtime_profile_source_snapshot_missing") from exc
    if not snapshot.is_relative_to(root) or not snapshot.is_file():
        raise ProfileLockError("runtime_profile_source_snapshot_outside_root")
    content = snapshot.read_bytes()
    if hashlib.sha256(content).hexdigest() != expected_hash:
        raise ProfileLockError("runtime_profile_source_snapshot_hash_mismatch")
    return {
        "url": url,
        "revision": revision,
        "content_sha256": expected_hash,
    }, content


def acquire_lock(
    request: object,
    *,
    evidence_root: Path,
    catalog: dict[str, tuple[str, ...]] | None = None,
) -> dict:
    """Create a deterministic lock record without granting qualification."""
    known = catalog or load_catalog()
    if not isinstance(request, dict) or set(request) != REQUEST_KEYS:
        raise ProfileLockError("runtime_profile_request_invalid")
    runtime = _nonempty_text(request["runtime"], "runtime_profile_runtime_unknown")
    required = known.get(runtime)
    if required is None:
        raise ProfileLockError("runtime_profile_runtime_unknown")
    version = _nonempty_text(request["upstream_version"], "runtime_profile_upstream_version_unknown")
    os_name = _nonempty_text(request["os"], "runtime_profile_os_missing")
    os_build = _nonempty_text(request["os_build"], "runtime_profile_os_build_missing")
    backend = _nonempty_text(request["backend"], "runtime_profile_backend_missing")
    protocol = _nonempty_text(request["protocol"], "runtime_profile_protocol_missing")
    fields = request["fields"]
    if not isinstance(fields, dict) or set(fields) != set(required):
        raise ProfileLockError("runtime_profile_required_fields_mismatch")
    if fields["upstream_version"] not in (None, version):
        raise ProfileLockError("runtime_profile_upstream_version_mismatch")
    strategies = fields["installation_strategies"]
    if not isinstance(strategies, list) or any(
        not isinstance(value, str) or value not in INSTALL_STRATEGIES for value in strategies
    ):
        raise ProfileLockError("runtime_profile_installation_strategies_invalid")
    if len(set(strategies)) != len(strategies):
        raise ProfileLockError("runtime_profile_installation_strategies_duplicate")
    install_strategy = fields["install_strategy"]
    if install_strategy is not None and (
        not isinstance(install_strategy, str) or install_strategy not in INSTALL_STRATEGIES
    ):
        raise ProfileLockError("runtime_profile_install_strategy_invalid")
    fixture_hashes = fields["fixture_hashes"]
    if not isinstance(fixture_hashes, (list, dict)) or not fixture_hashes:
        raise ProfileLockError("runtime_profile_fixtures_missing")
    fixture_values = fixture_hashes.values() if isinstance(fixture_hashes, dict) else fixture_hashes
    if any(not isinstance(value, str) or not HASH.fullmatch(value) for value in fixture_values):
        raise ProfileLockError("runtime_profile_fixture_hash_invalid")
    evidence_refs = fields["evidence_refs"]
    if not isinstance(evidence_refs, list) or not evidence_refs or any(
        not isinstance(value, str) or not value.strip() for value in evidence_refs
    ):
        raise ProfileLockError("runtime_profile_evidence_refs_missing")

    source, _content = _source_snapshot(request, evidence_root)
    lock = {
        "schema_version": LOCK_SCHEMA,
        "key": {
            "runtime": runtime,
            "source_url": source["url"],
            "source_revision": source["revision"],
            "source_sha256": source["content_sha256"],
            "upstream_version": version,
            "os": os_name,
            "os_build": os_build,
            "backend": backend,
            "protocol": protocol,
        },
        "fields": fields,
        # A complete record and pinned source are still only documentary/code
        # inputs. Real probes, qualification tests and smokes must promote it
        # through a separately authorized application workflow.
        "qualification": "UNQUALIFIED",
    }
    try:
        lock["lock_sha256"] = digest(lock)
        canonical(lock)
    except (TypeError, ValueError) as exc:
        raise ProfileLockError("runtime_profile_canonicalization_failed") from exc
    return lock


def verify_lock(lock: object, *, catalog: dict[str, tuple[str, ...]] | None = None) -> dict:
    known = catalog or load_catalog()
    if not isinstance(lock, dict) or set(lock) != LOCK_KEYS:
        raise ProfileLockError("runtime_profile_lock_invalid")
    if lock["schema_version"] != LOCK_SCHEMA or lock["qualification"] != "UNQUALIFIED":
        raise ProfileLockError("runtime_profile_lock_status_invalid")
    key = lock["key"]
    if not isinstance(key, dict) or set(key) != KEY_KEYS:
        raise ProfileLockError("runtime_profile_lock_key_invalid")
    runtime = key["runtime"]
    required = known.get(runtime) if isinstance(runtime, str) else None
    if required is None or not isinstance(lock["fields"], dict):
        raise ProfileLockError("runtime_profile_runtime_unknown")
    for name in KEY_KEYS - {"runtime", "source_sha256"}:
        _nonempty_text(key[name], "runtime_profile_lock_key_invalid")
    url = urlsplit(key["source_url"])
    if url.scheme != "https" or not url.netloc or url.username or url.password or url.fragment:
        raise ProfileLockError("runtime_profile_source_url_invalid")
    if key["source_revision"].casefold() in {"main", "master", "head", "latest", "stable"}:
        raise ProfileLockError("runtime_profile_source_revision_unpinned")
    if not isinstance(key["source_sha256"], str) or not HASH.fullmatch(key["source_sha256"]):
        raise ProfileLockError("runtime_profile_source_hash_invalid")
    if set(lock["fields"]) != set(required):
        raise ProfileLockError("runtime_profile_required_fields_mismatch")
    if lock["fields"]["upstream_version"] not in (None, key["upstream_version"]):
        raise ProfileLockError("runtime_profile_upstream_version_mismatch")
    strategies = lock["fields"]["installation_strategies"]
    if not isinstance(strategies, list) or any(
        not isinstance(value, str) or value not in INSTALL_STRATEGIES for value in strategies
    ):
        raise ProfileLockError("runtime_profile_installation_strategies_invalid")
    if len(set(strategies)) != len(strategies):
        raise ProfileLockError("runtime_profile_installation_strategies_duplicate")
    install_strategy = lock["fields"]["install_strategy"]
    if install_strategy is not None and (
        not isinstance(install_strategy, str) or install_strategy not in INSTALL_STRATEGIES
    ):
        raise ProfileLockError("runtime_profile_install_strategy_invalid")
    fixture_hashes = lock["fields"]["fixture_hashes"]
    if not isinstance(fixture_hashes, (list, dict)) or not fixture_hashes:
        raise ProfileLockError("runtime_profile_fixtures_missing")
    fixture_values = fixture_hashes.values() if isinstance(fixture_hashes, dict) else fixture_hashes
    if any(not isinstance(value, str) or not HASH.fullmatch(value) for value in fixture_values):
        raise ProfileLockError("runtime_profile_fixture_hash_invalid")
    evidence_refs = lock["fields"]["evidence_refs"]
    if not isinstance(evidence_refs, list) or not evidence_refs or any(
        not isinstance(value, str) or not value.strip() for value in evidence_refs
    ):
        raise ProfileLockError("runtime_profile_evidence_refs_missing")
    supplied_hash = lock["lock_sha256"]
    if not isinstance(supplied_hash, str) or not HASH.fullmatch(supplied_hash):
        raise ProfileLockError("runtime_profile_lock_hash_invalid")
    payload = {name: value for name, value in lock.items() if name != "lock_sha256"}
    if digest(payload) != supplied_hash:
        raise ProfileLockError("runtime_profile_lock_hash_mismatch")
    return lock


def write_immutable(path: Path, lock: dict) -> str:
    """Create once; an identical retry is a no-op, different bytes conflict."""
    data = canonical(lock) + b"\n"
    try:
        with path.open("xb") as stream:
            stream.write(data)
            stream.flush()
            os.fsync(stream.fileno())
    except FileExistsError:
        try:
            existing = json.loads(path.read_text(encoding="utf-8"))
            verify_lock(existing)
        except (OSError, json.JSONDecodeError, ProfileLockError) as exc:
            raise ProfileLockError("runtime_profile_existing_lock_invalid") from exc
        if canonical(existing) != canonical(lock):
            raise ProfileLockError("runtime_profile_immutable_key_conflict")
        return "REUSED"
    except OSError as exc:
        raise ProfileLockError("runtime_profile_lock_write_failed") from exc
    return "CREATED"


def _read_json(path: Path) -> object:
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        raise ProfileLockError("runtime_profile_json_invalid") from exc


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    acquire = commands.add_parser("acquire", help="create an immutable unqualified lock")
    acquire.add_argument("--input", required=True, type=Path)
    acquire.add_argument("--output", required=True, type=Path)
    verify = commands.add_parser("verify", help="verify a saved profile lock")
    verify.add_argument("--lock", required=True, type=Path)
    args = parser.parse_args()

    try:
        if args.command == "acquire":
            input_path = args.input.resolve(strict=True)
            request = _read_json(input_path)
            lock = acquire_lock(request, evidence_root=input_path.parent)
            action = write_immutable(args.output, lock)
            print(json.dumps({"status": action, "qualification": lock["qualification"], "lock_sha256": lock["lock_sha256"]}))
        else:
            lock = verify_lock(_read_json(args.lock))
            print(json.dumps({"status": "PASS", "qualification": lock["qualification"], "lock_sha256": lock["lock_sha256"]}))
    except (ProfileLockError, OSError) as exc:
        print(json.dumps({"status": "FAIL", "reason": str(exc)}), file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
