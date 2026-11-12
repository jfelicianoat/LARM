#!/usr/bin/env python3
"""Inspect llama.cpp builds beneath explicitly configured Windows roots.

This inventory probe is read-only. It does not search whole drives, install or
start a runtime, download release assets, or infer support from a build name.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
from pathlib import Path
from typing import Callable, Sequence

SCHEMA_VERSION = "larm.runtime.llama-cpp.observation/1"
MAX_CANDIDATES = 128
MAX_OUTPUT_BYTES = 1024 * 1024
PROBE_TIMEOUT_SECONDS = 10

EXECUTABLES = {
    "llama-server": "llama-server.exe",
    "llama-cli": "llama-cli.exe",
    "llama-bench": "llama-bench.exe",
    "llama-quantize": "llama-quantize.exe",
    "llama-gguf-split": "llama-gguf-split.exe",
}
CONVERTERS = (
    "convert_hf_to_gguf.py",
    "convert_lora_to_gguf.py",
    "convert_llama_ggml_to_gguf.py",
)
BIN_DIRECTORIES = (
    Path("."),
    Path("bin"),
    Path("bin/Release"),
    Path("build/bin"),
    Path("build/bin/Release"),
    Path("build/bin/RelWithDebInfo"),
)
VERSION_LINE = re.compile(
    r"^\s*(?:llama\.cpp\s+)?version\s*:\s*(?P<version>[^\r\n]+?)\s*$",
    re.IGNORECASE,
)
VERSION_COMMIT = re.compile(r"\b([0-9a-f]{7,40})\b", re.IGNORECASE)
COMMIT_LINE = re.compile(r"^\s*(?:build\s+)?commit\s*:\s*([0-9a-f]{7,40})\s*$", re.IGNORECASE)
BACKEND_PATTERNS = {
    "CPU": re.compile(r"\bCPU(?:\d+)?\s*:", re.IGNORECASE),
    "CUDA": re.compile(r"\bCUDA(?:\d+)?\s*:", re.IGNORECASE),
    "Vulkan": re.compile(r"\bVulkan(?:\d+)?\s*:", re.IGNORECASE),
    "SYCL": re.compile(r"\bSYCL(?:\d+)?\s*:", re.IGNORECASE),
    "HIP": re.compile(r"\bHIP(?:\d+)?\s*:", re.IGNORECASE),
    "ROCm": re.compile(r"\bROCm(?:\d+)?\s*:", re.IGNORECASE),
    "OpenVINO": re.compile(r"\bOpenVINO(?:\d+)?\s*:", re.IGNORECASE),
}

Run = Callable[..., subprocess.CompletedProcess[bytes]]


def _sha256_bytes(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()


def _sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def _safe_environment(executable_dir: Path, source: dict[str, str] | None = None) -> dict[str, str]:
    """Construct a minimal Windows environment and never inherit credentials."""
    source = source if source is not None else dict(os.environ)
    allowed = {
        "SYSTEMROOT",
        "SYSTEMDRIVE",
        "WINDIR",
        "NUMBER_OF_PROCESSORS",
        "PROCESSOR_ARCHITECTURE",
        "TEMP",
        "TMP",
    }
    safe = {key: value for key, value in source.items() if key.upper() in allowed}
    system_root = safe.get("SystemRoot") or safe.get("SYSTEMROOT")
    path_entries = [str(executable_dir)]
    if system_root:
        path_entries.append(str(Path(system_root) / "System32"))
    safe["PATH"] = os.pathsep.join(path_entries)
    return safe


def _run(
    executable: Path,
    args: Sequence[str],
    *,
    timeout: int = PROBE_TIMEOUT_SECONDS,
    run: Run = subprocess.run,
    environment: dict[str, str] | None = None,
) -> subprocess.CompletedProcess[bytes]:
    kwargs: dict[str, object] = {
        "capture_output": True,
        "check": False,
        "shell": False,
        "stdin": subprocess.DEVNULL,
        "timeout": timeout,
        "cwd": str(executable.parent),
        "env": _safe_environment(executable.parent, environment),
    }
    if os.name == "nt":
        kwargs["creationflags"] = subprocess.CREATE_NO_WINDOW
    return run([str(executable), *args], **kwargs)


def _text(value: bytes) -> str:
    return value[:MAX_OUTPUT_BYTES].decode("utf-8", errors="replace")


def _version_from_output(stdout: bytes, stderr: bytes) -> dict[str, object]:
    output = stdout if stdout.strip() else stderr
    lines = _text(output).splitlines()
    matches = [(line.strip(), VERSION_LINE.match(line)) for line in lines]
    matches = [(line, match) for line, match in matches if match]
    commits = [match.group(1).lower() for line in lines if (match := COMMIT_LINE.match(line))]
    if len(matches) != 1 or len(commits) > 1:
        return {
            "status": "UNKNOWN",
            "version_string": None,
            "commit": None,
            "build": None,
            "channel": "custom",
            "stdout_sha256": _sha256_bytes(stdout),
            "stderr_sha256": _sha256_bytes(stderr),
        }
    build, match = matches[0]
    assert match is not None
    version_value = match.group("version").strip()
    if not version_value or version_value.casefold() in {"latest", "stable", "nightly"}:
        return {
            "status": "UNKNOWN",
            "version_string": None,
            "commit": None,
            "build": build,
            "channel": "custom",
            "stdout_sha256": _sha256_bytes(stdout),
            "stderr_sha256": _sha256_bytes(stderr),
        }
    parenthesized_commit = re.search(r"\(([0-9a-f]{7,40})\)", version_value, re.IGNORECASE)
    commit = commits[0] if commits else (parenthesized_commit.group(1).lower() if parenthesized_commit else None)
    exact_version = (
        version_value[: parenthesized_commit.start()].strip()
        if parenthesized_commit
        else version_value
    )
    return {
        "status": "OBSERVED",
        "version_string": exact_version,
        "commit": commit,
        "build": build,
        "channel": "custom",
        "stdout_sha256": _sha256_bytes(stdout),
        "stderr_sha256": _sha256_bytes(stderr),
    }


def _backend_labels(output: bytes) -> list[str]:
    text = _text(output)
    return [name for name, pattern in BACKEND_PATTERNS.items() if pattern.search(text)]


def _configured_candidates(roots: Sequence[Path]) -> tuple[list[tuple[Path, Path]], list[str]]:
    candidates: dict[str, tuple[Path, Path]] = {}
    errors: list[str] = []
    for requested_root in roots:
        try:
            root = requested_root.expanduser().resolve(strict=True)
        except OSError:
            errors.append("configured_root_unavailable")
            continue
        if not root.is_dir():
            errors.append("configured_root_not_directory")
            continue
        directories = [root]
        try:
            children = sorted(
                (entry for entry in root.iterdir() if entry.is_dir()),
                key=lambda entry: entry.name.casefold(),
            )
        except OSError:
            errors.append("configured_root_unreadable")
            children = []
        for entry in children:
            try:
                resolved = entry.resolve(strict=True)
                if resolved.is_relative_to(root):
                    directories.append(resolved)
            except OSError:
                continue
        for directory in directories:
            key = os.path.normcase(str(directory)).casefold()
            candidates[key] = (root, directory)
    ordered = sorted(candidates.values(), key=lambda item: str(item[1]).casefold())
    if len(ordered) > MAX_CANDIDATES:
        errors.append("candidate_directory_limit_exceeded")
        ordered = ordered[:MAX_CANDIDATES]
    return ordered, errors


def _tool_paths(root: Path, configured_root: Path) -> dict[str, Path]:
    tools: dict[str, Path] = {}
    for directory in BIN_DIRECTORIES:
        bin_dir = (root / directory).resolve()
        try:
            if not bin_dir.is_relative_to(configured_root):
                continue
        except ValueError:
            continue
        for name, filename in EXECUTABLES.items():
            candidate = bin_dir / filename
            try:
                resolved = candidate.resolve(strict=True)
                if resolved.is_file() and resolved.is_relative_to(configured_root):
                    tools.setdefault(name, resolved)
            except OSError:
                continue
    return tools


def inspect_build(
    configured_root: Path,
    build_root: Path,
    *,
    run: Run = subprocess.run,
    environment: dict[str, str] | None = None,
) -> dict[str, object] | None:
    tools = _tool_paths(build_root, configured_root)
    if not tools:
        return None
    tool_records = {
        name: {"path": str(path), "sha256": _sha256_file(path)}
        for name, path in sorted(tools.items())
    }
    converter_records: dict[str, dict[str, str]] = {}
    for name in CONVERTERS:
        for candidate in (build_root / name, build_root / "convert" / name):
            try:
                resolved = candidate.resolve(strict=True)
                if resolved.is_file() and resolved.is_relative_to(configured_root):
                    converter_records[name] = {
                        "path": str(resolved),
                        "sha256": _sha256_file(resolved),
                    }
                    break
            except OSError:
                continue

    primary_name = "llama-server" if "llama-server" in tools else "llama-cli"
    primary = tools[primary_name]
    version: dict[str, object]
    try:
        version_result = _run(primary, ["--version"], run=run, environment=environment)
        if len(version_result.stdout) > MAX_OUTPUT_BYTES or len(version_result.stderr) > MAX_OUTPUT_BYTES:
            version = {"status": "OUTPUT_LIMIT", "version_string": None, "commit": None, "build": None,
                       "channel": "custom", "stdout_sha256": _sha256_bytes(version_result.stdout),
                       "stderr_sha256": _sha256_bytes(version_result.stderr)}
        elif version_result.returncode == 0:
            version = _version_from_output(version_result.stdout, version_result.stderr)
            version["exit_code"] = version_result.returncode
        else:
            version = {"status": "FAILED", "version_string": None, "commit": None, "build": None,
                       "channel": "custom", "exit_code": version_result.returncode,
                       "stdout_sha256": _sha256_bytes(version_result.stdout),
                       "stderr_sha256": _sha256_bytes(version_result.stderr)}
    except (OSError, subprocess.TimeoutExpired):
        version = {"status": "FAILED", "version_string": None, "commit": None, "build": None,
                   "channel": "custom", "exit_code": None, "stdout_sha256": _sha256_bytes(b""),
                   "stderr_sha256": _sha256_bytes(b"")}

    help_name = "llama-cli" if "llama-cli" in tools else primary_name
    help_tool = tools[help_name]
    try:
        help_result = _run(help_tool, ["--help"], run=run, environment=environment)
        help_supported = help_result.returncode == 0 and len(help_result.stdout) + len(help_result.stderr) <= MAX_OUTPUT_BYTES
        help_text = _text(help_result.stdout + b"\n" + help_result.stderr)
        list_devices_advertised = help_supported and "--list-devices" in help_text
        help_record = {
            "status": "OBSERVED" if help_supported else "FAILED",
            "tool": help_name,
            "exit_code": help_result.returncode,
            "stdout_sha256": _sha256_bytes(help_result.stdout),
            "stderr_sha256": _sha256_bytes(help_result.stderr),
            "list_devices_advertised": list_devices_advertised,
        }
    except (OSError, subprocess.TimeoutExpired):
        help_record = {
            "status": "FAILED", "tool": help_name, "exit_code": None,
            "stdout_sha256": _sha256_bytes(b""), "stderr_sha256": _sha256_bytes(b""),
            "list_devices_advertised": False,
        }

    if help_record["list_devices_advertised"]:
        devices_tool_name = "llama-cli" if "llama-cli" in tools else primary_name
        try:
            devices_result = _run(tools[devices_tool_name], ["--list-devices"], run=run, environment=environment)
            successful = devices_result.returncode == 0 and len(devices_result.stdout) + len(devices_result.stderr) <= MAX_OUTPUT_BYTES
            device_output = devices_result.stdout + b"\n" + devices_result.stderr
            backends = _backend_labels(device_output) if successful else []
            backend_record = {
                "status": "OBSERVED" if successful else "FAILED",
                "tool": devices_tool_name,
                "exit_code": devices_result.returncode,
                "output_sha256": _sha256_bytes(device_output),
                "backends": backends,
            }
        except (OSError, subprocess.TimeoutExpired):
            backend_record = {"status": "FAILED", "tool": devices_tool_name, "exit_code": None,
                              "output_sha256": _sha256_bytes(b""), "backends": []}
    else:
        backend_record = {
            "status": "UNSUPPORTED",
            "tool": help_name,
            "exit_code": None,
            "output_sha256": None,
            "backends": [],
        }

    return {
        "build_path": str(build_root),
        "tools": tool_records,
        "converters": converter_records,
        "version": version,
        "help": help_record,
        "backend_probe": backend_record,
        "qualification": "UNQUALIFIED",
        "profile_lock_created": False,
    }


def discover(
    roots: Sequence[Path],
    *,
    run: Run = subprocess.run,
    environment: dict[str, str] | None = None,
) -> dict[str, object]:
    """Inspect configured roots and one side-by-side directory level only."""
    candidates, errors = _configured_candidates(roots)
    installations = []
    for configured_root, build_root in candidates:
        observation = inspect_build(configured_root, build_root, run=run, environment=environment)
        if observation is not None:
            installations.append(observation)
    return {
        "schema_version": SCHEMA_VERSION,
        "configured_roots": [str(Path(root).expanduser().resolve()) for root in roots],
        "installations": installations,
        "unrecognized": errors,
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", action="append", required=True, type=Path,
                        help="una raíz llama.cpp configurada; se examina sólo la raíz y sus hijos directos")
    args = parser.parse_args()
    try:
        result = discover(args.root)
    except (OSError, ValueError) as exc:
        print(json.dumps({"status": "FAILED", "reason": type(exc).__name__}, ensure_ascii=False))
        return 1
    print(json.dumps(result, ensure_ascii=False, sort_keys=True))
    return 0


if __name__ == "__main__":
    sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    raise SystemExit(main())
