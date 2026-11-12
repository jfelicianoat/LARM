#!/usr/bin/env python3
"""Capture GET /api/version from a verified, already-running local Ollama.

The capture never starts or stops a service. It resolves the loopback listener's
PID and executable hash, uses it only when that hash is allowlisted, and checks
listener identity again after the HTTP response. The profile stays UNQUALIFIED.
"""

from __future__ import annotations

import argparse
import hashlib
import http.client
import json
import shutil
import uuid
from datetime import datetime, timezone
from pathlib import Path
from typing import Callable

from p099_capture_ollama import (
    CaptureError,
    EXPECTED_EXECUTABLE_SHA256,
    EXPECTED_VERSION,
    REPO,
    SCHEMA,
    SOURCE_REVISION,
    SOURCE_SHA256,
    SOURCE_SNAPSHOT,
    SOURCE_URL,
    RUN_ID_RE,
    _catalog_probe,
    _sha256,
    _sha256_file,
    _windows_host,
    _write_new,
)
from p099_ollama_listener import (
    ListenerInspectionError,
    listener_environment,
    listener_identity,
)

API_HOST = "127.0.0.1"
API_PORT = 11434
API_PATH = "/api/version"
API_TIMEOUT_SECONDS = 2.0
MAX_RESPONSE_BYTES = 64 * 1024
OBSERVATIONS = Path("docs/reports/P099_runs/api_observations")
ListenerLookup = Callable[[str, int, dict[str, str]], dict[str, object] | None]


def _get_version(
    host: str = API_HOST,
    port: int = API_PORT,
    path: str = API_PATH,
    timeout: float = API_TIMEOUT_SECONDS,
) -> tuple[int, bytes]:
    connection = http.client.HTTPConnection(host, port, timeout=timeout)
    try:
        connection.request("GET", path, headers={"Accept": "application/json"})
        response = connection.getresponse()
        return response.status, response.read(MAX_RESPONSE_BYTES + 1)
    finally:
        connection.close()


def _same_listener(
    first: dict[str, object] | None, second: dict[str, object] | None
) -> bool:
    return bool(
        first
        and second
        and first.get("pid") == second.get("pid")
        and first.get("executable_sha256") == second.get("executable_sha256")
        and first.get("executable") == second.get("executable")
    )


def capture_ollama_api(
    run_id: str,
    *,
    repo_root: Path = REPO,
    observation_root: Path | None = None,
    which: Callable[[str], str | None] = shutil.which,
    http_get: Callable[[str, int, str, float], tuple[int, bytes]] = _get_version,
    listener_lookup: ListenerLookup | None = None,
    environment: dict[str, str] | None = None,
) -> dict[str, object]:
    """Observe the single pinned local API endpoint without owning the server."""
    if not RUN_ID_RE.fullmatch(run_id) or run_id in {".", ".."}:
        raise CaptureError("runtime_profile_api_capture_run_id_invalid")
    root = (observation_root or (repo_root / OBSERVATIONS)).resolve()
    final_dir = root / run_id
    stage_dir = root / f".{run_id}.{uuid.uuid4().hex}.tmp"
    if final_dir.exists():
        raise CaptureError("runtime_profile_api_capture_already_exists")

    _catalog_probe(repo_root / "docs/contracts/runtime-profiles.json")
    executable = which("ollama")
    if not executable:
        raise CaptureError("runtime_profile_api_executable_not_found")
    executable_path = Path(executable).resolve(strict=True)
    if not executable_path.is_file():
        raise CaptureError("runtime_profile_api_executable_not_file")
    executable_sha256 = _sha256_file(executable_path)
    if executable_sha256 != EXPECTED_EXECUTABLE_SHA256:
        raise CaptureError("runtime_profile_api_executable_hash_not_allowlisted")
    source_path = (repo_root / SOURCE_SNAPSHOT).resolve(strict=True)
    if _sha256_file(source_path) != SOURCE_SHA256:
        raise CaptureError("runtime_profile_api_source_snapshot_hash_mismatch")

    host = _windows_host()
    safe_env = listener_environment(environment)
    if listener_lookup is None:
        def lookup(bind_host: str, port: int, env: dict[str, str]) -> dict[str, object] | None:
            return listener_identity(bind_host, port, environment=env)
    else:
        lookup = listener_lookup

    response_body = b""
    http_status: int | None = None
    observed_version: str | None = None
    owner: dict[str, object] | None = None
    capture_status = "API_UNAVAILABLE"
    failure_reason: str | None = "loopback_listener_missing"

    try:
        owner = lookup(API_HOST, API_PORT, safe_env)
    except ListenerInspectionError as exc:
        capture_status = "LISTENER_UNVERIFIED"
        failure_reason = str(exc)
    if owner is not None:
        if owner.get("executable_sha256") != EXPECTED_EXECUTABLE_SHA256:
            capture_status = "LISTENER_UNVERIFIED"
            failure_reason = "loopback_listener_binary_not_allowlisted"
        else:
            try:
                http_status, response_body = http_get(
                    API_HOST, API_PORT, API_PATH, API_TIMEOUT_SECONDS
                )
                current_owner = lookup(API_HOST, API_PORT, safe_env)
                if not _same_listener(owner, current_owner):
                    capture_status = "LISTENER_CHANGED"
                    failure_reason = "loopback_listener_changed_during_probe"
                elif len(response_body) > MAX_RESPONSE_BYTES:
                    capture_status = "API_RESPONSE_TOO_LARGE"
                    failure_reason = "api_response_size_limit"
                elif http_status != 200:
                    capture_status = "API_HTTP_ERROR"
                    failure_reason = "api_http_status_not_200"
                else:
                    try:
                        parsed = json.loads(response_body)
                    except (json.JSONDecodeError, UnicodeDecodeError):
                        parsed = None
                    if not isinstance(parsed, dict) or not isinstance(
                        parsed.get("version"), str
                    ):
                        capture_status = "API_RESPONSE_INVALID"
                        failure_reason = "api_version_response_invalid"
                    else:
                        observed_version = parsed["version"]
                        capture_status = (
                            "PASS" if observed_version == EXPECTED_VERSION else "API_VERSION_MISMATCH"
                        )
                        failure_reason = None if capture_status == "PASS" else "api_version_mismatch"
            except (OSError, http.client.HTTPException, ListenerInspectionError, TimeoutError):
                capture_status = "API_UNAVAILABLE"
                failure_reason = "api_request_failed"

    stage_dir.parent.mkdir(parents=True, exist_ok=True)
    stage_dir.mkdir()
    listener = owner or {}
    observation: dict[str, object] = {
        "schema_version": SCHEMA,
        "runtime": "Ollama",
        "upstream_version": observed_version,
        "expected_version": EXPECTED_VERSION,
        "capture_status": capture_status,
        "qualification": "UNQUALIFIED",
        "host": host,
        "captured_at": datetime.now(timezone.utc).isoformat(timespec="milliseconds").replace(
            "+00:00", "Z"
        ),
        "api_probe": {
            "method": "GET",
            "host": API_HOST,
            "port": API_PORT,
            "path": API_PATH,
            "http_status": http_status,
            "response_sha256": _sha256(response_body),
            "response_path": "api.response.bin",
            "timeout_seconds": API_TIMEOUT_SECONDS,
        },
        "service": {
            "ownership": "PREEXISTING_VERIFIED" if owner else "NOT_VERIFIED",
            "pid": listener.get("pid"),
            "executable": listener.get("executable"),
            "executable_sha256": listener.get("executable_sha256"),
            "started_by_capture": False,
            "stopped_by_capture": False,
        },
        "failure_reason": failure_reason,
        "source": {
            "url": SOURCE_URL,
            "revision": SOURCE_REVISION,
            "snapshot_path": SOURCE_SNAPSHOT.as_posix(),
            "content_sha256": SOURCE_SHA256,
        },
        "profile_lock_created": False,
    }
    canonical = json.dumps(
        observation, ensure_ascii=False, sort_keys=True, separators=(",", ":")
    ).encode("utf-8")
    observation["capture_sha256"] = _sha256(canonical)
    try:
        _write_new(stage_dir / "api.response.bin", response_body)
        _write_new(
            stage_dir / "capture.json",
            json.dumps(observation, ensure_ascii=False, sort_keys=True, indent=2).encode(
                "utf-8"
            )
            + b"\n",
        )
        stage_dir.replace(final_dir)
    except OSError as exc:
        shutil.rmtree(stage_dir, ignore_errors=True)
        raise CaptureError("runtime_profile_api_capture_write_failed") from exc

    return {
        "status": capture_status,
        "qualification": "UNQUALIFIED",
        "runtime": "Ollama",
        "upstream_version": observed_version,
        "capture_sha256": observation["capture_sha256"],
        "capture_path": (final_dir / "capture.json").relative_to(repo_root).as_posix(),
        "failure_reason": failure_reason,
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--run-id", required=True)
    args = parser.parse_args()
    try:
        result = capture_ollama_api(args.run_id)
    except (CaptureError, ListenerInspectionError, OSError) as exc:
        print(json.dumps({"status": "BLOCKED", "reason": str(exc)}, ensure_ascii=False))
        return 1
    print(json.dumps(result, ensure_ascii=False, sort_keys=True))
    return 0 if result["status"] == "PASS" else 1


if __name__ == "__main__":
    raise SystemExit(main())
