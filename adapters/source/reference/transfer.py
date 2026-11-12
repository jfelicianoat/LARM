"""IO privado de la fuente de referencia P025; no sustituye DownloadManager.

Origen fijado, roots explícitas, partial durable, ETag/Range y SHA256 final.
No crea un JobReceipt ni afirma admisión/journal global del Core (P050).
"""
import hashlib
import http.client
import json
import os
import re
import shutil
import stat
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path

from larm_adapter_sdk import DomainError
from larm_adapter_sdk.access import fail, relative, path_allowed, origin, NoRedirect

MAX_FIXTURE_BYTES = 64 << 20
CHUNK = 16 << 10


def open_public(url, origins, headers, *, allow_unsatisfied_range=False):
    if origin(url) not in {origin(value) for value in origins}:
        raise fail("FORBIDDEN", "reference_origin_not_allowed")
    request = urllib.request.Request(url, headers=headers)
    # Ni proxies heredados ni redirecciones; no se recibe ni transmite un token.
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), NoRedirect())
    try:
        return opener.open(request, timeout=2)
    except urllib.error.HTTPError as error:
        if allow_unsatisfied_range and error.code == 416:
            return error
        error.close()
        if error.code in (401, 403):
            raise fail("AUTH_REQUIRED", "source_auth_required") from None
        if error.code == 429:
            def decimal(name):
                value = error.headers.get(name)
                return value if value and re.fullmatch(r"\d{1,18}", value) else None
            rate = {"limit": decimal("X-RateLimit-Limit"), "remaining": decimal("X-RateLimit-Remaining"), "reset_at": None, "retry_after_seconds": decimal("Retry-After")}
            failure = fail("UPSTREAM_UNAVAILABLE", "source_rate_limited", retryable=True)
            failure.data["details"]["original_error"] = {"system": "source", "code": "429", "message": json.dumps(rate, separators=(",", ":"))}
            raise failure from None
        if 300 <= error.code < 400:
            raise fail("FORBIDDEN", "reference_redirect_denied") from None
        raise fail("UPSTREAM_UNAVAILABLE", "source_http_error", retryable=True) from None
    except (OSError, urllib.error.URLError, ValueError):
        raise fail("UPSTREAM_UNAVAILABLE", "source_network_interrupted", retryable=True) from None


def file_hash(path):
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        while chunk := stream.read(CHUNK):
            digest.update(chunk)
    return digest.hexdigest()


def publish(partial, target, write_roots):
    path_allowed(partial, write_roots)
    path_allowed(target, write_roots, writing=True)
    target.parent.mkdir(parents=True, exist_ok=True)
    path_allowed(target.parent, write_roots)
    try:
        os.rename(partial, target)  # Windows: no reemplaza un destino existente.
    except FileExistsError:
        raise fail("CONFLICT", "reference_target_appeared") from None
    except OSError:
        raise fail("RESOURCE_UNAVAILABLE", "reference_publish_failed") from None


def transfer(*, url, target, partial, size, sha256, etag, read_roots, write_roots, origins, ctx, hard_budget=None):
    """Copia una fixture fijada; el caller posee serialización/admisión y journal.

    La referencia sólo admite Windows, archivos de hasta 64 MiB, tamaño y SHA
    conocidos. Reintentar con el mismo partial conserva bytes sólo con ETag fuerte.
    No sobrescribe un destino existente ni publica bytes con checksum incorrecto.
    """
    if os.name != "nt":
        raise fail("UNSUPPORTED_CAPABILITY", "reference_windows_only")
    if not isinstance(size, int) or isinstance(size, bool) or not 0 <= size <= MAX_FIXTURE_BYTES or not re.fullmatch(r"[0-9a-f]{64}", sha256):
        raise fail("VALIDATION_FAILED", "reference_artifact_unpinned")
    if hard_budget is not None and (not isinstance(hard_budget, int) or isinstance(hard_budget, bool) or hard_budget < size):
        raise fail("STORAGE_INSUFFICIENT", "reference_budget_exceeded")
    if etag is not None and (not re.fullmatch(r'"[^"\r\n]+"', etag) or etag.startswith('W/')):
        raise fail("VALIDATION_FAILED", "reference_strong_etag_required")
    target = path_allowed(target, write_roots, writing=True)
    partial = path_allowed(partial, write_roots, writing=True)
    path_allowed(partial, read_roots, writing=True)  # verificar bytes exige lectura explícita
    if target == partial:
        raise fail("VALIDATION_FAILED", "reference_partial_is_target")
    # Declara y comprueba el origen antes de crear una carpeta o un partial.
    if origin(url) not in {origin(value) for value in origins}:
        raise fail("FORBIDDEN", "reference_origin_not_allowed")
    ctx.check_cancelled()
    if target.exists():
        path_allowed(target, read_roots)
        if target.stat().st_size == size and file_hash(target) == sha256:
            return {"received_bytes": 0, "resumed_from": 0, "reused": True}
        raise fail("INTEGRITY_FAILED", "reference_existing_target_mismatch")
    root = partial.parent
    while not root.exists():
        root = root.parent
    # Budget privado y conservador; no afirma conocer reservas globales P050.
    margin = max(1 << 30, (size + 19) // 20)
    if shutil.disk_usage(root).free < size + margin:
        raise fail("STORAGE_INSUFFICIENT", "reference_free_space_insufficient")
    offset = 0
    if partial.exists():
        path_allowed(partial, read_roots)
        offset = partial.stat().st_size
        if offset > size:
            raise fail("INTEGRITY_FAILED", "reference_partial_too_large")
        if offset == size:
            if file_hash(partial) != sha256:
                raise fail("INTEGRITY_FAILED", "reference_partial_hash_mismatch")
            ctx.check_cancelled()
            publish(partial, target, write_roots)
            return {"received_bytes": 0, "resumed_from": offset, "reused": False}
        if etag is None:
            offset = 0
    headers = {"Accept-Encoding": "identity"}
    if offset:
        headers |= {"Range": f"bytes={offset}-", "If-Range": etag}
    with open_public(url, origins, headers) as response:
        if response.headers.get("Content-Encoding", "identity").lower() != "identity":
            raise fail("PROTOCOL_ERROR", "reference_content_encoding")
        if etag is not None and response.headers.get("ETag") != etag:
            raise fail("CONFLICT", "source_revision_changed")
        if response.status == 206:
            expected_range = f"bytes {offset}-{size - 1}/{size}"
            if not offset or response.headers.get("Content-Range") != expected_range:
                raise fail("PROTOCOL_ERROR", "reference_content_range_mismatch")
        elif response.status == 200:
            offset = 0  # Range ignorado: reiniciar de cero, nunca concatenar.
        else:
            raise fail("PROTOCOL_ERROR", "reference_response_status")
        declared = response.headers.get("Content-Length")
        if declared is not None and (not declared.isdecimal() or int(declared) != size - offset):
            raise fail("INTEGRITY_FAILED", "reference_content_length_mismatch")
        for parent in (target.parent, partial.parent):
            parent.mkdir(parents=True, exist_ok=True)
            path_allowed(parent, write_roots)
        received = 0
        try:
            path_allowed(partial, write_roots, writing=True)
            with partial.open("r+b" if partial.exists() else "x+b") as output:
                output.seek(offset)
                output.truncate(offset)
                try:
                    ctx.progress("DOWNLOAD", offset, size, "reference_download")
                    while True:
                        ctx.check_cancelled()
                        try:
                            chunk = response.read(CHUNK)
                        except (OSError, http.client.HTTPException):
                            raise fail("UPSTREAM_UNAVAILABLE", "source_network_interrupted", retryable=True) from None
                        if not chunk:
                            break
                        if offset + received + len(chunk) > size:
                            raise fail("INTEGRITY_FAILED", "reference_response_too_large")
                        output.write(chunk)
                        received += len(chunk)
                        output.flush()
                        os.fsync(output.fileno())
                        ctx.progress("DOWNLOAD", offset + received, size, "reference_download")
                finally:
                    output.flush()
                    os.fsync(output.fileno())
        except OSError:
            raise fail("RESOURCE_UNAVAILABLE", "reference_write_failed") from None
    if partial.stat().st_size < size:
        raise fail("UPSTREAM_UNAVAILABLE", "source_network_interrupted", retryable=True)
    path_allowed(partial, read_roots)
    if file_hash(partial) != sha256:
        raise fail("INTEGRITY_FAILED", "reference_checksum_mismatch")
    ctx.check_cancelled()
    path_allowed(partial, read_roots)
    publish(partial, target, write_roots)
    return {"received_bytes": received, "resumed_from": offset, "reused": False}
