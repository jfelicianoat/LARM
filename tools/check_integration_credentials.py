#!/usr/bin/env python3
"""Comprueba sólo las dos credenciales LARM enroladas y el auth/check del Broker.

CredReadW se limita a los nombres exactos generados por keyring 4.2.0.
No enumera el almacén, no imprime secretos y no envía nada a Agora.
"""
import ctypes
from concurrent.futures import FIRST_COMPLETED, ThreadPoolExecutor, wait
from ctypes import wintypes
import json
import os
import sys
import time
import urllib.request
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
SCAN_SUFFIXES = {".rs", ".py", ".ps1", ".ts", ".tsx", ".mjs", ".js", ".json", ".md", ".toml", ".css", ".html", ".log", ".sql", ".txt", ".yaml", ".yml", ".sqlite", ".sqlite3", ".db", ".ndjson", ".csv"}
SCAN_EXCLUDED_DIRS = {".git", "target", "node_modules", ".venv-tools", "__pycache__"}


class CredentialMatchError(RuntimeError):
    pass


def scan_paths(root):
    def fail(error):
        raise error
    for folder, dirs, files in os.walk(root, onerror=fail):
        dirs[:] = [name for name in dirs if name not in SCAN_EXCLUDED_DIRS]
        for filename in files:
            path = Path(folder) / filename
            if path.suffix.lower() in SCAN_SUFFIXES or filename.lower().endswith(('.sqlite-wal', '.sqlite-shm', '.db-wal', '.db-shm')):
                yield path


def scan_file(path, needles, chunk_size=1 << 20):
    if not needles or any(not isinstance(value, bytes) or not value for value in needles) or chunk_size < 1:
        raise ValueError('nonempty byte patterns and positive chunk size required')
    overlap_size = max(map(len, needles)) - 1
    overlap, total = b'', 0
    with path.open('rb') as source:
        while chunk := source.read(chunk_size):
            total += len(chunk)
            block = overlap + chunk
            if any(value in block for value in needles):
                # Nunca incluir el patrón, contenido ni nombre potencialmente secreto.
                raise CredentialMatchError('credential detected in inspected file')
            overlap = block[-overlap_size:] if overlap_size else b''
    return total


def scan_files(paths, needles, workers=8, progress=None):
    if not needles or any(not isinstance(value, bytes) or not value for value in needles):
        raise ValueError('nonempty byte patterns required')
    if workers < 1 or workers > 32:
        raise ValueError('scan concurrency out of bounds')
    iterator = iter(paths)
    scanned, total = 0, 0
    with ThreadPoolExecutor(max_workers=workers) as pool:
        pending = set()
        def schedule():
            path = next(iterator, None)
            if path is not None:
                pending.add(pool.submit(scan_file, path, needles))
        # Cola acotada: no materializar todos los paths ni todos los archivos.
        for _ in range(workers * 2):
            schedule()
        while pending:
            completed, _ = wait(pending, return_when=FIRST_COMPLETED)
            for result in completed:
                pending.remove(result)
                total += result.result()  # IO/error/match nunca se omiten.
                scanned += 1
                if progress:
                    progress(scanned, total)
                schedule()
    return scanned, total


class Credential(ctypes.Structure):
    _fields_ = [("Flags", wintypes.DWORD), ("Type", wintypes.DWORD), ("TargetName", wintypes.LPWSTR),
                ("Comment", wintypes.LPWSTR), ("LastWritten", wintypes.FILETIME), ("CredentialBlobSize", wintypes.DWORD),
                ("CredentialBlob", ctypes.POINTER(ctypes.c_ubyte)), ("Persist", wintypes.DWORD),
                ("AttributeCount", wintypes.DWORD), ("Attributes", ctypes.c_void_p),
                ("TargetAlias", wintypes.LPWSTR), ("UserName", wintypes.LPWSTR)]


def secret(name: str) -> str:
    assert name in ("ai-broker", "agora")
    api = ctypes.WinDLL("advapi32", use_last_error=True)
    pointer = ctypes.POINTER(Credential)()
    api.CredReadW.argtypes = [wintypes.LPCWSTR, wintypes.DWORD, wintypes.DWORD, ctypes.POINTER(ctypes.POINTER(Credential))]
    api.CredReadW.restype = wintypes.BOOL
    api.CredFree.argtypes = [ctypes.c_void_p]
    api.CredFree.restype = None
    if not api.CredReadW(f"operator.larm/integration/{name}", 1, 0, ctypes.byref(pointer)):
        raise RuntimeError(f"Credencial {name} no disponible; Win32 {ctypes.get_last_error()}")
    try:
        value = ctypes.string_at(pointer.contents.CredentialBlob, pointer.contents.CredentialBlobSize).decode("utf-16-le")
        assert value, "credencial vacía"
        return value
    finally:
        api.CredFree(pointer)


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


def main() -> int:
    references = json.loads((REPO / ".local/integrations.json").read_text(encoding="utf-8"))["credentials"]
    names = ("ai-broker",) if "--broker-only" in sys.argv else ("ai-broker", "agora")
    for name in names:
        assert references[name]["service"] == f"larm/integration/{name}"
        assert references[name]["username"] == "operator"
        assert references[name]["backend"] == "WINDOWS_CREDENTIAL_MANAGER"
        value = secret(name)
        print(f"PASS: credencial {name} recuperable desde Credential Manager")
        del value
    if "--scan-only" in sys.argv:
        needles = [secret(name).encode(encoding) for name in names for encoding in ("utf-8", "utf-16-le")]
        last = time.monotonic()
        def progress(scanned, total):
            nonlocal last
            if time.monotonic() - last >= 30:
                print(f'SCAN_PROGRESS: files={scanned} bytes={total}', flush=True)
                last = time.monotonic()
        scanned, total = scan_files(scan_paths(REPO), needles, progress=progress)
        print(f"PASS: tokens ausentes de {scanned} archivos de código, datos operativos, documentación, builds y logs; bytes={total}; workers=8")
        return 0
    token = secret("ai-broker")
    request = urllib.request.Request("http://127.0.0.1:8765/api/v1/auth/check", headers={"X-Admin-Token": token})
    with urllib.request.build_opener(NoRedirect).open(request, timeout=10) as response:
        result = json.load(response)
        assert response.status == 200 and result["authenticated"] is True and result["auth_required"] is True
    del token, request
    print("PASS: Broker auth/check HTTP 200, authenticated=true, auth_required=true")
    return 0


if __name__ == "__main__":
    sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    try:
        raise SystemExit(main())
    except Exception as exc:
        print(f"FAIL: {type(exc).__name__}")
        raise SystemExit(1)
