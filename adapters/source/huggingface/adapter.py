"""Read-only Hugging Face Hub integration for the LARM source protocol.

The worker never writes model bytes to a storage location.  Public model data
is fetched from the Hub API, while source.download_chunk returns bounded bytes
to the Download Manager.  Authentication is deliberately fail-closed until
the P045/P141 credential resolver is qualified.
"""

from __future__ import annotations

import base64
import hashlib
import json
import os
import re
import sqlite3
import threading
import urllib.error
import urllib.parse
import urllib.request
import uuid
from datetime import datetime, timedelta, timezone
from pathlib import Path, PurePosixPath

import rfc8785
from larm_adapter_sdk import ContractViolation, DomainError, RpcError
from larm_adapter_sdk.access import NoRedirect, fail, path_allowed
from larm_adapter_sdk.source_acquisition import validate_source_acquisition, select_source_acquisition, source_acquisition_payload


HTTP_TIMEOUT_SECONDS = 8
MAX_JSON_BYTES = 8 << 20
MAX_REVISIONS = 64
MAX_REDIRECTS = 5
MAX_SQLITE_BYTES = (1 << 63) - 1
MAX_CANONICAL_JSON_INTEGER = (1 << 53) - 1
CHUNK_MAX_BYTES = 512 << 10

_CAPABILITIES = {
    "text-generation": "text.generate",
    "text2text-generation": "text.generate",
    "text-classification": "text.classify",
    "zero-shot-classification": "text.classify",
    "feature-extraction": "embedding.create",
    "sentence-similarity": "embedding.create",
    "text-ranking": "rerank",
    "image-classification": "vision.understand",
    "image-to-text": "vision.understand",
    "image-text-to-text": "vision.understand",
    "object-detection": "vision.detect",
    "image-segmentation": "vision.segment",
    "text-to-image": "image.generate",
    "text-to-video": "video.generate",
    "image-to-video": "video.generate",
    "automatic-speech-recognition": "audio.transcribe",
    "audio-to-audio": "audio.classify",
    "audio-classification": "audio.classify",
    "text-to-speech": "speech.synthesize",
    "text-to-audio": "speech.synthesize",
}
_QUANTIZATION = re.compile(
    r"(?<![A-Z0-9])((?:Q[2-8]_[KM]_[SML])|(?:IQ|Q)[2-8](?:_[A-Z0-9]+)?|"
    r"(?:F|BF|FP|INT|UINT)(?:2|4|8|16|32|64))(?![A-Z0-9])",
    re.IGNORECASE,
)
_SHA256 = re.compile(r"^[0-9a-f]{64}$")
_COMMIT = re.compile(r"^[0-9a-f]{40,64}$")
_ETAG = re.compile(r'^"[!#-~]*"$')
_CONTENT_RANGE = re.compile(r"^bytes (\d+)-(\d+)/(\d+|\*)$")
_RATE_LIMIT = re.compile(r'"([^";]+)"\s*;\s*r\s*=\s*(\d+)\s*;\s*t\s*=\s*(\d+)', re.I)
_SENSITIVE_KEYS = {
    "token", "access_token", "refresh_token", "api_token", "api_key", "apikey",
    "password", "passwd", "secret", "authorization", "credential", "credentials",
    "cookie", "cookies", "signature", "x-amz-signature", "x-goog-signature",
}
_SECRET_VALUE = re.compile(
    r"(?:Bearer\s+|hf_[A-Za-z0-9]{12,}|github_pat_[A-Za-z0-9_]{12,}|"
    r"gh[pousr]_[A-Za-z0-9]{12,})",
    re.I,
)


def _now() -> str:
    return datetime.now(timezone.utc).isoformat(timespec="milliseconds").replace("+00:00", "Z")


def _stamp(value: object, fallback: str | None = None) -> str:
    if not isinstance(value, str) or not value:
        return fallback or _now()
    try:
        parsed = datetime.fromisoformat(value.replace("Z", "+00:00"))
        if parsed.tzinfo is None:
            return fallback or _now()
        return parsed.astimezone(timezone.utc).isoformat(timespec="milliseconds").replace("+00:00", "Z")
    except ValueError:
        return fallback or _now()


def _unique_pairs(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError("duplicate JSON key")
        result[key] = value
    return result


def _reject_constant(_value):
    raise ValueError("non-finite JSON number")


def _redact(value):
    """Keep upstream metadata open while removing secret-bearing values."""
    if isinstance(value, dict):
        result = {}
        for key, item in value.items():
            key_text = str(key)
            lower = key_text.lower()
            sensitive = lower in _SENSITIVE_KEYS or lower.endswith(("_token", "_password", "_secret"))
            result[key_text] = "[REDACTED]" if sensitive else _redact(item)
        return result
    if isinstance(value, list):
        return [_redact(item) for item in value]
    if isinstance(value, str):
        if len(value) > 65536 or _SECRET_VALUE.search(value):
            return "[REDACTED]"
        try:
            parsed = urllib.parse.urlsplit(value)
        except ValueError:
            return "[REDACTED]"
        if parsed.scheme.lower() in ("http", "https") and parsed.query:
            for key, _query_value in urllib.parse.parse_qsl(parsed.query, keep_blank_values=True):
                lowered = key.lower()
                if lowered in _SENSITIVE_KEYS or any(part in lowered for part in ("token", "signature", "secret", "credential", "password", "cookie", "authorization")):
                    return "[REDACTED]"
        return value
    return value


def _json_bytes(value) -> bytes:
    """Canonicalize JSON exactly as required by the LARM contracts."""
    def normalize(item):
        if item is None or isinstance(item, (str, bool, float)):
            return item
        if isinstance(item, int):
            if abs(item) > 1 << 53:
                raise ValueError("integer outside RFC 8785 range")
            return float(item) if abs(item) == 1 << 53 else item
        if isinstance(item, list):
            return [normalize(child) for child in item]
        if isinstance(item, dict) and all(isinstance(key, str) for key in item):
            return {key: normalize(child) for key, child in item.items()}
        raise ValueError("value is not canonical JSON")

    return rfc8785.dumps(normalize(value))


def _digest(value) -> str:
    return hashlib.sha256(_json_bytes(value)).hexdigest()


def _decimal(value, *, maximum=MAX_SQLITE_BYTES):
    if isinstance(value, bool) or not isinstance(value, int) or not 0 <= value <= maximum:
        return None
    return str(value)


def _safe_origin(url: str) -> str:
    try:
        parsed = urllib.parse.urlsplit(url)
        port = parsed.port
    except (TypeError, ValueError):
        raise fail("FORBIDDEN", "hf_origin_invalid") from None
    if (
        parsed.scheme not in ("https", "http")
        or not parsed.hostname
        or parsed.username is not None
        or parsed.password is not None
        or parsed.fragment
    ):
        raise fail("FORBIDDEN", "hf_origin_invalid") from None
    host = parsed.hostname.lower()
    effective_port = port or (443 if parsed.scheme == "https" else 80)
    if ":" in host:
        host = f"[{host}]"
    return f"{parsed.scheme}://{host}:{effective_port}"


def _profile_origin(url: str) -> str:
    """Strict origin parser for configured API endpoints (no signed URL data)."""
    parsed = urllib.parse.urlsplit(url)
    if parsed.query or parsed.fragment:
        raise fail("VALIDATION_FAILED", "hf_base_url_query_forbidden")
    origin = _safe_origin(url)
    if parsed.path not in ("", "/"):
        raise fail("VALIDATION_FAILED", "hf_base_url_path_forbidden")
    if parsed.scheme == "http":
        host = (parsed.hostname or "").lower()
        if host not in ("localhost", "127.0.0.1", "::1"):
            raise fail("FORBIDDEN", "hf_http_non_loopback_forbidden")
    return origin


def _normal_relative(path: str) -> str:
    if (
        not isinstance(path, str) or not path or len(path) > 4096
        or path.startswith("/") or "\\" in path or "\x00" in path
    ):
        raise fail("VALIDATION_FAILED", "hf_relative_path_invalid")
    parts = path.split("/")
    reserved = {"con", "prn", "aux", "nul", "conin$", "conout$"}
    reserved |= {f"{prefix}{n}" for prefix in ("com", "lpt") for n in range(1, 10)}
    for part in parts:
        if (
            not part or part in (".", "..") or part.endswith((".", " "))
            or any(char in part for char in '<>:"|?*')
            or any(ord(char) < 32 for char in part)
            or any(0xD800 <= ord(char) <= 0xDFFF for char in part)
            or part.split(".")[0].lower() in reserved
        ):
            raise fail("VALIDATION_FAILED", "hf_relative_path_invalid")
    return path


def _repo_id(namespace: str, model_id: str) -> str:
    if not isinstance(namespace, str) or not isinstance(model_id, str):
        raise fail("VALIDATION_FAILED", "hf_model_identity_invalid")
    if model_id.startswith(namespace + "/"):
        repo = model_id
    elif "/" not in model_id:
        repo = f"{namespace}/{model_id}"
    else:
        repo = model_id
    bits = repo.split("/")
    if len(bits) != 2 or any(not part or part in (".", "..") for part in bits):
        raise fail("VALIDATION_FAILED", "hf_model_identity_invalid")
    if any(any(ord(ch) < 32 for ch in part) for part in bits):
        raise fail("VALIDATION_FAILED", "hf_model_identity_invalid")
    return repo


def _revision_parts(revision_key: str):
    if not isinstance(revision_key, str) or not revision_key.startswith("commit:"):
        raise fail("STALE_REVISION", "hf_revision_not_pinned")
    commit, separator, encoded_bundle = revision_key[7:].partition("#bundle=")
    if not _COMMIT.fullmatch(commit):
        raise fail("STALE_REVISION", "hf_revision_not_pinned")
    if not separator:
        return commit, None
    try:
        bundle = base64.urlsafe_b64decode(encoded_bundle + "=" * (-len(encoded_bundle) % 4)).decode("utf-8")
    except (ValueError, UnicodeError):
        raise fail("VALIDATION_FAILED", "hf_revision_bundle_invalid") from None
    if not bundle or len(bundle) > 4096:
        raise fail("VALIDATION_FAILED", "hf_revision_bundle_invalid")
    return commit, bundle


def _revision_key(commit: str, bundle_key: str) -> str:
    encoded = base64.urlsafe_b64encode(bundle_key.encode("utf-8")).decode("ascii").rstrip("=")
    return f"commit:{commit}#bundle={encoded}"


def _is_gated(value: object) -> bool:
    return value is True or value in ("auto", "manual")


def _file_sha(sibling: dict) -> str | None:
    lfs = sibling.get("lfs")
    candidates = []
    if isinstance(lfs, dict):
        candidates.extend((lfs.get("sha256"), lfs.get("oid")))
    candidates.extend((sibling.get("sha256"), sibling.get("oid")))
    for candidate in candidates:
        if isinstance(candidate, str):
            value = candidate.removeprefix("sha256:").lower()
            if _SHA256.fullmatch(value):
                return value
    # `blob_id` is commonly a 40-character Git SHA-1, never a SHA-256 claim.
    return None


def _file_size(sibling: dict) -> int | None:
    lfs = sibling.get("lfs")
    candidates = [sibling.get("size")]
    if isinstance(lfs, dict):
        candidates.append(lfs.get("size"))
    for candidate in candidates:
        if isinstance(candidate, int) and not isinstance(candidate, bool) and 0 <= candidate <= MAX_CANONICAL_JSON_INTEGER:
            return candidate
    return None


def _file_metadata(sibling: dict) -> dict:
    path = sibling.get("rfilename", sibling.get("path"))
    if not isinstance(path, str):
        raise fail("PROTOCOL_ERROR", "hf_file_name_missing")
    path = _normal_relative(path)
    return {
        "relative_path": path,
        "size_bytes": _file_size(sibling),
        "sha256": _file_sha(sibling),
        "raw": _redact(sibling),
    }


def _role(path: str) -> str:
    lower = path.lower()
    name = lower.rsplit("/", 1)[-1]
    if "mmproj" in name:
        return "mmproj"
    if name.endswith((".safetensors", ".gguf", ".onnx", ".bin", ".pt", ".pth", ".model")):
        return "weights"
    if name.endswith(".onnx_data") or name.endswith(".onnx.data") or name.endswith(".data"):
        return "external_data"
    if "tokenizer" in name or name in {"vocab.json", "merges.txt", "spiece.model", "sentencepiece.bpe.model"}:
        return "tokenizer"
    if "processor" in name or "preprocessor" in name:
        return "processor"
    if name in {"config.json", "generation_config.json", "model.safetensors.index.json", "pytorch_model.bin.index.json"}:
        return "config"
    return "other"


def _bundle_groups(files: list[dict]) -> list[dict]:
    """Conservative bundle candidates from filenames; unknown files stay unclaimed."""
    normalized = []
    paths = set()
    for sibling in files:
        item = _file_metadata(sibling) if "relative_path" not in sibling else sibling
        path = _normal_relative(item["relative_path"])
        key = path.casefold()
        if key in paths:
            raise fail("PROTOCOL_ERROR", "hf_duplicate_file_path")
        paths.add(key)
        normalized.append(item | {"relative_path": path})
    by_path = {item["relative_path"].casefold(): item for item in normalized}
    result = []

    # GGUF quantizations are separate variants. A single model/projector pair
    # is grouped only when the repository has exactly one non-projector GGUF.
    ggufs = [f for f in normalized if f["relative_path"].lower().endswith(".gguf")]
    main_ggufs = [f for f in ggufs if _role(f["relative_path"]) != "mmproj"]
    projectors = [f for f in ggufs if _role(f["relative_path"]) == "mmproj"]
    for model in main_ggufs:
        members = [model]
        if len(main_ggufs) == 1 and len(projectors) == 1:
            members.append(projectors[0])
        result.append({"key": "gguf:" + model["relative_path"], "format": "GGUF", "files": members})

    # Keep each weight directory as its own candidate; unrelated model folders
    # in a monorepo must never be merged into one downloadable bundle.
    safe_weights = [f for f in normalized if f["relative_path"].lower().endswith(".safetensors")]
    transformer_include_names = {
        "config.json", "generation_config.json", "model.safetensors.index.json",
        "tokenizer.json", "tokenizer_config.json", "special_tokens_map.json",
        "added_tokens.json", "vocab.json", "merges.txt", "spiece.model",
        "sentencepiece.bpe.model", "processor_config.json", "preprocessor_config.json",
        "chat_template.jinja",
    }
    safe_dirs = {str(PurePosixPath(f["relative_path"]).parent).casefold() for f in safe_weights}
    if safe_weights:
        for directory in sorted(safe_dirs):
            members = [f for f in safe_weights if str(PurePosixPath(f["relative_path"]).parent).casefold() == directory]
            for item in normalized:
                parent = str(PurePosixPath(item["relative_path"]).parent).casefold()
                basename = PurePosixPath(item["relative_path"]).name.lower()
                if parent == directory and basename in transformer_include_names and item not in members:
                    members.append(item)
            key = "transformers:safetensors" if len(safe_dirs) == 1 else "transformers:safetensors:" + directory
            result.append({"key": key, "format": "SAFETENSORS", "files": members})

    # ONNX external-data sidecars are included only when their names bind
    # unambiguously to one graph.
    onnx_files = [f for f in normalized if f["relative_path"].lower().endswith(".onnx")]
    for graph in onnx_files:
        path = graph["relative_path"]
        stem = path[:-5]
        members = [graph]
        sidecars = (stem + ".onnx_data", stem + ".onnx.data", stem + ".data")
        for sidecar in sidecars:
            found = by_path.get(sidecar.casefold())
            if found is not None and found not in members:
                members.append(found)
        result.append({"key": "onnx:" + path, "format": "ONNX", "files": members})

    # PyTorch checkpoints are a recognizable format, but do not claim that
    # arbitrary repository files form a Transformers bundle.
    binaries = [f for f in normalized if f["relative_path"].lower().endswith((".bin", ".pt", ".pth"))]
    binary_dirs = {str(PurePosixPath(f["relative_path"]).parent).casefold() for f in binaries} - safe_dirs
    pytorch_include_names = transformer_include_names | {"pytorch_model.bin.index.json"}
    for directory in sorted(binary_dirs):
        members = [f for f in binaries if str(PurePosixPath(f["relative_path"]).parent).casefold() == directory]
        for item in normalized:
            parent = str(PurePosixPath(item["relative_path"]).parent).casefold()
            basename = PurePosixPath(item["relative_path"]).name.lower()
            if parent == directory and basename in pytorch_include_names and item not in members:
                members.append(item)
        key = "transformers:pytorch" if len(binary_dirs) == 1 else "transformers:pytorch:" + directory
        result.append({"key": key, "format": "PYTORCH", "files": members})

    # Identical files can belong to different format bundles; each output has
    # its own required set. Keep order deterministic for stable hashes.
    for group in result:
        group["files"] = sorted(group["files"], key=lambda f: f["relative_path"].casefold())
    return sorted(result, key=lambda group: (group["format"], group["key"].casefold()))


def _quantization(files: list[dict]) -> str | None:
    for item in files:
        match = _QUANTIZATION.search(PurePosixPath(item["relative_path"]).stem)
        if match:
            return match.group(1).upper()
    return None


def _capabilities(info: dict) -> list[str]:
    tag = info.get("pipeline_tag")
    if isinstance(tag, str) and tag in _CAPABILITIES:
        return [_CAPABILITIES[tag]]
    return []


def _license(info: dict) -> str | None:
    card = info.get("cardData")
    value = card.get("license") if isinstance(card, dict) else None
    if not isinstance(value, str) or not value:
        value = info.get("license")
    if isinstance(value, str) and value:
        return value[:256]
    tags = info.get("tags")
    if isinstance(tags, list):
        for tag in tags:
            if isinstance(tag, str) and tag.lower().startswith("license:") and tag[8:]:
                return tag[8:][:256]
    return None


def _model_limits(info: dict) -> dict:
    config = info.get("config") if isinstance(info.get("config"), dict) else {}
    safe = info.get("safetensors") if isinstance(info.get("safetensors"), dict) else {}
    parameters = safe.get("total")
    if not isinstance(parameters, int) or isinstance(parameters, bool) or not 0 <= parameters <= MAX_SQLITE_BYTES:
        parameters = None
    if parameters is None:
        config_count = config.get("num_parameters")
        if isinstance(config_count, int) and not isinstance(config_count, bool) and 0 <= config_count <= MAX_SQLITE_BYTES:
            parameters = config_count
    def integer(*keys):
        for key in keys:
            value = config.get(key)
            if isinstance(value, int) and not isinstance(value, bool) and 0 <= value <= MAX_SQLITE_BYTES:
                return value
        return None
    config_for_hash = _redact(config)
    try:
        config_hash = _digest(config_for_hash) if config_for_hash else None
    except ValueError:
        config_hash = None
    result = {
        "parameter_count": _decimal(parameters),
        "layers": integer("num_hidden_layers", "n_layer", "num_layers"),
        "kv_heads": integer("num_key_value_heads", "num_kv_heads"),
        "head_dim": integer("head_dim", "kv_channels"),
        "context_limit": integer("max_position_embeddings", "n_positions", "max_seq_len", "seq_length"),
        "kv_bits": None,
        "model_max_memory_bytes": None,
        "architecture_metadata_hash": config_hash,
    }
    return result


def _artifact_size(item: dict, evidence: str, source_override: str | None = None) -> dict:
    size, sha = item.get("size_bytes"), item.get("sha256")
    if size is not None and sha is not None:
        return {
            "confidence": "EXACT", "bytes": str(size), "lower_bound_bytes": str(size),
            "upper_bound_bytes": str(size), "source": "MANIFEST", "evidence_refs": [evidence],
            "reason": "Tamaño y SHA-256 publicados para la revisión fijada.",
        }
    if size is not None:
        raw = item.get("raw") if isinstance(item.get("raw"), dict) else {}
        lfs = raw.get("lfs") if isinstance(raw.get("lfs"), dict) else {}
        source = source_override or ("GIT_LFS_METADATA" if lfs.get("size") is not None else "SOURCE_METADATA")
        reason = "Content-Length comprobado en la descarga HEAD de la revisión fijada; no se anunció SHA-256." if source == "HTTP_HEAD" else "Tamaño publicado por la API de archivos de la revisión fijada; no se anunció SHA-256."
        return {
            "confidence": "HIGH", "bytes": str(size), "lower_bound_bytes": None,
            "upper_bound_bytes": str(size), "source": source, "evidence_refs": [evidence],
            "reason": reason,
        }
    return {
        "confidence": "UNKNOWN", "bytes": None, "lower_bound_bytes": None,
        "upper_bound_bytes": None, "source": "UNKNOWN", "evidence_refs": [evidence],
        "reason": "La fuente no publica un tamaño verificable para este archivo.",
    }


def _descriptor(item: dict, *, required: bool, evidence: str, include_estimate: bool) -> dict:
    result = {
        "relative_path": item["relative_path"],
        "size_bytes": _decimal(item.get("size_bytes")),
        "sha256": item.get("sha256"),
        "required": bool(required),
        "role": _role(item["relative_path"]),
    }
    if include_estimate:
        result["size_estimate"] = _artifact_size(item, evidence)
    return result


class IdentityStore:
    """Stable UUIDv7 bindings owned by this worker; never stores credentials."""

    def __init__(self, path: Path):
        self.path = path
        self.lock = threading.RLock()
        self.connection: sqlite3.Connection | None = None

    def open(self):
        with self.lock:
            if self.connection is not None:
                return
            db = sqlite3.connect(self.path, timeout=5, isolation_level=None, check_same_thread=False)
            db.execute("PRAGMA journal_mode=WAL")
            db.execute("PRAGMA synchronous=FULL")
            db.execute("PRAGMA foreign_keys=ON")
            db.executescript(
                """
                CREATE TABLE IF NOT EXISTS distributions(
                    repo_id TEXT PRIMARY KEY,
                    distribution_id TEXT NOT NULL UNIQUE,
                    created_at TEXT NOT NULL,
                    is_private INTEGER NOT NULL DEFAULT 0,
                    gated TEXT
                );
                CREATE TABLE IF NOT EXISTS variants(
                    repo_id TEXT NOT NULL,
                    bundle_key TEXT NOT NULL,
                    variant_id TEXT NOT NULL UNIQUE,
                    created_at TEXT NOT NULL,
                    PRIMARY KEY(repo_id,bundle_key),
                    FOREIGN KEY(repo_id) REFERENCES distributions(repo_id)
                );
                CREATE TABLE IF NOT EXISTS revisions(
                    repo_id TEXT NOT NULL,
                    bundle_key TEXT NOT NULL,
                    commit_hash TEXT NOT NULL,
                    revision_id TEXT NOT NULL UNIQUE,
                    created_at TEXT NOT NULL,
                    manifest_hash TEXT NOT NULL,
                    PRIMARY KEY(repo_id,bundle_key,commit_hash),
                    FOREIGN KEY(repo_id,bundle_key) REFERENCES variants(repo_id,bundle_key)
                );
                """
            )
            self.connection = db

    def close(self):
        with self.lock:
            if self.connection is not None:
                self.connection.close()
                self.connection = None

    def _db(self):
        if self.connection is None:
            raise fail("RESOURCE_UNAVAILABLE", "hf_state_not_open")
        return self.connection

    def distribution(self, repo_id: str, *, private: bool = False, gated=None) -> tuple[str, str]:
        with self.lock:
            db = self._db()
            row = db.execute("SELECT distribution_id,created_at FROM distributions WHERE repo_id=?", (repo_id,)).fetchone()
            if row is None:
                now, identity = _now(), str(uuid.uuid7())
                db.execute("INSERT INTO distributions(repo_id,distribution_id,created_at,is_private,gated) VALUES(?,?,?,?,?)", (repo_id, identity, now, int(private), json.dumps(gated)))
                return identity, now
            db.execute("UPDATE distributions SET is_private=?,gated=? WHERE repo_id=?", (int(private), json.dumps(gated), repo_id))
            return row[0], row[1]

    def flags(self, repo_id: str) -> tuple[bool, object]:
        with self.lock:
            row = self._db().execute("SELECT is_private,gated FROM distributions WHERE repo_id=?", (repo_id,)).fetchone()
            if row is None:
                return False, None
            try:
                gated = json.loads(row[1]) if row[1] is not None else None
            except (TypeError, ValueError):
                gated = None
            return bool(row[0]), gated

    def variant(self, repo_id: str, bundle_key: str) -> tuple[str, str]:
        with self.lock:
            db = self._db()
            row = db.execute("SELECT variant_id,created_at FROM variants WHERE repo_id=? AND bundle_key=?", (repo_id, bundle_key)).fetchone()
            if row is None:
                now, identity = _now(), str(uuid.uuid7())
                db.execute("INSERT INTO variants(repo_id,bundle_key,variant_id,created_at) VALUES(?,?,?,?)", (repo_id, bundle_key, identity, now))
                return identity, now
            return row[0], row[1]

    def revision(self, repo_id: str, bundle_key: str, commit: str, manifest_hash: str) -> tuple[str, str]:
        with self.lock:
            db = self._db()
            row = db.execute("SELECT revision_id,created_at,manifest_hash FROM revisions WHERE repo_id=? AND bundle_key=? AND commit_hash=?", (repo_id, bundle_key, commit)).fetchone()
            if row is None:
                now, identity = _now(), str(uuid.uuid7())
                db.execute("INSERT INTO revisions(repo_id,bundle_key,commit_hash,revision_id,created_at,manifest_hash) VALUES(?,?,?,?,?,?)", (repo_id, bundle_key, commit, identity, now, manifest_hash))
                return identity, now
            if row[2] != manifest_hash:
                raise fail("INTEGRITY_FAILED", "hf_pinned_revision_manifest_changed")
            return row[0], row[1]

    def variant_context(self, variant_id: str):
        with self.lock:
            row = self._db().execute("SELECT repo_id,bundle_key FROM variants WHERE variant_id=?", (variant_id,)).fetchone()
            return None if row is None else (row[0], row[1])


class HubHttp:
    """Bounded HTTPS client: no implicit proxies or redirects."""

    def __init__(self, base_url: str, allowed_origins: set[str]):
        self.base_url = base_url.rstrip("/")
        self.base_origin = _profile_origin(base_url)
        self.allowed_origins = allowed_origins
        if self.base_origin not in allowed_origins:
            raise fail("FORBIDDEN", "hf_api_origin_not_permitted")
        self.opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), NoRedirect())

    def _check_url(self, url: str, *, allow_sensitive_query=False):
        origin = _safe_origin(url)
        parsed = urllib.parse.urlsplit(url)
        if origin not in self.allowed_origins:
            raise fail("FORBIDDEN", "hf_network_origin_not_permitted")
        if parsed.scheme != "https" and not (origin.startswith("http://127.0.0.1:") or origin.startswith("http://localhost:") or origin.startswith("http://[::1]:")):
            raise fail("FORBIDDEN", "hf_https_required")
        if not allow_sensitive_query:
            for key, _ in urllib.parse.parse_qsl(parsed.query, keep_blank_values=True):
                lowered = key.lower()
                if lowered in _SENSITIVE_KEYS or any(value in lowered for value in ("token", "signature", "secret", "credential", "password")):
                    raise fail("FORBIDDEN", "hf_credentials_in_api_url")
        return origin

    def request(self, url: str, *, method="GET", headers=None, ctx=None, body_limit=MAX_JSON_BYTES):
        current = url
        current_headers = dict(headers or {})
        for redirect_count in range(MAX_REDIRECTS + 1):
            self._check_url(current, allow_sensitive_query=(current != url))
            if ctx is not None:
                ctx.check_cancelled()
            request = urllib.request.Request(current, headers=current_headers, method=method)
            try:
                response = self.opener.open(request, timeout=HTTP_TIMEOUT_SECONDS)
            except urllib.error.HTTPError as error:
                response = error
            except (TimeoutError, OSError, urllib.error.URLError, ValueError):
                raise self._upstream_error("upstream_network_interrupted", 0, {}) from None
            status = getattr(response, "status", response.code)
            if 300 <= status < 400:
                location = response.headers.get("Location")
                response.close()
                if not location or redirect_count >= MAX_REDIRECTS:
                    raise fail("PROTOCOL_ERROR", "hf_redirect_invalid")
                target = urllib.parse.urljoin(current, location)
                old_origin = _safe_origin(current)
                new_origin = self._check_url(target, allow_sensitive_query=True)
                if new_origin != old_origin:
                    # No Authorization, cookies or other credentials ever cross origins.
                    current_headers = {
                        key: value for key, value in current_headers.items()
                        if key.lower() in {"accept", "accept-encoding", "range", "if-range"}
                    }
                current = target
                continue
            if status >= 400:
                headers_copy = dict(response.headers.items())
                response.close()
                raise self._status_error(status, headers_copy)
            try:
                length = response.headers.get("Content-Length")
                if method.upper() != "HEAD" and length is not None and (not length.isdecimal() or int(length) > body_limit):
                    raise fail("PROTOCOL_ERROR", "hf_response_size_invalid")
                payload = response.read(body_limit + 1)
                if len(payload) > body_limit:
                    raise fail("PROTOCOL_ERROR", "hf_response_too_large")
                return status, dict(response.headers.items()), payload
            except (TimeoutError, OSError, urllib.error.URLError):
                raise self._upstream_error("upstream_network_interrupted", 0, {}) from None
            finally:
                response.close()
        raise fail("PROTOCOL_ERROR", "hf_redirect_limit")

    def request_chunk(self, url: str, *, headers, maximum: int, ctx=None):
        current = url
        current_headers = dict(headers)
        for redirect_count in range(MAX_REDIRECTS + 1):
            self._check_url(current, allow_sensitive_query=(current != url))
            if ctx is not None:
                ctx.check_cancelled()
            request = urllib.request.Request(current, headers=current_headers, method="GET")
            try:
                response = self.opener.open(request, timeout=HTTP_TIMEOUT_SECONDS)
            except urllib.error.HTTPError as error:
                response = error
            except (TimeoutError, OSError, urllib.error.URLError, ValueError):
                raise self._upstream_error("upstream_network_interrupted", 0, {}) from None
            status = getattr(response, "status", response.code)
            if 300 <= status < 400:
                location = response.headers.get("Location")
                response.close()
                if not location or redirect_count >= MAX_REDIRECTS:
                    raise fail("PROTOCOL_ERROR", "hf_redirect_invalid")
                target = urllib.parse.urljoin(current, location)
                old_origin = _safe_origin(current)
                new_origin = self._check_url(target, allow_sensitive_query=True)
                if new_origin != old_origin:
                    current_headers = {
                        key: value for key, value in current_headers.items()
                        if key.lower() in {"accept", "accept-encoding", "range", "if-range"}
                    }
                current = target
                continue
            if status >= 400 and status != 416:
                headers_copy = dict(response.headers.items())
                response.close()
                raise self._status_error(status, headers_copy)
            try:
                if status == 416:
                    return status, dict(response.headers.items()), b""
                data = response.read(maximum + 1)
                if len(data) > maximum:
                    raise fail("PROTOCOL_ERROR", "hf_chunk_exceeds_limit")
                return status, dict(response.headers.items()), data
            except (TimeoutError, OSError, urllib.error.URLError):
                raise self._upstream_error("upstream_network_interrupted", 0, {}) from None
            finally:
                response.close()
        raise fail("PROTOCOL_ERROR", "hf_redirect_limit")

    @staticmethod
    def _upstream_error(reason, status, headers):
        error = fail("UPSTREAM_UNAVAILABLE", reason, "No se pudo consultar Hugging Face", retryable=True)
        error.data["details"]["original_error"] = {
            "system": "huggingface", "code": str(status),
            "message": json.dumps(_rate_limit(headers), separators=(",", ":"), ensure_ascii=False),
        }
        return error

    @staticmethod
    def _status_error(status, headers):
        lower = {str(key).lower(): str(value) for key, value in headers.items()}
        hf_code = lower.get("x-error-code", "")
        if status == 401:
            code, reason, retryable = "AUTH_REQUIRED", "hf_auth_required", False
        elif status == 403 and hf_code.lower() == "gatedrepo":
            code, reason, retryable = "LICENSE_REQUIRED", "hf_gated_access_required", False
        elif status == 403:
            code, reason, retryable = "AUTH_REQUIRED", "hf_access_denied", False
        elif status == 404:
            code, reason, retryable = "NOT_FOUND", "hf_repository_or_file_not_found", False
        elif status == 429:
            code, reason, retryable = "UPSTREAM_UNAVAILABLE", "source_rate_limited", True
        elif status == 408 or 500 <= status <= 599:
            code, reason, retryable = "UPSTREAM_UNAVAILABLE", "hf_upstream_unavailable", True
        elif status == 400:
            code, reason, retryable = "VALIDATION_FAILED", "hf_request_rejected", False
        else:
            code, reason, retryable = "UPSTREAM_UNAVAILABLE", "hf_http_error", status >= 500
        message = json.dumps(_rate_limit(headers), separators=(",", ":"), ensure_ascii=False)
        error = fail(code, reason, "Hugging Face rechazó la solicitud", retryable=retryable)
        error.data["details"]["original_error"] = {
            "system": "source", "code": str(status), "message": message[:4096],
        }
        return error


def _rate_limit(headers: dict) -> dict:
    lower = {str(key).lower(): str(value) for key, value in headers.items()}
    result = {"limit": None, "remaining": None, "retry_after_seconds": None, "reset_at": None}
    match = _RATE_LIMIT.search(lower.get("ratelimit", ""))
    if match:
        result["remaining"] = match.group(2)
        result["reset_at"] = str(int(datetime.now(timezone.utc).timestamp()) + int(match.group(3)))
    policy = lower.get("ratelimit-policy", "")
    policy_match = re.search(r"q\s*=\s*(\d+)", policy, re.I)
    if policy_match:
        result["limit"] = policy_match.group(1)
    retry = lower.get("retry-after")
    if retry and re.fullmatch(r"\d{1,18}", retry.strip()):
        result["retry_after_seconds"] = retry.strip()
    return result


class HuggingFaceAdapter:
    """P046 adapter. All remote bytes flow through source.download_chunk."""

    def __init__(self, package, work, manifest, contracts):
        self.package = Path(package).resolve(strict=True)
        self.work = Path(work).resolve(strict=True)
        self.manifest, self.contracts = manifest, contracts
        profile_path = path_allowed(self.work / "profile.json", [self.work])
        try:
            profile = json.loads(profile_path.read_text(encoding="utf-8"), object_pairs_hook=_unique_pairs, parse_constant=_reject_constant)
        except (OSError, ValueError, json.JSONDecodeError):
            raise fail("VALIDATION_FAILED", "hf_profile_invalid") from None
        if set(profile) != {"source_id", "base_url"}:
            raise fail("VALIDATION_FAILED", "hf_profile_fields_invalid")
        source_id, base_url = profile["source_id"], profile["base_url"]
        if not isinstance(source_id, str) or not re.fullmatch(r"[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}", source_id):
            raise fail("VALIDATION_FAILED", "hf_source_id_invalid")
        if not isinstance(base_url, str):
            raise fail("VALIDATION_FAILED", "hf_base_url_invalid")
        permissions = manifest.get("permissions") or {}
        configured_origins = permissions.get("network_origins")
        if not isinstance(configured_origins, list):
            raise fail("FORBIDDEN", "hf_network_permissions_missing")
        self.allowed_origins = {_profile_origin(value) for value in configured_origins if isinstance(value, str)}
        self.http = HubHttp(base_url, self.allowed_origins)
        db_path = path_allowed(self.work / "hf-identities.sqlite", [self.work], writing=True)
        self.identities = IdentityStore(db_path)
        self.source_id = source_id
        self.snapshot_ttl = timedelta(minutes=30)
        self._catalog_lock = threading.RLock()
        # SearchSupport for this profile is deliberately limited to text. The
        # Core can post-filter capabilities without making an OR/AND claim.
        self.search_support = {
            "search_applicable": True,
            "supported_filters": ["source.search.q"],
            "limitations": [
                "huggingface_public_metadata_only_until_p045_p141_credential_resolution",
                "capabilities_are_deferred_to_core",
                "upstream_pages_are_a_moving_view",
                "downloads_use_source_download_chunk_only",
            ],
        }
        self.contracts.request("source.auth_probe", {"source_id": source_id, "auth_mode": "NONE", "account_id": None, "credential_reference_id": None})

    def open_session(self):
        self.identities.open()

    def shutdown(self):
        self.identities.close()

    def _check_source(self, source_id):
        if source_id is not None and source_id != self.source_id:
            raise fail("VALIDATION_FAILED", "hf_source_id_mismatch")

    def _api_url(self, path: str, query: dict | None = None) -> str:
        url = self.http.base_url + path
        if query:
            url += "?" + urllib.parse.urlencode(query, doseq=True)
        return url

    def _get_json(self, url: str, ctx=None):
        _status, headers, body = self.http.request(url, headers={"Accept": "application/json", "Accept-Encoding": "identity"}, ctx=ctx)
        try:
            value = json.loads(body.decode("utf-8"), object_pairs_hook=_unique_pairs, parse_constant=_reject_constant)
        except (UnicodeError, ValueError, json.JSONDecodeError):
            raise fail("PROTOCOL_ERROR", "hf_json_invalid") from None
        if not isinstance(value, (dict, list)):
            raise fail("PROTOCOL_ERROR", "hf_json_shape_invalid")
        return value, headers

    def _model_info(self, repo_id: str, revision: str | None = None, *, files_metadata=False, ctx=None):
        parts = "/".join(urllib.parse.quote(segment, safe="") for segment in repo_id.split("/"))
        path = f"/api/models/{parts}"
        if revision:
            if not _COMMIT.fullmatch(revision):
                raise fail("STALE_REVISION", "hf_revision_not_pinned")
            path += "/revision/" + urllib.parse.quote(revision, safe="")
        query = {"blobs": "true"} if files_metadata else {"expand": "author,cardData,config,createdAt,gated,lastModified,library_name,pipeline_tag,private,safetensors,sha,tags,usedStorage"}
        value, headers = self._get_json(self._api_url(path, query), ctx)
        if not isinstance(value, dict):
            raise fail("PROTOCOL_ERROR", "hf_model_info_shape_invalid")
        received_id = value.get("id")
        if isinstance(received_id, str) and received_id.casefold() != repo_id.casefold():
            raise fail("PROTOCOL_ERROR", "hf_model_identity_mismatch")
        return value, headers

    def _repo_and_info(self, ref: dict, *, files_metadata=False, ctx=None):
        repo = _repo_id(ref["upstream_namespace"], ref["upstream_model_id"])
        info, _ = self._model_info(repo, files_metadata=files_metadata, ctx=ctx)
        self.identities.distribution(repo, private=bool(info.get("private")), gated=info.get("gated"))
        return repo, info

    def _search_cursor(self, cursor: str | None, query_hash: str, query: str, limit: int):
        if cursor is None:
            return None, None
        if len(cursor) > 8192:
            raise fail("VALIDATION_FAILED", "hf_cursor_too_large")
        try:
            raw = base64.urlsafe_b64decode(cursor + "=" * (-len(cursor) % 4))
            value = json.loads(raw.decode("utf-8"), object_pairs_hook=_unique_pairs, parse_constant=_reject_constant)
        except (ValueError, UnicodeError, json.JSONDecodeError):
            raise fail("VALIDATION_FAILED", "hf_cursor_invalid") from None
        if set(value) != {"query_hash", "url", "snapshot_at"} or value["query_hash"] != query_hash:
            raise fail("VALIDATION_FAILED", "hf_cursor_query_mismatch")
        if not isinstance(value["url"], str) or not isinstance(value["snapshot_at"], str):
            raise fail("VALIDATION_FAILED", "hf_cursor_invalid")
        self.http._check_url(value["url"])
        parsed = urllib.parse.urlsplit(value["url"])
        expected_path = urllib.parse.urlsplit(self.http.base_url).path.rstrip("/") + "/api/models"
        if parsed.path != expected_path:
            raise fail("VALIDATION_FAILED", "hf_cursor_endpoint_invalid")
        params = urllib.parse.parse_qs(parsed.query, keep_blank_values=True)
        allowed = {"search", "limit", "full", "config", "sort", "cursor", "skip", "direction", "after"}
        if set(params) - allowed:
            raise fail("VALIDATION_FAILED", "hf_cursor_filter_mismatch")
        if params.get("search", [""])[0] != query:
            raise fail("VALIDATION_FAILED", "hf_cursor_filter_mismatch")
        if params.get("limit", [str(limit)])[0] != str(limit):
            raise fail("VALIDATION_FAILED", "hf_cursor_limit_mismatch")
        return value["url"], value["snapshot_at"]

    @staticmethod
    def _encode_cursor(url: str, query_hash: str, snapshot_at: str):
        payload = _json_bytes({"query_hash": query_hash, "url": url, "snapshot_at": snapshot_at})
        return base64.urlsafe_b64encode(payload).decode("ascii").rstrip("=")

    def _model_item(self, info: dict, *, commit: str | None = None):
        repo = info.get("id")
        if not isinstance(repo, str) or "/" not in repo:
            raise fail("PROTOCOL_ERROR", "hf_model_id_invalid")
        namespace = repo.split("/", 1)[0]
        commit = commit or info.get("sha")
        if not isinstance(commit, str) or not _COMMIT.fullmatch(commit):
            # A repo with no immutable commit can be displayed but not downloaded.
            commit = None
        self.identities.distribution(repo, private=bool(info.get("private")), gated=info.get("gated"))
        raw_siblings = info.get("siblings") if isinstance(info.get("siblings"), list) else []
        groups = _bundle_groups(raw_siblings)
        if not groups:
            # Preserve discoverability when the Hub has no recognizable bundle.
            groups = [{"key": "metadata:unclassified", "format": None, "files": []}]
        distribution_id, distribution_created = self.identities.distribution(repo, private=bool(info.get("private")), gated=info.get("gated"))
        result = []
        capabilities = _capabilities(info)
        limits = _model_limits(info)
        architecture = None
        config = info.get("config")
        if isinstance(config, dict):
            candidate = config.get("model_type") or config.get("architectures")
            if isinstance(candidate, str) and candidate:
                architecture = candidate[:256]
            elif isinstance(candidate, list) and candidate and isinstance(candidate[0], str):
                architecture = candidate[0][:256]
        gguf = info.get("gguf")
        if architecture is None and isinstance(gguf, dict):
            candidate = gguf.get("architecture")
            architecture = candidate[:256] if isinstance(candidate, str) and candidate else None
        safe_payload = _redact(info)
        if isinstance(safe_payload, dict):
            safe_payload.pop("siblings", None)
        for group in groups:
            bundle_key = group["key"]
            variant_id, created_at = self.identities.variant(repo, bundle_key)
            revision_key = _revision_key(commit, bundle_key) if commit else "pending:" + str(uuid.uuid7())
            manifest = self._manifest_document(repo, bundle_key, commit, info, group=group)
            revision_id, revision_created = self.identities.revision(repo, bundle_key, commit, manifest["manifest_hash"]) if commit else (str(uuid.uuid7()), _now())
            group_files = group["files"]
            format_name = group["format"]
            source_meta = {
                "original": safe_payload,
                "normalized": {
                    "source_type": "HUGGING_FACE", "source_id": self.source_id,
                    "repository_id": repo, "namespace": namespace,
                    "bundle_key": bundle_key, "revision_key": revision_key,
                    "catalog_reference": {
                        "source_id": self.source_id,
                        "upstream_namespace": namespace,
                        "upstream_model_id": repo,
                        "revision_key": revision_key,
                    },
                    "commit": commit, "private": bool(info.get("private")),
                    "gated": info.get("gated") if info.get("gated") is not None else False,
                    "downloads": _decimal(info.get("downloads")),
                    "likes": _decimal(info.get("likes")), "license": _license(info),
                    "upstream_distribution_id": distribution_id,
                },
                "limitations": self.search_support["limitations"],
                "rate_limit": {"limit": None, "remaining": None, "retry_after_seconds": None, "reset_at": None},
            }
            created_upstream = _stamp(info.get("createdAt"), fallback=distribution_created)
            modified_upstream = _stamp(info.get("lastModified"), fallback=_now())
            item = {
                "id": variant_id, "created_at": created_upstream, "updated_at": modified_upstream,
                "row_version": 1,
                "metadata": {"display_name": info.get("id", repo), "source_v1": source_meta},
                "revision_id": revision_id, "format": format_name,
                "quantization": _quantization(group_files), "architecture": architecture,
                "bundle_hash": None, "lifecycle": "DISCOVERED", "availability": "UNKNOWN",
                "ownership": "EXTERNAL", "promotion": "NONE", "system_role": "GENERAL",
                "capabilities": capabilities, "model_limits": limits,
                "system1_state": None, "update_available": False, "status": "DISCOVERED",
            }
            result.append(item)
        return result

    def _head_content_length(self, repo_id: str, commit: str, relative_path: str, ctx=None):
        encoded_repo = "/".join(urllib.parse.quote(segment, safe="") for segment in repo_id.split("/"))
        encoded_path = "/".join(urllib.parse.quote(segment, safe="") for segment in _normal_relative(relative_path).split("/"))
        url = f"{self.http.base_url}/{encoded_repo}/resolve/{urllib.parse.quote(commit, safe='')}/{encoded_path}"
        try:
            _status, headers, _body = self.http.request(
                url, method="HEAD", headers={"Accept-Encoding": "identity"}, ctx=ctx,
            )
        except RpcError as error:
            details = error.data.get("details", {}) if isinstance(error.data, dict) else {}
            if details.get("reason") == "hf_http_error":
                return None
            raise
        lower = {str(key).lower(): str(value) for key, value in headers.items()}
        content_length = lower.get("content-length", "")
        if not re.fullmatch(r"(?:0|[1-9][0-9]{0,18})", content_length):
            return None
        size = int(content_length)
        return size if size <= MAX_SQLITE_BYTES else None

    def _manifest_document(self, repo_id: str, bundle_key: str | None, commit: str | None, info: dict, *, group=None, size_fallback=False, ctx=None):
        siblings = info.get("siblings") if isinstance(info.get("siblings"), list) else []
        groups = _bundle_groups(siblings)
        if group is None:
            group = next((candidate for candidate in groups if candidate["key"] == bundle_key), None)
        if bundle_key is None:
            selected = [_file_metadata(item) for item in siblings if isinstance(item, dict)]
            for item in selected:
                item["required"] = True
        elif group is None:
            if bundle_key == "metadata:unclassified" and not groups:
                selected = []
            else:
                raise fail("STALE_REVISION", "hf_bundle_not_in_revision")
        else:
            selected = [item | {"required": True} for item in group["files"]]
        evidence = "huggingface:revision:" + (commit or "unknown")
        artifacts = [_descriptor(item, required=item.get("required", True), evidence=evidence + ":" + item["relative_path"], include_estimate=True) for item in selected]
        if size_fallback and commit:
            for artifact in artifacts:
                if artifact["size_bytes"] is not None:
                    continue
                size = self._head_content_length(repo_id, commit, artifact["relative_path"], ctx=ctx)
                if size is not None:
                    artifact["size_estimate"] = _artifact_size(
                        {"size_bytes": size, "sha256": None},
                        evidence + ":" + artifact["relative_path"] + ":content-length",
                        source_override="HTTP_HEAD",
                    )
        artifacts.sort(key=lambda artifact: artifact["relative_path"].casefold())
        hash_entries = [
            {
                "relative_path": artifact["relative_path"],
                "sha256": artifact["sha256"],
                "size_bytes": int(artifact["size_bytes"]) if artifact["size_bytes"] is not None else None,
                "required": artifact["required"],
            }
            for artifact in artifacts
        ]
        manifest_hash = _digest(hash_entries)
        return {"revision_key": _revision_key(commit, bundle_key) if commit and bundle_key else (f"commit:{commit}" if commit else "pending:" + str(uuid.uuid7())), "manifest_hash": manifest_hash, "artifacts": artifacts}

    def _pinned_info(self, revision: dict, *, ctx=None):
        repo = _repo_id(revision["upstream_namespace"], revision["upstream_model_id"])
        commit, bundle_key = _revision_parts(revision["revision_key"])
        private, gated = self.identities.flags(repo)
        if private:
            raise fail("AUTH_REQUIRED", "hf_private_repository_requires_credentials")
        if _is_gated(gated):
            raise fail("LICENSE_REQUIRED", "hf_gated_repository_requires_access")
        info, headers = self._model_info(repo, commit, files_metadata=True, ctx=ctx)
        self.identities.distribution(repo, private=bool(info.get("private")), gated=info.get("gated"))
        if info.get("private") is True:
            raise fail("AUTH_REQUIRED", "hf_private_repository_requires_credentials")
        if _is_gated(info.get("gated")):
            raise fail("LICENSE_REQUIRED", "hf_gated_repository_requires_access")
        observed_commit = info.get("sha")
        if isinstance(observed_commit, str) and observed_commit != commit:
            raise fail("STALE_REVISION", "hf_revision_changed")
        return repo, commit, bundle_key, info, headers

    def _estimate(self, artifacts: list[dict], evidence: str):
        known_sizes = []
        all_exact = bool(artifacts)
        unknown = False
        for artifact in artifacts:
            size = artifact.get("size_bytes")
            if size is None:
                estimate = artifact.get("size_estimate")
                estimate_bytes = estimate.get("bytes") if isinstance(estimate, dict) and estimate.get("confidence") in ("EXACT", "HIGH") else None
                if isinstance(estimate_bytes, str) and re.fullmatch(r"(?:0|[1-9][0-9]{0,18})", estimate_bytes):
                    size = int(estimate_bytes)
                else:
                    unknown = True
            if size is not None:
                if isinstance(size, str) and re.fullmatch(r"(?:0|[1-9][0-9]{0,18})", size):
                    size = int(size)
                if isinstance(size, bool) or not isinstance(size, int) or size < 0 or size > MAX_SQLITE_BYTES:
                    unknown = True
                    size = None
                else:
                    known_sizes.append(size)
            if size is None or artifact.get("sha256") is None or artifact.get("size_bytes") is None:
                all_exact = False
        total = sum(known_sizes)
        if total > MAX_SQLITE_BYTES:
            raise fail("VALIDATION_FAILED", "hf_size_overflow")
        if not artifacts:
            unknown = True
        total_text = str(total) if not unknown else None
        confidence = "UNKNOWN" if unknown else "EXACT" if all_exact else "HIGH"
        return {
            "download_bytes": total_text,
            "final_artifact_bytes": total_text,
            "installed_bytes": total_text,
            "derived_cache_bytes": "0",
            "temporary_peak_bytes": None,
            "existing_bytes": "0",
            "peak_extra_bytes": None,
            "required_free_bytes": None,
            "upper_bound_bytes": total_text,
            "confidence": confidence,
            "reason": "Estimación de la fuente; destino, temporales y reservas se calculan en Core.",
            "evidence_refs": [evidence],
            "per_volume": [],
        }

    def _bundle_manifest(self, revision: dict, *, size_fallback=False, ctx=None):
        repo, commit, bundle_key, info, _headers = self._pinned_info(revision, ctx=ctx)
        manifest = self._manifest_document(repo, bundle_key, commit, info, size_fallback=size_fallback, ctx=ctx)
        return repo, commit, bundle_key, info, manifest

    def _search(self, request: dict, ctx=None):
        if request["capabilities"]:
            raise fail("UNSUPPORTED_CAPABILITY", "hf_capabilities_deferred_to_core")
        query_hash = _digest({
            "source_id": self.source_id, "q": request["q"],
            "capabilities": [], "limit": request["limit"],
        })
        url, snapshot = self._search_cursor(request["cursor"], query_hash, request["q"], request["limit"])
        if url is None:
            params = {
                "limit": str(request["limit"]), "full": "true", "config": "true",
                "sort": "downloads",
            }
            if request["q"]:
                params["search"] = request["q"]
            url = self._api_url("/api/models", params)
            snapshot = _now()
        status, headers, body = self.http.request(url, headers={"Accept": "application/json", "Accept-Encoding": "identity"}, ctx=ctx)
        try:
            items = json.loads(body.decode("utf-8"), object_pairs_hook=_unique_pairs, parse_constant=_reject_constant)
        except (UnicodeError, ValueError, json.JSONDecodeError):
            raise fail("PROTOCOL_ERROR", "hf_search_json_invalid") from None
        if not isinstance(items, list):
            raise fail("PROTOCOL_ERROR", "hf_search_shape_invalid")
        if len(items) > request["limit"]:
            raise fail("PROTOCOL_ERROR", "hf_search_limit_exceeded")
        output = []
        for raw in items:
            if ctx is not None:
                ctx.check_cancelled()
            if not isinstance(raw, dict):
                raise fail("PROTOCOL_ERROR", "hf_search_item_invalid")
            output.extend(self._model_item(raw))
        next_url = _next_link(headers.get("Link") or headers.get("link"))
        next_cursor = None
        if next_url:
            next_url = urllib.parse.urljoin(url, next_url)
            if _safe_origin(next_url) != self.http.base_origin:
                raise fail("FORBIDDEN", "hf_pagination_origin_invalid")
            self.http._check_url(next_url)
            parsed_next = urllib.parse.urlsplit(next_url)
            expected_path = urllib.parse.urlsplit(self.http.base_url).path.rstrip("/") + "/api/models"
            if parsed_next.path != expected_path:
                raise fail("PROTOCOL_ERROR", "hf_pagination_endpoint_invalid")
            next_cursor = self._encode_cursor(next_url, query_hash, snapshot)
        self.contracts.response("source.search", {"items": output, "next_cursor": next_cursor, "snapshot_at": snapshot})
        return {"items": output, "next_cursor": next_cursor, "snapshot_at": snapshot}

    def _get_model(self, ref, *, ctx=None):
        repo, info = self._repo_and_info(ref, files_metadata=True, ctx=ctx)
        siblings = info.get("siblings") if isinstance(info.get("siblings"), list) else []
        groups = _bundle_groups(siblings)
        selected = []
        for group in groups:
            selected.extend(group["files"])
        unique = {}
        for file in selected:
            unique[file["relative_path"].casefold()] = file
        files = sorted(unique.values(), key=lambda item: item["relative_path"].casefold())
        multiple_groups = len(groups) > 1
        artifacts = [
            _descriptor(item, required=(not multiple_groups), evidence="huggingface:model-info:" + repo + ":" + item["relative_path"], include_estimate=False)
            for item in files
        ]
        config = info.get("config") if isinstance(info.get("config"), dict) else {}
        architecture = config.get("model_type") if isinstance(config.get("model_type"), str) else None
        if architecture is None and isinstance(info.get("gguf"), dict) and isinstance(info["gguf"].get("architecture"), str):
            architecture = info["gguf"]["architecture"]
        formats = {group["format"] for group in groups if group["format"]}
        quantizations = {_quantization(group["files"]) for group in groups if _quantization(group["files"])}
        limits = _model_limits(info)
        raw_metadata = _redact(info)
        if isinstance(raw_metadata, dict):
            raw_metadata.pop("siblings", None)
        source_metadata = {
            "source_type": "HUGGING_FACE", "repository_id": repo,
            "private": bool(info.get("private")), "gated": info.get("gated") if info.get("gated") is not None else False,
            "commit": info.get("sha"), "raw": raw_metadata,
            "limitations": self.search_support["limitations"],
        }
        inspection = {
            "upstream_identity": "huggingface:model:" + repo,
            "architecture": architecture,
            "format": next(iter(formats)) if len(formats) == 1 else None,
            "quantization": next(iter(quantizations)) if len(quantizations) == 1 else None,
            "capabilities": _capabilities(info), "limits": limits | {"source_v1": source_metadata},
            "artifacts": artifacts, "license": _license(info),
            "evidence_refs": ["huggingface:model-info:" + repo + (":" + info["sha"] if isinstance(info.get("sha"), str) else "")],
        }
        self.contracts.response("source.get_model", inspection)
        return inspection

    def _list_revisions(self, ref, *, ctx=None):
        repo, _info = self._repo_and_info(ref, ctx=ctx)
        path = "/api/models/" + "/".join(urllib.parse.quote(s, safe="") for s in repo.split("/")) + "/refs"
        refs, _ = self._get_json(self._api_url(path), ctx)
        if not isinstance(refs, dict):
            raise fail("PROTOCOL_ERROR", "hf_refs_shape_invalid")
        candidates = []
        for key in ("branches", "tags"):
            entries = refs.get(key, [])
            if not isinstance(entries, list):
                raise fail("PROTOCOL_ERROR", "hf_refs_list_invalid")
            for item in entries:
                if not isinstance(item, dict):
                    raise fail("PROTOCOL_ERROR", "hf_ref_invalid")
                commit = item.get("targetCommit") or item.get("target_commit")
                name = item.get("name")
                if isinstance(commit, str) and _COMMIT.fullmatch(commit) and isinstance(name, str):
                    candidates.append((commit, "branch" if key == "branches" else "tag", name))
        unique_commits = list(dict.fromkeys(commit for commit, _kind, _name in candidates))
        if not unique_commits:
            latest, _ = self._model_info(repo, ctx=ctx)
            commit = latest.get("sha")
            if isinstance(commit, str) and _COMMIT.fullmatch(commit):
                candidates = [(commit, "branch", "main")]
                unique_commits = [commit]
        if len(unique_commits) > MAX_REVISIONS:
            raise fail("UPSTREAM_UNAVAILABLE", "hf_revision_count_exceeds_limit")
        rows = []
        for commit in unique_commits:
            if ctx is not None:
                ctx.check_cancelled()
            info, _ = self._model_info(repo, commit, files_metadata=True, ctx=ctx)
            self.identities.distribution(repo, private=bool(info.get("private")), gated=info.get("gated"))
            if info.get("private") or _is_gated(info.get("gated")):
                continue
            groups = _bundle_groups(info.get("siblings", []))
            if not groups:
                groups = [{"key": "metadata:unclassified", "format": None, "files": []}]
            names = [{"kind": kind, "name": name} for ref_commit, kind, name in candidates if ref_commit == commit]
            for group in groups:
                manifest = self._manifest_document(repo, group["key"], commit, info, group=group)
                distribution_id, _distribution_created = self.identities.distribution(repo, private=False, gated=False)
                variant_id, _variant_created = self.identities.variant(repo, group["key"])
                revision_id, created_at = self.identities.revision(repo, group["key"], commit, manifest["manifest_hash"])
                rows.append({
                    "id": revision_id, "created_at": created_at, "updated_at": _now(), "row_version": 1,
                    "metadata": {"source_v1": {"ref_names": names, "bundle_key": group["key"], "source_type": "HUGGING_FACE"}},
                    "distribution_id": distribution_id,
                    "revision_key": _revision_key(commit, group["key"]), "upstream_commit": commit,
                    "manifest_hash": manifest["manifest_hash"], "state": "CANDIDATE",
                    "_variant_id": variant_id,
                })
        rows.sort(key=lambda row: (row["upstream_commit"], row["metadata"]["source_v1"]["bundle_key"]))
        for row in rows:
            row.pop("_variant_id", None)
        response = {"items": rows, "next_cursor": None, "snapshot_at": _now()}
        self.contracts.response("source.list_revisions", response)
        return response

    def _estimate_from_revision(self, revision, *, ctx=None):
        _repo, _commit, _bundle, _info, manifest = self._bundle_manifest(revision, size_fallback=True, ctx=ctx)
        return self._estimate(manifest["artifacts"], "huggingface:manifest:" + manifest["manifest_hash"])

    def _download_plan(self, request, *, ctx=None):
        self._check_source(request.get("source_id"))
        if request.get("storage_location_id") is None:
            raise fail("UNSUPPORTED_CAPABILITY", "hf_destination_preflight_owned_by_core")
        acquisition = validate_source_acquisition(request)
        if acquisition is not None:
            revision = dict(acquisition["revision"])
        else:
            if request.get("optional_artifact_ids"):
                # Artifact UUIDs are Core-owned and are absent from SourceArtifact;
                # Core P052 resolves those IDs and seals the selected file set.
                raise fail("UNSUPPORTED_CAPABILITY", "hf_optional_artifact_binding_owned_by_core")
            context = self.identities.variant_context(request["variant_id"])
            if context is None:
                raise fail("NOT_FOUND", "hf_variant_unknown")
            repo, bundle_key = context
            private, gated = self.identities.flags(repo)
            if private:
                raise fail("AUTH_REQUIRED", "hf_private_repository_requires_credentials")
            if _is_gated(gated):
                raise fail("LICENSE_REQUIRED", "hf_gated_repository_requires_access")
            # Select the most recent immutable commit observed for this bundle.
            row = self.identities._db().execute(
                "SELECT commit_hash FROM revisions WHERE repo_id=? AND bundle_key=? ORDER BY created_at DESC LIMIT 1",
                (repo, bundle_key),
            ).fetchone()
            if row is None:
                raise fail("STALE_REVISION", "hf_variant_has_no_pinned_revision")
            commit = row[0]
            revision = {"source_id": self.source_id, "upstream_namespace": repo.split("/", 1)[0], "upstream_model_id": repo, "revision_key": _revision_key(commit, bundle_key)}
        _repo, _commit, _bundle, info, manifest = self._bundle_manifest(revision, ctx=ctx)
        artifacts = select_source_acquisition(request, manifest) if acquisition is not None else manifest["artifacts"]
        if not artifacts:
            raise fail("UNSUPPORTED_CAPABILITY", "hf_bundle_has_no_downloadable_artifacts")
        spec = {
            "variant_id": request["variant_id"], "storage_location_id": request["storage_location_id"],
            "revision": revision, "artifacts": artifacts,
            "auth": {"authenticated": False, "auth_required": False, "license_required": False, "reason": "huggingface_public_repository"},
            "resumable": all(_file_resume_supported(artifact) for artifact in artifacts),
            "priority": request["priority"], "hard_budget_bytes": request["hard_budget_bytes"],
        }
        estimate = self._estimate(artifacts, "huggingface:manifest:" + manifest["manifest_hash"])
        plan = {
            "plan_id": str(uuid.uuid7()), "plan_hash": "0" * 64,
            "operation": "source.download", "resource_ids": [request["variant_id"], request["storage_location_id"]],
            "storage_estimate": estimate,
            "steps": [{"phase": "DOWNLOAD", "adapter_id": self.manifest["id"], "request": source_acquisition_payload(request, spec)}],
            "expires_at": (datetime.now(timezone.utc) + timedelta(seconds=900)).isoformat(timespec="milliseconds").replace("+00:00", "Z"),
            "approval_required": False,
        }
        plan["plan_hash"] = _digest({key: value for key, value in plan.items() if key != "plan_hash"})
        self.contracts.response("source.download_plan", plan)
        return plan

    def _download_chunk(self, request, ctx):
        self._check_source(request["source_id"])
        revision = request["revision"]
        repo, commit, bundle_key, info, manifest = self._bundle_manifest(revision, ctx=ctx)
        artifact = next((item for item in manifest["artifacts"] if item["relative_path"] == request["relative_path"]), None)
        if artifact is None:
            raise fail("VALIDATION_FAILED", "hf_file_not_in_pinned_bundle")
        path = _normal_relative(request["relative_path"])
        offset = _parse_decimal(request["offset"], "hf_offset_invalid")
        max_bytes = _parse_decimal(request["max_bytes"], "hf_chunk_size_invalid")
        if not 1 <= max_bytes <= CHUNK_MAX_BYTES:
            raise fail("VALIDATION_FAILED", "hf_chunk_size_invalid")
        if offset > MAX_SQLITE_BYTES - max_bytes:
            raise fail("VALIDATION_FAILED", "hf_range_overflow")
        expected_size = artifact.get("size_bytes")
        if isinstance(expected_size, str) and re.fullmatch(r"(?:0|[1-9][0-9]{0,18})", expected_size):
            expected_size = int(expected_size)
        elif expected_size is not None:
            raise fail("PROTOCOL_ERROR", "hf_manifest_size_invalid")
        if expected_size is not None and offset > expected_size:
            raise fail("VALIDATION_FAILED", "hf_offset_past_eof")
        request_validator = request["validator"]
        if offset > 0 and request_validator is None:
            raise fail("CONFLICT", "hf_resume_validator_missing")
        if request_validator and request_validator.get("kind") == "LAST_MODIFIED_AND_SHA256":
            if artifact.get("sha256") is None or artifact["sha256"] != request_validator.get("sha256"):
                raise fail("CONFLICT", "hf_resume_checksum_changed")
        headers = {"Accept-Encoding": "identity", "Accept": "application/octet-stream"}
        end = offset + max_bytes - 1
        headers["Range"] = f"bytes={offset}-{end}"
        if request_validator and request_validator["kind"] == "STRONG_ETAG":
            headers["If-Range"] = request_validator["value"]
        elif request_validator and request_validator["kind"] == "LAST_MODIFIED_AND_SHA256":
            headers["If-Range"] = request_validator["last_modified"]
        encoded_repo = "/".join(urllib.parse.quote(segment, safe="") for segment in repo.split("/"))
        encoded_path = "/".join(urllib.parse.quote(segment, safe="") for segment in path.split("/"))
        url = f"{self.http.base_url}/{encoded_repo}/resolve/{urllib.parse.quote(commit, safe='')}/{encoded_path}"
        status, response_headers, data = self.http.request_chunk(url, headers=headers, maximum=max_bytes, ctx=ctx)
        lower = {key.lower(): value for key, value in response_headers.items()}
        if lower.get("content-encoding", "identity").lower() != "identity":
            raise fail("PROTOCOL_ERROR", "hf_content_encoding_not_identity")
        if status == 416:
            if expected_size is not None and offset == expected_size:
                result = {"offset": str(offset), "data_base64": "", "eof": True, "total_bytes": str(offset), "validator": request_validator}
                self.contracts.response("source.download_chunk", result)
                return result
            raise fail("STALE_REVISION", "hf_range_not_satisfiable")
        if status == 206:
            match = _CONTENT_RANGE.fullmatch(lower.get("content-range", ""))
            if not match:
                raise fail("PROTOCOL_ERROR", "hf_content_range_invalid")
            start, end = int(match.group(1)), int(match.group(2))
            total = None if match.group(3) == "*" else int(match.group(3))
            if start != offset or end < start or end - start + 1 != len(data):
                raise fail("PROTOCOL_ERROR", "hf_content_range_mismatch")
            if len(data) > max_bytes or end >= MAX_SQLITE_BYTES:
                raise fail("PROTOCOL_ERROR", "hf_content_range_overflow")
            if total is not None and (total > MAX_SQLITE_BYTES or end >= total):
                raise fail("PROTOCOL_ERROR", "hf_content_range_total_invalid")
            if expected_size is not None and total is not None and expected_size != total:
                raise fail("INTEGRITY_FAILED", "hf_size_changed_for_pinned_revision")
        elif status == 200:
            if offset != 0:
                raise fail("CONFLICT", "hf_server_ignored_range_restart_required")
            length = lower.get("content-length")
            total = int(length) if length and length.isdecimal() and int(length) <= MAX_SQLITE_BYTES else expected_size
            if total is not None and len(data) > total:
                raise fail("PROTOCOL_ERROR", "hf_body_exceeds_declared_size")
        else:
            raise fail("PROTOCOL_ERROR", "hf_download_status_invalid")
        if expected_size is not None and total is not None and expected_size != total:
            raise fail("INTEGRITY_FAILED", "hf_size_changed_for_pinned_revision")
        etag = lower.get("etag")
        last_modified = lower.get("last-modified")
        validator = None
        if isinstance(etag, str) and _ETAG.fullmatch(etag):
            validator = {"kind": "STRONG_ETAG", "value": etag}
        elif last_modified and artifact.get("sha256"):
            validator = {"kind": "LAST_MODIFIED_AND_SHA256", "last_modified": last_modified, "sha256": artifact["sha256"]}
        if request_validator is not None and validator != request_validator:
            raise fail("CONFLICT", "hf_resume_validator_changed")
        if status == 200 and total is None:
            eof = len(data) < max_bytes
            if eof:
                total = len(data)
        else:
            eof = total is not None and offset + len(data) == total
            if status == 206 and total is None and len(data) < max_bytes:
                eof, total = True, offset + len(data)
        if not data and not eof:
            raise fail("UPSTREAM_UNAVAILABLE", "hf_empty_nonterminal_chunk", retryable=True)
        if eof and total is None:
            total = offset + len(data)
        if total is None and expected_size is not None:
            total = expected_size
            eof = offset + len(data) == total
        result = {
            "offset": str(offset), "data_base64": base64.b64encode(data).decode("ascii"),
            "eof": bool(eof), "total_bytes": str(total) if total is not None else None,
            "validator": validator,
        }
        self.contracts.response("source.download_chunk", result)
        return result

    def _sync(self, request, ctx=None):
        query = request["query"]
        if not isinstance(query, dict) or set(query) - {"q", "capabilities"}:
            raise fail("UNSUPPORTED_CAPABILITY", "hf_sync_filter_unsupported")
        search = {
            "source_id": self.source_id,
            "q": query.get("q", ""),
            "capabilities": query.get("capabilities", []),
            "cursor": request["cursor"],
            "limit": 50,
        }
        if not isinstance(search["q"], str) or not isinstance(search["capabilities"], list):
            raise fail("VALIDATION_FAILED", "hf_sync_query_invalid")
        page = self._search(search, ctx)
        revisions = []
        seen = set()
        for variant in page["items"]:
            repo = variant["metadata"]["source_v1"]["normalized"]["repository_id"]
            if repo not in seen:
                seen.add(repo)
                inspection = self._get_model({"source_id": self.source_id, "upstream_namespace": repo.split("/", 1)[0], "upstream_model_id": repo}, ctx=ctx)
                revisions.append({"repo_id": repo, "upstream_identity": inspection["upstream_identity"], "evidence_refs": inspection["evidence_refs"]})
        etag = _digest({"q": search["q"], "capabilities": search["capabilities"], "variants": [
            {"id": item["id"], "revision_id": item["revision_id"], "source": item["metadata"]["source_v1"]["normalized"]["commit"]}
            for item in page["items"]
        ]})
        changed = request["etag"] != etag
        models = []
        if changed:
            for row in revisions:
                model = self._get_model({"source_id": self.source_id, "upstream_namespace": row["repo_id"].split("/", 1)[0], "upstream_model_id": row["repo_id"]}, ctx=ctx)
                models.append(model)
        result = {"cursor": page["next_cursor"], "etag": etag, "changed": changed, "models": models}
        self.contracts.response("source.sync", result)
        return result

    def call(self, method, params, ctx):
        try:
            if isinstance(params, dict):
                self._check_source(params.get("source_id"))
            ctx.check_cancelled()
            if method == "source.search":
                return self._search(params, ctx)
            if method == "source.get_model":
                return self._get_model(params, ctx=ctx)
            if method == "source.list_revisions":
                return self._list_revisions(params, ctx=ctx)
            if method == "source.get_manifest":
                _repo, _commit, _bundle, _info, manifest = self._bundle_manifest(params, size_fallback=True, ctx=ctx)
                self.contracts.response("source.get_manifest", manifest)
                return manifest
            if method == "source.estimate_size":
                return self._estimate_from_revision(params, ctx=ctx)
            if method == "source.download_plan":
                return self._download_plan(params, ctx=ctx)
            if method == "source.download":
                raise fail("UNSUPPORTED_CAPABILITY", "hf_download_execution_owned_by_download_manager")
            if method == "source.download_chunk":
                return self._download_chunk(params, ctx)
            if method == "source.sync":
                return self._sync(params, ctx)
            if method == "source.auth_probe":
                if params["auth_mode"] == "NONE" and params["account_id"] is None and params["credential_reference_id"] is None:
                    result = {"authenticated": False, "auth_required": False, "license_required": False, "reason": "huggingface_anonymous_public_access"}
                else:
                    result = {"authenticated": False, "auth_required": True, "license_required": False, "reason": "huggingface_credentials_unavailable_until_p045_p141"}
                self.contracts.response("source.auth_probe", result)
                return result
            raise fail("UNSUPPORTED_CAPABILITY", "hf_method_not_supported")
        except ContractViolation:
            raise fail("VALIDATION_FAILED", "hf_contract_document_invalid") from None
        except sqlite3.Error:
            raise fail("RESOURCE_UNAVAILABLE", "hf_identity_store_failed", retryable=True) from None
        except RpcError:
            raise
        except (OSError, ValueError, TypeError):
            # Never expose paths, response bodies, or credential material.
            raise fail("RESOURCE_UNAVAILABLE", "hf_adapter_io_failed", retryable=True) from None


def _next_link(value: str | None) -> str | None:
    if not value:
        return None
    for part in re.finditer(r"<([^>]+)>\s*;\s*rel\s*=\s*(?:\"next\"|next)(?:\s*;|\s*,|$)", value, re.I):
        return part.group(1)
    return None


def _parse_decimal(value: str, reason: str) -> int:
    if not isinstance(value, str) or not re.fullmatch(r"(?:0|[1-9][0-9]{0,18})", value):
        raise fail("VALIDATION_FAILED", reason)
    parsed = int(value)
    if parsed > MAX_SQLITE_BYTES:
        raise fail("VALIDATION_FAILED", reason)
    return parsed


def _file_resume_supported(artifact: dict) -> bool:
    # The Hub file response exposes validators only at transfer time. The plan
    # cannot promise re-resumability based solely on a filename/commit.
    return bool(artifact.get("sha256"))

