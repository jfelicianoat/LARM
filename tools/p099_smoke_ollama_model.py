#!/usr/bin/env python3
"""Capture one fixed generation smoke against a verified local Ollama listener."""

from __future__ import annotations

import argparse
import hashlib
import http.client
import json
import os
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
from p099_capture_ollama_api import API_HOST, API_PORT, _same_listener
from p099_ollama_listener import ListenerInspectionError, listener_environment, listener_identity

EXPECTED_MODEL = "gemma4:12b"
API_TIMEOUT_SECONDS = 2.0
GENERATION_TIMEOUT_SECONDS = 180.0
MAX_RESPONSE_BYTES = 1024 * 1024
PROMPT = "Reply with exactly this text and nothing else: LARM_OK"
EXPECTED_MARKER = "LARM_OK"
NUM_PREDICT = 64
OBSERVATIONS = Path("docs/reports/P099_runs/model_smokes")
ListenerLookup = Callable[[str, int, dict[str, str]], dict[str, object] | None]
HttpRequest = Callable[
    [str, str, int, str, dict[str, object] | None, float], tuple[int, bytes]
]


def _http_request(
    method: str,
    host: str,
    port: int,
    path: str,
    payload: dict[str, object] | None,
    timeout: float,
) -> tuple[int, bytes]:
    body = (
        json.dumps(payload, ensure_ascii=False, separators=(",", ":")).encode("utf-8")
        if payload is not None
        else None
    )
    headers = {"Accept": "application/json"}
    if body is not None:
        headers["Content-Type"] = "application/json"
    connection = http.client.HTTPConnection(host, port, timeout=timeout)
    try:
        connection.request(method, path, body=body, headers=headers)
        response = connection.getresponse()
        return response.status, response.read(MAX_RESPONSE_BYTES + 1)
    finally:
        connection.close()


def _parse_json(body: bytes) -> object | None:
    try:
        return json.loads(body)
    except (json.JSONDecodeError, UnicodeDecodeError):
        return None


def capture_ollama_model_smoke(
    run_id: str,
    *,
    repo_root: Path = REPO,
    observation_root: Path | None = None,
    which: Callable[[str], str | None] = shutil.which,
    http_request: HttpRequest = _http_request,
    listener_lookup: ListenerLookup | None = None,
    environment: dict[str, str] | None = None,
) -> dict[str, object]:
    """Read from a preexisting pinned listener; do not start or stop its service."""
    if not RUN_ID_RE.fullmatch(run_id) or run_id in {".", ".."}:
        raise CaptureError("runtime_profile_model_capture_run_id_invalid")
    root = (observation_root or (repo_root / OBSERVATIONS)).resolve()
    final_dir = root / run_id
    stage_dir = root / f".{run_id}.{uuid.uuid4().hex}.tmp"
    if final_dir.exists():
        raise CaptureError("runtime_profile_model_capture_already_exists")

    _catalog_probe(repo_root / "docs/contracts/runtime-profiles.json")
    executable = which("ollama")
    if not executable:
        raise CaptureError("runtime_profile_model_executable_not_found")
    executable_path = Path(executable).resolve(strict=True)
    if not executable_path.is_file():
        raise CaptureError("runtime_profile_model_executable_not_file")
    executable_sha256 = _sha256_file(executable_path)
    if executable_sha256 != EXPECTED_EXECUTABLE_SHA256:
        raise CaptureError("runtime_profile_model_executable_hash_not_allowlisted")
    source_path = (repo_root / SOURCE_SNAPSHOT).resolve(strict=True)
    if _sha256_file(source_path) != SOURCE_SHA256:
        raise CaptureError("runtime_profile_model_source_snapshot_hash_mismatch")

    host = _windows_host()
    safe_env = listener_environment(environment)
    if listener_lookup is None:
        def lookup(bind_host: str, port: int, env: dict[str, str]) -> dict[str, object] | None:
            return listener_identity(bind_host, port, environment=env)
    else:
        lookup = listener_lookup

    owner: dict[str, object] | None = None
    version_status: int | None = None
    version_body = b""
    tags_status: int | None = None
    tags_body = b""
    generation_status: int | None = None
    generation_body = b""
    version: str | None = None
    model_count: int | None = None
    generation: dict[str, object] | None = None
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
                version_status, version_body = http_request(
                    "GET", API_HOST, API_PORT, "/api/version", None, API_TIMEOUT_SECONDS
                )
                current = lookup(API_HOST, API_PORT, safe_env)
                if not _same_listener(owner, current):
                    capture_status = "LISTENER_CHANGED"
                    failure_reason = "loopback_listener_changed_after_version"
                elif len(version_body) > MAX_RESPONSE_BYTES:
                    capture_status = "API_RESPONSE_TOO_LARGE"
                    failure_reason = "api_version_response_size_limit"
                elif version_status != 200:
                    capture_status = "API_VERSION_HTTP_ERROR"
                    failure_reason = "api_version_http_status_not_200"
                else:
                    parsed_version = _parse_json(version_body)
                    version = (
                        parsed_version.get("version")
                        if isinstance(parsed_version, dict)
                        and isinstance(parsed_version.get("version"), str)
                        else None
                    )
                    if version != EXPECTED_VERSION:
                        capture_status = "API_VERSION_MISMATCH"
                        failure_reason = "api_version_mismatch"
                    else:
                        tags_status, tags_body = http_request(
                            "GET", API_HOST, API_PORT, "/api/tags", None, API_TIMEOUT_SECONDS
                        )
                        current = lookup(API_HOST, API_PORT, safe_env)
                        if not _same_listener(owner, current):
                            capture_status = "LISTENER_CHANGED"
                            failure_reason = "loopback_listener_changed_after_tags"
                        elif len(tags_body) > MAX_RESPONSE_BYTES:
                            capture_status = "API_TAGS_RESPONSE_TOO_LARGE"
                            failure_reason = "api_tags_response_size_limit"
                        elif tags_status != 200:
                            capture_status = "API_TAGS_HTTP_ERROR"
                            failure_reason = "api_tags_http_status_not_200"
                        else:
                            tags = _parse_json(tags_body)
                            models = tags.get("models") if isinstance(tags, dict) else None
                            if not isinstance(models, list):
                                capture_status = "API_TAGS_RESPONSE_INVALID"
                                failure_reason = "api_tags_response_invalid"
                            else:
                                model_count = len(models)
                                if not any(
                                    isinstance(model, dict)
                                    and model.get("name") == EXPECTED_MODEL
                                    for model in models
                                ):
                                    capture_status = "MODEL_NOT_INSTALLED"
                                    failure_reason = "allowlisted_model_not_installed"
                                else:
                                    request_body: dict[str, object] = {
                                        "model": EXPECTED_MODEL,
                                        "prompt": PROMPT,
                                        "think": False,
                                        "stream": False,
                                        "keep_alive": 0,
                                        "options": {
                                            "num_predict": NUM_PREDICT,
                                            "num_ctx": 512,
                                            "temperature": 0,
                                        },
                                    }
                                    generation_status, generation_body = http_request(
                                        "POST",
                                        API_HOST,
                                        API_PORT,
                                        "/api/generate",
                                        request_body,
                                        GENERATION_TIMEOUT_SECONDS,
                                    )
                                    current = lookup(API_HOST, API_PORT, safe_env)
                                    if not _same_listener(owner, current):
                                        capture_status = "LISTENER_CHANGED"
                                        failure_reason = "loopback_listener_changed_after_generate"
                                    elif len(generation_body) > MAX_RESPONSE_BYTES:
                                        capture_status = "MODEL_RESPONSE_TOO_LARGE"
                                        failure_reason = "api_generate_response_size_limit"
                                    elif generation_status != 200:
                                        capture_status = "MODEL_HTTP_ERROR"
                                        failure_reason = "api_generate_http_status_not_200"
                                    else:
                                        parsed_generation = _parse_json(generation_body)
                                        if not isinstance(parsed_generation, dict):
                                            capture_status = "MODEL_RESPONSE_INVALID"
                                            failure_reason = "api_generate_response_invalid"
                                        else:
                                            generation = parsed_generation
                                            output = generation.get("response")
                                            done = generation.get("done") is True
                                            marker_found = (
                                                isinstance(output, str)
                                                and EXPECTED_MARKER in output
                                            )
                                            capture_status = (
                                                "PASS" if done and marker_found else "MODEL_OUTPUT_MISMATCH"
                                            )
                                            failure_reason = (
                                                None
                                                if capture_status == "PASS"
                                                else "model_smoke_marker_or_done_missing"
                                            )
            except (OSError, http.client.HTTPException, ListenerInspectionError, TimeoutError):
                capture_status = "API_UNAVAILABLE"
                failure_reason = "api_request_or_listener_check_failed"

    stage_dir.parent.mkdir(parents=True, exist_ok=True)
    stage_dir.mkdir()
    request_body = {
        "model": EXPECTED_MODEL,
        "prompt": PROMPT,
        "think": False,
        "stream": False,
        "keep_alive": 0,
        "options": {"num_predict": NUM_PREDICT, "num_ctx": 512, "temperature": 0},
    }
    listener = owner or {}
    observation: dict[str, object] = {
        "schema_version": SCHEMA,
        "runtime": "Ollama",
        "upstream_version": version,
        "expected_version": EXPECTED_VERSION,
        "capture_status": capture_status,
        "qualification": "UNQUALIFIED",
        "host": host,
        "captured_at": datetime.now(timezone.utc).isoformat(timespec="milliseconds").replace(
            "+00:00", "Z"
        ),
        "api_probes": {
            "host": API_HOST,
            "port": API_PORT,
            "version": {
                "method": "GET",
                "path": "/api/version",
                "http_status": version_status,
                "response_sha256": _sha256(version_body),
            },
            "tags": {
                "method": "GET",
                "path": "/api/tags",
                "http_status": tags_status,
                "response_sha256": _sha256(tags_body),
                "model_count": model_count,
            },
            "generate": {
                "method": "POST",
                "path": "/api/generate",
                "http_status": generation_status,
                "model": EXPECTED_MODEL,
                "request_sha256": _sha256(
                    json.dumps(
                        request_body,
                        ensure_ascii=False,
                        sort_keys=True,
                        separators=(",", ":"),
                    ).encode("utf-8")
                ),
                "response_sha256": _sha256(generation_body),
                "response_path": "model.response.json",
                "done": generation.get("done") is True if generation else False,
                "done_reason": generation.get("done_reason") if generation else None,
                "output_contains_expected_marker": (
                    isinstance(generation.get("response"), str)
                    and EXPECTED_MARKER in generation["response"]
                )
                if generation
                else False,
                "keep_alive": 0,
                "think": False,
                "num_predict": NUM_PREDICT,
                "num_ctx": 512,
            },
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
        _write_new(stage_dir / "api.version.response.json", version_body)
        _write_new(stage_dir / "model.response.json", generation_body)
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
        raise CaptureError("runtime_profile_model_capture_write_failed") from exc

    output = generation.get("response") if generation else None
    return {
        "status": capture_status,
        "qualification": "UNQUALIFIED",
        "runtime": "Ollama",
        "upstream_version": version,
        "model": EXPECTED_MODEL,
        "model_count": model_count,
        "response_contains_expected_marker": isinstance(output, str)
        and EXPECTED_MARKER in output,
        "capture_sha256": observation["capture_sha256"],
        "capture_path": (final_dir / "capture.json").relative_to(repo_root).as_posix(),
        "failure_reason": failure_reason,
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--run-id", required=True)
    args = parser.parse_args()
    try:
        result = capture_ollama_model_smoke(args.run_id)
    except (CaptureError, ListenerInspectionError, OSError) as exc:
        print(json.dumps({"status": "BLOCKED", "reason": str(exc)}, ensure_ascii=False))
        return 1
    print(json.dumps(result, ensure_ascii=False, sort_keys=True))
    return 0 if result["status"] == "PASS" else 1


if __name__ == "__main__":
    raise SystemExit(main())
