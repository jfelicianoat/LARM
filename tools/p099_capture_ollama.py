#!/usr/bin/env python3
"""Capture one allowlisted, local Ollama version observation for P099.

This records an observation only. It does not create a runtime profile lock,
probe an HTTP API, or qualify any capability. Commands and the executable hash
are pinned below so capture never runs an arbitrary catalog command or binary.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import platform
import re
import shutil
import subprocess
import sys
import uuid
from datetime import datetime, timezone
from pathlib import Path
from typing import Callable

REPO = Path(__file__).resolve().parent.parent
OBSERVATIONS = REPO / "docs" / "reports" / "P099_runs" / "observations"
CATALOG = REPO / "docs" / "contracts" / "runtime-profiles.json"
SCHEMA = "larm.runtime-profile-observation/1"
EXPECTED_VERSION = "0.40.6"
EXPECTED_EXECUTABLE_SHA256 = "71d2002d3ae572d5fa8c1d368ce9d1b9a39c7187cf169e8b330c55410bf6c58e"
SOURCE_REVISION = "2476d5ead197abe04629651028f83d48dc75d2b2"
SOURCE_URL = f"https://github.com/ollama/ollama/blob/{SOURCE_REVISION}/docs/api.md"
SOURCE_SNAPSHOT = Path(
    "docs/reports/P099_runs/sources/ollama/2476d5e/docs/api.md"
)
SOURCE_SHA256 = "96db03080e385dbabf5d70e75f298d41b75983bc30975cae56f7119f848dd0f5"
VERSION_RE = re.compile(
    r"(?i)\b(?:client|ollama)\s+version\s+is\s+v?([0-9]+(?:\.[0-9]+){1,3}(?:[-+][0-9A-Za-z.-]+)?)"
)
RUN_ID_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]{0,79}$")


class CaptureError(ValueError):
    """The local observation cannot be trusted or safely captured."""


def _sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def _sha256_file(path: Path) -> str:
    hasher = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            hasher.update(chunk)
    return hasher.hexdigest()


def _catalog_probe(catalog_path: Path) -> tuple[str, list[str]]:
    try:
        catalog = json.loads(catalog_path.read_text(encoding="utf-8"))
        profile = next(item for item in catalog["profiles"] if item["runtime"] == "Ollama")
        probe = profile["version_probe"]
    except (OSError, json.JSONDecodeError, KeyError, StopIteration, TypeError) as exc:
        raise CaptureError("runtime_profile_catalog_invalid") from exc
    if (
        probe != {"executable": "ollama", "args": ["--version"], "read_only": True}
    ):
        raise CaptureError("runtime_probe_not_allowlisted")
    return probe["executable"], list(probe["args"])


def _windows_host() -> dict[str, object]:
    release, version, service_pack, _ = platform.win32_ver()
    if platform.system() != "Windows":
        raise CaptureError("runtime_profile_host_not_windows")
    try:
        import winreg

        with winreg.OpenKey(
            winreg.HKEY_LOCAL_MACHINE,
            r"SOFTWARE\Microsoft\Windows NT\CurrentVersion",
        ) as key:
            def read_value(name: str) -> object:
                try:
                    return winreg.QueryValueEx(key, name)[0]
                except OSError:
                    return None

            registry = {
                "edition_id": read_value("EditionID"),
                "display_version": read_value("DisplayVersion"),
                "build_number": read_value("CurrentBuildNumber"),
                "update_build_revision": read_value("UBR"),
            }
    except OSError as exc:
        raise CaptureError("runtime_profile_windows_metadata_unavailable") from exc
    return {
        "host_id": platform.node(),
        "os": f"Windows {release}" if release else "Windows",
        "os_version": version or platform.version(),
        "service_pack": service_pack,
        **registry,
        "architecture": platform.machine(),
    }


def _safe_environment(source: dict[str, str]) -> dict[str, str]:
    allowed = {
        "PATH",
        "SystemRoot",
        "WINDIR",
        "TEMP",
        "TMP",
        "USERPROFILE",
        "APPDATA",
        "LOCALAPPDATA",
        "HOMEDRIVE",
        "HOMEPATH",
    }
    return {name: source[name] for name in allowed if source.get(name)}


def _write_new(path: Path, data: bytes) -> None:
    with path.open("xb") as stream:
        stream.write(data)
        stream.flush()
        os.fsync(stream.fileno())


def capture_ollama(
    run_id: str,
    *,
    repo_root: Path = REPO,
    observation_root: Path | None = None,
    which: Callable[[str], str | None] = shutil.which,
    run: Callable[..., subprocess.CompletedProcess[bytes]] = subprocess.run,
    environment: dict[str, str] | None = None,
) -> dict[str, object]:
    """Run the single pinned version probe and atomically save its evidence."""
    if not RUN_ID_RE.fullmatch(run_id) or run_id in {".", ".."}:
        raise CaptureError("runtime_profile_capture_run_id_invalid")

    root = (observation_root or (repo_root / OBSERVATIONS.relative_to(REPO))).resolve()
    final_dir = root / run_id
    stage_dir = root / f".{run_id}.{uuid.uuid4().hex}.tmp"
    if final_dir.exists():
        raise CaptureError("runtime_profile_capture_already_exists")

    exe_name, args = _catalog_probe(repo_root / CATALOG.relative_to(REPO))
    executable = which(exe_name)
    if not executable:
        raise CaptureError("runtime_profile_executable_not_found")
    executable_path = Path(executable).resolve(strict=True)
    if not executable_path.is_file():
        raise CaptureError("runtime_profile_executable_not_file")
    executable_sha256 = _sha256_file(executable_path)
    if executable_sha256 != EXPECTED_EXECUTABLE_SHA256:
        raise CaptureError("runtime_profile_executable_hash_not_allowlisted")

    source_path = (repo_root / SOURCE_SNAPSHOT).resolve(strict=True)
    if _sha256_file(source_path) != SOURCE_SHA256:
        raise CaptureError("runtime_profile_source_snapshot_hash_mismatch")

    host = _windows_host()
    safe_env = _safe_environment(environment if environment is not None else dict(os.environ))
    command = [str(executable_path), *args]
    try:
        proc = run(
            command,
            cwd=str(repo_root),
            stdin=subprocess.DEVNULL,
            capture_output=True,
            check=False,
            shell=False,
            timeout=15,
            env=safe_env,
        )
        stdout = proc.stdout or b""
        stderr = proc.stderr or b""
        exit_code = proc.returncode
    except subprocess.TimeoutExpired as exc:
        stdout = exc.stdout or b""
        stderr = exc.stderr or b""
        exit_code = None

    text_output = (stdout + b"\n" + stderr).decode("utf-8", errors="replace")
    version_match = VERSION_RE.search(text_output)
    observed_version = version_match.group(1) if version_match else None
    capture_status = (
        "PASS"
        if exit_code == 0 and observed_version == EXPECTED_VERSION
        else "VERSION_OR_PROBE_MISMATCH"
    )

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
        "probe": {
            "executable": str(executable_path),
            "executable_sha256": executable_sha256,
            "args": args,
            "exit_code": exit_code,
            "stdout_sha256": _sha256(stdout),
            "stderr_sha256": _sha256(stderr),
            "stdout_path": "probe.stdout.bin",
            "stderr_path": "probe.stderr.bin",
        },
        "source": {
            "url": SOURCE_URL,
            "revision": SOURCE_REVISION,
            "snapshot_path": SOURCE_SNAPSHOT.as_posix(),
            "content_sha256": SOURCE_SHA256,
        },
        "api_compatibility": "NOT_TESTED",
        "profile_lock_created": False,
    }
    canonical = json.dumps(
        observation, ensure_ascii=False, sort_keys=True, separators=(",", ":")
    ).encode("utf-8")
    observation["capture_sha256"] = _sha256(canonical)
    capture_json = json.dumps(observation, ensure_ascii=False, sort_keys=True, indent=2).encode(
        "utf-8"
    ) + b"\n"

    root.mkdir(parents=True, exist_ok=True)
    stage_dir.mkdir()
    try:
        _write_new(stage_dir / "probe.stdout.bin", stdout)
        _write_new(stage_dir / "probe.stderr.bin", stderr)
        _write_new(stage_dir / "capture.json", capture_json)
        stage_dir.replace(final_dir)
    except OSError as exc:
        shutil.rmtree(stage_dir, ignore_errors=True)
        raise CaptureError("runtime_profile_capture_write_failed") from exc

    return {
        "status": capture_status,
        "qualification": "UNQUALIFIED",
        "runtime": "Ollama",
        "upstream_version": observed_version,
        "capture_sha256": observation["capture_sha256"],
        "capture_path": (final_dir / "capture.json").relative_to(repo_root).as_posix(),
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--run-id", required=True)
    args = parser.parse_args()
    try:
        result = capture_ollama(args.run_id)
    except (CaptureError, OSError) as exc:
        print(json.dumps({"status": "BLOCKED", "reason": str(exc)}, ensure_ascii=False))
        return 1
    print(json.dumps(result, ensure_ascii=False, sort_keys=True))
    return 0 if result["status"] == "PASS" else 1


if __name__ == "__main__":
    raise SystemExit(main())
