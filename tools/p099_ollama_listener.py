#!/usr/bin/env python3
"""Identify a Windows loopback listener and its executable without modifying it."""

from __future__ import annotations

import ctypes
import ctypes.wintypes
import hashlib
import os
import platform
import shutil
import subprocess
from pathlib import Path
from typing import Callable


class ListenerInspectionError(ValueError):
    """The listener could not be attributed to one verified process."""


def listener_environment(source: dict[str, str] | None = None) -> dict[str, str]:
    """Keep the Windows runtime environment while removing credential variables."""
    blocked_markers = ("TOKEN", "API_KEY", "SECRET", "PASSWORD", "PASSWD", "CREDENTIAL")
    values = source if source is not None else dict(os.environ)
    return {
        name: value
        for name, value in values.items()
        if not any(marker in name.upper() for marker in blocked_markers)
    }


def _process_image_path(pid: int) -> Path:
    if platform.system() != "Windows":
        raise ListenerInspectionError("listener_inspection_requires_windows")
    kernel = ctypes.WinDLL("kernel32", use_last_error=True)
    kernel.OpenProcess.argtypes = [
        ctypes.wintypes.DWORD,
        ctypes.wintypes.BOOL,
        ctypes.wintypes.DWORD,
    ]
    kernel.OpenProcess.restype = ctypes.wintypes.HANDLE
    kernel.QueryFullProcessImageNameW.argtypes = [
        ctypes.wintypes.HANDLE,
        ctypes.wintypes.DWORD,
        ctypes.wintypes.LPWSTR,
        ctypes.POINTER(ctypes.wintypes.DWORD),
    ]
    kernel.QueryFullProcessImageNameW.restype = ctypes.wintypes.BOOL
    kernel.CloseHandle.argtypes = [ctypes.wintypes.HANDLE]

    process = kernel.OpenProcess(0x1000, False, pid)  # PROCESS_QUERY_LIMITED_INFORMATION
    if not process:
        raise ListenerInspectionError("listener_process_path_unavailable")
    try:
        buffer = ctypes.create_unicode_buffer(32768)
        size = ctypes.wintypes.DWORD(len(buffer))
        if not kernel.QueryFullProcessImageNameW(process, 0, buffer, ctypes.byref(size)):
            raise ListenerInspectionError("listener_process_path_unavailable")
        return Path(buffer.value)
    finally:
        kernel.CloseHandle(process)


def listener_identity(
    host: str,
    port: int,
    *,
    environment: dict[str, str] | None = None,
    run: Callable[..., subprocess.CompletedProcess[str]] = subprocess.run,
    image_path: Callable[[int], Path] = _process_image_path,
) -> dict[str, object] | None:
    """Return the sole TCP listener's PID/path/hash, or None if no listener exists."""
    if platform.system() != "Windows":
        raise ListenerInspectionError("listener_inspection_requires_windows")
    powershell = shutil.which("powershell.exe")
    if not powershell:
        raise ListenerInspectionError("listener_inspection_powershell_unavailable")
    if host not in {"127.0.0.1", "localhost"} or not 1 <= port <= 65535:
        raise ListenerInspectionError("listener_inspection_target_invalid")

    command = (
        "$ErrorActionPreference='Stop'; "
        f"Get-NetTCPConnection -State Listen -LocalPort {int(port)} "
        "-ErrorAction SilentlyContinue | Select-Object -ExpandProperty OwningProcess -Unique"
    )
    safe_environment = listener_environment(environment)
    try:
        result = run(
            [powershell, "-NoProfile", "-NonInteractive", "-Command", command],
            capture_output=True,
            check=False,
            shell=False,
            timeout=8,
            text=True,
            env=safe_environment,
        )
    except (OSError, subprocess.TimeoutExpired) as exc:
        raise ListenerInspectionError("listener_inspection_failed") from exc
    if result.returncode != 0:
        raise ListenerInspectionError("listener_inspection_failed")

    pids: set[int] = set()
    for line in result.stdout.splitlines():
        value = line.strip()
        if value:
            try:
                pids.add(int(value))
            except ValueError as exc:
                raise ListenerInspectionError("listener_pid_invalid") from exc
    if not pids:
        return None
    if len(pids) != 1:
        raise ListenerInspectionError("listener_multiple_owners")

    pid = next(iter(pids))
    path = image_path(pid).resolve(strict=True)
    if not path.is_file():
        raise ListenerInspectionError("listener_process_path_unavailable")
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return {"pid": pid, "executable": str(path), "executable_sha256": digest.hexdigest()}
