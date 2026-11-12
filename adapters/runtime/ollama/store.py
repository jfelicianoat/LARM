"""Read-only Ollama manifest/blob inspection; no daemon, copying or promotion."""
from __future__ import annotations

from contextlib import ExitStack, contextmanager
import ctypes
from ctypes import wintypes
import hashlib
import json
import os
from pathlib import Path
import re
import stat

from larm_adapter_sdk import DomainError
from larm_adapter_sdk.access import path_allowed, relative

MAX_JSON_BYTES = 1 << 20
MAX_JSON_DEPTH = 64
MAX_FILES = 512
MAX_DEPTH = 8
MAX_BYTES = (1 << 63) - 1
CHUNK = 128 << 10
DIGEST = re.compile(r"sha256:([0-9a-f]{64})\Z")
MANIFEST_TYPES = {
    "application/vnd.docker.distribution.manifest.v2+json",
    "application/vnd.oci.image.manifest.v1+json",
}
ROLES = {
    "application/vnd.ollama.image.model": "weights",
    "application/vnd.ollama.image.projector": "projector",
    "application/vnd.ollama.image.license": "license",
    "application/vnd.ollama.image.template": "template",
    "application/vnd.ollama.image.params": "config",
    "application/vnd.ollama.image.system": "config",
}


def fail(code, reason):
    return DomainError(code, "No se pudo inspeccionar el modelo de Ollama", reason)


def _unique(pairs):
    result = {}
    for name, value in pairs:
        if name in result:
            raise ValueError("duplicate key")
        result[name] = value
    return result


def parse_json(raw):
    if len(raw) > MAX_JSON_BYTES:
        raise fail("RESOURCE_UNAVAILABLE", "ollama_metadata_size_limit")
    try:
        text = raw.decode("utf-8")
        depth, quoted, escaped = 0, False, False
        for character in text:
            if quoted:
                if escaped:
                    escaped = False
                elif character == "\\":
                    escaped = True
                elif character == '"':
                    quoted = False
            elif character == '"':
                quoted = True
            elif character in "[{":
                depth += 1
                if depth > MAX_JSON_DEPTH:
                    raise ValueError("metadata nesting limit")
            elif character in "]}":
                depth -= 1
        def invalid_constant(_value):
            raise ValueError("non-finite JSON")
        value = json.loads(text, object_pairs_hook=_unique, parse_constant=invalid_constant)
    except (ValueError, UnicodeError, RecursionError):
        raise fail("VALIDATION_FAILED", "ollama_metadata_json_invalid") from None
    if not isinstance(value, dict):
        raise fail("VALIDATION_FAILED", "ollama_metadata_object_required")
    for field in ("remote_model", "remote_host"):
        item = value.get(field, "")
        if not isinstance(item, str):
            raise fail("VALIDATION_FAILED", "ollama_remote_metadata_invalid")
        if item:
            raise fail("INCOMPATIBLE", "ollama_model_is_remote")
    return value


@contextmanager
def open_readonly(path, root):
    """On Windows, retain non-reparse ancestors and deny file write/delete.

    These handles pin the observed paths during hashing. They do not replace
    the host's filesystem grant or provide OS isolation for the Python worker.
    """
    try:
        path = path_allowed(Path(path), [Path(root)])
        with ExitStack() as stack:
            if os.name == "nt":
                import msvcrt
                kernel = ctypes.WinDLL("kernel32", use_last_error=True)
                kernel.CreateFileW.argtypes = [wintypes.LPCWSTR, wintypes.DWORD, wintypes.DWORD,
                                              wintypes.LPVOID, wintypes.DWORD, wintypes.DWORD, wintypes.HANDLE]
                kernel.CreateFileW.restype = wintypes.HANDLE
                kernel.CloseHandle.argtypes = [wintypes.HANDLE]
                kernel.CloseHandle.restype = wintypes.BOOL
                class Info(ctypes.Structure):
                    _fields_ = [("attributes", wintypes.DWORD), ("created", wintypes.FILETIME),
                                ("accessed", wintypes.FILETIME), ("written", wintypes.FILETIME),
                                ("volume", wintypes.DWORD), ("size_high", wintypes.DWORD),
                                ("size_low", wintypes.DWORD), ("links", wintypes.DWORD),
                                ("index_high", wintypes.DWORD), ("index_low", wintypes.DWORD)]
                kernel.GetFileInformationByHandle.argtypes = [wintypes.HANDLE, ctypes.POINTER(Info)]
                kernel.GetFileInformationByHandle.restype = wintypes.BOOL

                def acquire(candidate, directory):
                    # Ancestors deny delete/rename; the file also denies writes.
                    handle = kernel.CreateFileW(str(candidate), 0x80 if directory else 0x80000000,
                                                3 if directory else 1, None, 3, 0x02200000, None)
                    if handle == wintypes.HANDLE(-1).value:
                        raise OSError(ctypes.get_last_error(), "file pin unavailable")
                    info = Info()
                    if (not kernel.GetFileInformationByHandle(handle, ctypes.byref(info))
                            or info.attributes & 0x400 or bool(info.attributes & 0x10) != directory):
                        kernel.CloseHandle(handle)
                        raise fail("FORBIDDEN", "ollama_reparse_or_file_type_denied")
                    return handle

                for parent in reversed(path.parents):
                    stack.callback(kernel.CloseHandle, acquire(parent, True))
                handle = acquire(path, False)
                try:
                    fd = msvcrt.open_osfhandle(handle, os.O_RDONLY | os.O_BINARY)
                except Exception:
                    kernel.CloseHandle(handle)
                    raise
            else:
                fd = os.open(path, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0))
            stream = stack.enter_context(os.fdopen(fd, "rb"))
            if not stat.S_ISREG(os.fstat(stream.fileno()).st_mode):
                raise fail("FORBIDDEN", "ollama_regular_file_required")
            yield stream
    except FileNotFoundError:
        raise fail("MODEL_NOT_FOUND", "ollama_local_file_missing") from None
    except OSError:
        raise fail("RESOURCE_UNAVAILABLE", "ollama_local_file_unavailable") from None


class Store:
    def __init__(self, root):
        self.root = path_allowed(Path(root), [Path(root)])
        if not self.root.is_dir():
            raise fail("VALIDATION_FAILED", "ollama_store_root_invalid")

    def inspect(self, manifest_path, ctx, format_hint=None):
        ctx.check_cancelled()
        requested = Path(manifest_path)
        try:
            requested_relative = requested.relative_to(self.root)
        except ValueError:
            requested_relative = None
        if requested_relative is not None:
            # Reject ADS/device/traversal spellings before filesystem IO.
            relative(requested_relative.as_posix())
        try:
            path = path_allowed(requested, [self.root])
        except FileNotFoundError:
            raise fail("MODEL_NOT_FOUND", "ollama_local_file_missing") from None
        try:
            relative_manifest = path.relative_to(self.root / "manifests")
        except ValueError:
            raise fail("FORBIDDEN", "ollama_manifest_outside_store") from None
        if len(relative_manifest.parts) < 3 or not path.is_file():
            raise fail("VALIDATION_FAILED", "ollama_manifest_path_invalid")
        artifacts, seen, configs, weights_headers = [], {}, [], []
        observed_bytes = 0

        def read_file(file, role, expected_size=None, expected_hash=None, metadata=False):
            nonlocal observed_bytes
            relative_path = file.relative_to(self.root).as_posix()
            previous = seen.get(relative_path)
            if previous:
                if (previous["size_bytes"] != str(expected_size)
                        or previous["sha256"] != expected_hash or previous["role"] != role):
                    raise fail("INTEGRITY_FAILED", "ollama_descriptor_conflict")
                return previous, None
            if len(artifacts) >= MAX_FILES:
                raise fail("RESOURCE_UNAVAILABLE", "ollama_artifact_count_limit")
            hasher, raw, actual, header = hashlib.sha256(), bytearray(), 0, bytearray()
            with open_readonly(file, self.root) as stream:
                size = os.fstat(stream.fileno()).st_size
                if size > MAX_BYTES - observed_bytes or (expected_size is not None and size != expected_size):
                    raise fail("INTEGRITY_FAILED", "ollama_blob_size_mismatch")
                if metadata and size > MAX_JSON_BYTES:
                    raise fail("RESOURCE_UNAVAILABLE", "ollama_metadata_size_limit")
                while block := stream.read(CHUNK):
                    ctx.check_cancelled()
                    actual += len(block)
                    if actual > size:
                        raise fail("INTEGRITY_FAILED", "ollama_blob_changed")
                    hasher.update(block)
                    if role == "weights" and len(header) < 24:
                        header.extend(block[:24-len(header)])
                    if metadata:
                        raw.extend(block)
                checksum = hasher.hexdigest()
                if actual != size or (expected_hash is not None and checksum != expected_hash):
                    raise fail("INTEGRITY_FAILED", "ollama_blob_hash_mismatch")
            observed_bytes += actual
            if role == "weights":
                weights_headers.append(bytes(header))
            artifact = {"relative_path":relative_path, "size_bytes":str(actual), "sha256":checksum,
                        "required":True, "role":role}
            seen[relative_path] = artifact
            artifacts.append(artifact)
            ctx.progress("INSPECT", observed_bytes, None, "ollama_local_inspection")
            return artifact, bytes(raw) if metadata else None

        def descriptor(value, role=None):
            if not isinstance(value, dict):
                raise fail("VALIDATION_FAILED", "ollama_descriptor_invalid")
            size, digest, media = value.get("size"), value.get("digest"), value.get("mediaType")
            match = DIGEST.fullmatch(digest) if isinstance(digest, str) else None
            if (not match or type(size) is not int or not 0 <= size <= MAX_BYTES
                    or not isinstance(media, str) or not media):
                raise fail("VALIDATION_FAILED", "ollama_descriptor_invalid")
            checksum = match.group(1)
            file = self.root / "blobs" / ("sha256-" + checksum)
            artifact, raw = read_file(file, role or ("manifest" if media in MANIFEST_TYPES else ROLES.get(media, "other")),
                                      size, checksum, metadata=role == "config" or media in MANIFEST_TYPES)
            return artifact, raw, media

        def visit(raw, depth):
            if depth > MAX_DEPTH:
                raise fail("RESOURCE_UNAVAILABLE", "ollama_manifest_depth_limit")
            value = parse_json(raw)
            if type(value.get("schemaVersion")) is not int or value["schemaVersion"] != 2 or value.get("mediaType") not in MANIFEST_TYPES:
                raise fail("VALIDATION_FAILED", "ollama_manifest_schema_unsupported")
            layers = value.get("layers")
            if not isinstance(layers, list) or len(layers) > MAX_FILES:
                raise fail("VALIDATION_FAILED", "ollama_manifest_layers_invalid")
            config = value.get("config")
            if not isinstance(config, dict) or config.get("mediaType") not in {
                "application/vnd.docker.container.image.v1+json", "application/vnd.oci.image.config.v1+json"
            }:
                raise fail("VALIDATION_FAILED", "ollama_config_media_type_invalid")
            _, config_raw, _ = descriptor(config, "config")
            if config_raw is not None:
                parsed = parse_json(config_raw)
                # Retain only relevant scalar metadata, not arbitrarily large
                # additive vendor objects across hundreds of descriptors.
                configs.append({key:parsed.get(key) for key in ("model_format","file_type")
                                if isinstance(parsed.get(key), str)})
            for layer in layers:
                _, nested, media = descriptor(layer)
                if media in MANIFEST_TYPES and nested is not None:
                    visit(nested, depth + 1)

        root_artifact, root_raw = read_file(path, "manifest", metadata=True)
        visit(root_raw, 0)
        if not any(artifact["role"] == "weights" for artifact in artifacts):
            raise fail("INCOMPATIBLE", "ollama_model_weights_missing")
        formats = {config.get("model_format") for config in configs if isinstance(config.get("model_format"), str)}
        format_value = "GGUF" if formats == {"gguf"} else None
        if format_value == "GGUF" and not all(len(header) == 24 and header[:4] == b"GGUF"
                and int.from_bytes(header[4:8], "little") in (2, 3) for header in weights_headers):
            raise fail("INTEGRITY_FAILED", "ollama_model_format_mismatch")
        if format_hint is not None and format_hint != format_value:
            raise fail("INCOMPATIBLE", "ollama_format_hint_mismatch")
        quantizations = {config.get("file_type") for config in configs if isinstance(config.get("file_type"), str) and config["file_type"]}
        return {"upstream_identity":"ollama:" + relative_manifest.as_posix() + "@sha256:" + root_artifact["sha256"],
                "architecture":None, "format":format_value,
                "quantization":next(iter(quantizations)) if len(quantizations) == 1 else None,
                "capabilities":[], "limits":{}, "artifacts":sorted(artifacts,key=lambda item:item["relative_path"]),
                "license":None, "evidence_refs":["ollama:local-manifest-sha256:" + root_artifact["sha256"],
                    "ollama:physical-blobs-sha256-checked", "ollama:capabilities-not-probed", "ollama:license-not-accepted"]}
