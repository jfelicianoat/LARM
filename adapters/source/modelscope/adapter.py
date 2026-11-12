"""ModelScope Hub adapter. Network metadata is read-only and model bytes
are returned only as bounded chunks to LARM's Download Manager.
"""
from __future__ import annotations

import base64
import hashlib
import json
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
from larm_adapter_sdk import ContractViolation, RpcError
from larm_adapter_sdk.access import NoRedirect, fail, path_allowed
from larm_adapter_sdk.source_acquisition import validate_source_acquisition, select_source_acquisition, source_acquisition_payload


HTTP_TIMEOUT_SECONDS = 8
MAX_JSON_BYTES = 8 << 20
MAX_REVISIONS = 64
MAX_REDIRECTS = 5
MAX_SQLITE_BYTES = (1 << 63) - 1
MAX_CANONICAL_INTEGER = (1 << 53) - 1
CHUNK_MAX_BYTES = 512 << 10
UUID7 = re.compile(r"^[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$")
SHA256 = re.compile(r"^[0-9a-f]{64}$")
COMMIT = re.compile(r"^[0-9a-f]{40,64}$")
ETAG = re.compile(r'^"[!#-~]*"$')
CONTENT_RANGE = re.compile(r"^bytes (\d+)-(\d+)/(\d+|\*)$")
SENSITIVE_KEYS = {
    "token", "access_token", "refresh_token", "api_token", "api_key", "apikey",
    "password", "passwd", "secret", "authorization", "credential", "credentials",
    "cookie", "cookies", "signature", "x-amz-signature", "x-goog-signature",
}


def _now() -> str:
    return datetime.now(timezone.utc).isoformat(timespec="milliseconds").replace("+00:00", "Z")


def _stamp(value: object, fallback: str) -> str:
    if not isinstance(value, str) or not value:
        return fallback
    try:
        parsed = datetime.fromisoformat(value.replace("Z", "+00:00"))
        if parsed.tzinfo is None:
            return fallback
        return parsed.astimezone(timezone.utc).isoformat(timespec="milliseconds").replace("+00:00", "Z")
    except ValueError:
        return fallback


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
    if isinstance(value, dict):
        return {
            str(key): "[REDACTED]" if str(key).lower() in SENSITIVE_KEYS or str(key).lower().endswith(("_token", "_password", "_secret")) else _redact(item)
            for key, item in value.items()
        }
    if isinstance(value, list):
        return [_redact(item) for item in value]
    if isinstance(value, str):
        if len(value) > 65536:
            return "[REDACTED]"
        parsed = urllib.parse.urlsplit(value)
        if parsed.scheme.lower() in ("http", "https") and parsed.query:
            if any(k.lower() in SENSITIVE_KEYS or any(part in k.lower() for part in ("token", "signature", "secret", "credential", "password", "cookie")) for k, _ in urllib.parse.parse_qsl(parsed.query, keep_blank_values=True)):
                return "[REDACTED]"
    return value


def _canonical(value) -> bytes:
    def normalize(item):
        if item is None or isinstance(item, (str, bool, float)):
            return item
        if isinstance(item, int):
            if abs(item) > (1 << 53):
                raise ValueError("integer outside RFC 8785 range")
            return float(item) if abs(item) == 1 << 53 else item
        if isinstance(item, list):
            return [normalize(child) for child in item]
        if isinstance(item, dict) and all(isinstance(key, str) for key in item):
            return {key: normalize(child) for key, child in item.items()}
        raise ValueError("value is not canonical JSON")
    return rfc8785.dumps(normalize(value))


def _digest(value) -> str:
    return hashlib.sha256(_canonical(value)).hexdigest()


def _decimal(value, *, maximum=MAX_SQLITE_BYTES):
    if isinstance(value, bool) or not isinstance(value, int) or not 0 <= value <= maximum:
        return None
    return str(value)


def _origin(url: str) -> str:
    try:
        parsed = urllib.parse.urlsplit(url)
        port = parsed.port
    except (TypeError, ValueError):
        raise fail("FORBIDDEN", "modelscope_origin_invalid") from None
    if parsed.scheme not in ("https", "http") or not parsed.hostname or parsed.username is not None or parsed.password is not None or parsed.fragment:
        raise fail("FORBIDDEN", "modelscope_origin_invalid")
    host = parsed.hostname.lower()
    port_text = port or (443 if parsed.scheme == "https" else 80)
    if ":" in host:
        host = f"[{host}]"
    return f"{parsed.scheme}://{host}:{port_text}"


def _profile_origin(url: str) -> str:
    parsed = urllib.parse.urlsplit(url)
    if parsed.query or parsed.fragment or parsed.path not in ("", "/"):
        raise fail("VALIDATION_FAILED", "modelscope_base_url_invalid")
    origin = _origin(url)
    if parsed.scheme == "http" and not origin.startswith(("http://127.0.0.1:", "http://localhost:", "http://[::1]:")):
        raise fail("FORBIDDEN", "modelscope_http_non_loopback_forbidden")
    return origin


def _repo_id(namespace: str, model_id: str) -> str:
    if not isinstance(namespace, str) or not isinstance(model_id, str):
        raise fail("VALIDATION_FAILED", "modelscope_model_identity_invalid")
    repo = model_id if model_id.startswith(namespace + "/") else f"{namespace}/{model_id}" if "/" not in model_id else model_id
    parts = repo.split("/")
    if len(parts) != 2 or any(not p or p in (".", "..") or any(ord(c) < 32 for c in p) for p in parts):
        raise fail("VALIDATION_FAILED", "modelscope_model_identity_invalid")
    return repo


def _relative(path: str) -> str:
    if not isinstance(path, str) or not path or len(path) > 4096 or path.startswith("/") or "\\" in path or "\x00" in path:
        raise fail("VALIDATION_FAILED", "modelscope_relative_path_invalid")
    reserved = {"con", "prn", "aux", "nul", "conin$", "conout$"} | {f"{p}{n}" for p in ("com", "lpt") for n in range(1, 10)}
    for part in path.split("/"):
        if not part or part in (".", "..") or part.endswith((".", " ")) or any(c in part for c in '<>:"|?*') or any(ord(c) < 32 for c in part) or part.split(".")[0].lower() in reserved:
            raise fail("VALIDATION_FAILED", "modelscope_relative_path_invalid")
    return path


def _size(entry: dict) -> int | None:
    for value in (entry.get("Size"), entry.get("size"), entry.get("size_bytes")):
        if isinstance(value, int) and not isinstance(value, bool) and 0 <= value <= MAX_CANONICAL_INTEGER:
            return value
    lfs = entry.get("Lfs") or entry.get("lfs")
    if isinstance(lfs, dict):
        value = lfs.get("Size") or lfs.get("size")
        if isinstance(value, int) and not isinstance(value, bool) and 0 <= value <= MAX_CANONICAL_INTEGER:
            return value
    return None


def _sha256(entry: dict) -> str | None:
    values = [entry.get("Sha256"), entry.get("sha256")]
    lfs = entry.get("Lfs") or entry.get("lfs")
    if isinstance(lfs, dict):
        values.extend((lfs.get("Sha256"), lfs.get("sha256"), lfs.get("oid")))
    for value in values:
        if isinstance(value, str):
            clean = value.removeprefix("sha256:").lower()
            if SHA256.fullmatch(clean):
                return clean
    return None


def _file(entry: dict) -> dict:
    path = entry.get("Path") or entry.get("path") or entry.get("Name") or entry.get("name")
    if not isinstance(path, str):
        raise fail("PROTOCOL_ERROR", "modelscope_file_path_missing")
    return {"relative_path": _relative(path), "size_bytes": _size(entry), "sha256": _sha256(entry), "raw": _redact(entry)}


def _role(path: str) -> str:
    name = path.lower().rsplit("/", 1)[-1]
    if "mmproj" in name:
        return "mmproj"
    if name.endswith((".safetensors", ".gguf", ".onnx", ".bin", ".pt", ".pth", ".model")):
        return "weights"
    if name.endswith((".onnx_data", ".onnx.data", ".data")):
        return "external_data"
    if "tokenizer" in name or name in {"vocab.json", "merges.txt", "spiece.model"}:
        return "tokenizer"
    if "processor" in name:
        return "processor"
    if name in {"config.json", "generation_config.json", "model.safetensors.index.json", "pytorch_model.bin.index.json"}:
        return "config"
    return "other"


def _format_groups(files: list[dict]) -> list[dict]:
    normalized = []
    seen = set()
    for raw in files:
        item = raw if "relative_path" in raw else _file(raw)
        path = _relative(item["relative_path"])
        if path.casefold() in seen:
            raise fail("PROTOCOL_ERROR", "modelscope_duplicate_file_path")
        seen.add(path.casefold())
        normalized.append(item | {"relative_path": path})
    groups = []
    for extension, format_name in ((".gguf", "GGUF"), (".safetensors", "SAFETENSORS"), (".onnx", "ONNX"), (".bin", "PYTORCH"), (".pt", "PYTORCH"), (".pth", "PYTORCH")):
        weights = [item for item in normalized if item["relative_path"].lower().endswith(extension)]
        if format_name == "SAFETENSORS":
            dirs = sorted({str(PurePosixPath(item["relative_path"]).parent).casefold() for item in weights})
            for directory in dirs:
                members = [item for item in normalized if str(PurePosixPath(item["relative_path"]).parent).casefold() == directory and (_role(item["relative_path"]) != "other" or item["relative_path"].lower().endswith(extension))]
                key = "transformers:safetensors" if len(dirs) == 1 else "transformers:safetensors:" + directory
                groups.append({"key": key, "format": format_name, "files": members})
            continue
        if format_name == "PYTORCH":
            dirs = sorted({str(PurePosixPath(item["relative_path"]).parent).casefold() for item in weights})
            for directory in dirs:
                members = [item for item in normalized if str(PurePosixPath(item["relative_path"]).parent).casefold() == directory and (_role(item["relative_path"]) != "other" or item in weights)]
                groups.append({"key": "transformers:pytorch:" + directory, "format": format_name, "files": members})
            continue
        for item in weights:
            members = [item]
            if format_name == "ONNX":
                sidecar = item["relative_path"] + ".data"
                found = next((entry for entry in normalized if entry["relative_path"].casefold() == sidecar.casefold()), None)
                if found:
                    members.append(found)
            groups.append({"key": format_name.lower() + ":" + item["relative_path"], "format": format_name, "files": members})
    for group in groups:
        group["files"] = sorted(group["files"], key=lambda row: row["relative_path"].casefold())
    if not groups:
        groups = [{"key": "metadata:unclassified", "format": None, "files": []}]
    return sorted(groups, key=lambda row: (row["format"] or "", row["key"].casefold()))


def _revision_key(revision: str, bundle: str | None = None) -> str:
    if COMMIT.fullmatch(revision):
        prefix = "commit:" + revision.lower()
    else:
        prefix = "ref:" + base64.urlsafe_b64encode(revision.encode()).decode().rstrip("=")
    if bundle is not None:
        prefix += "#bundle=" + base64.urlsafe_b64encode(bundle.encode()).decode().rstrip("=")
    return prefix


def _revision_parts(key: str) -> tuple[str, str | None, bool]:
    if not isinstance(key, str) or len(key) > 8192:
        raise fail("STALE_REVISION", "modelscope_revision_key_invalid")
    prefix, sep, encoded_bundle = key.partition("#bundle=")
    if prefix.startswith("commit:") and COMMIT.fullmatch(prefix[7:]):
        revision, pinned = prefix[7:].lower(), True
    elif prefix.startswith("ref:"):
        try:
            revision = base64.urlsafe_b64decode(prefix[4:] + "=" * (-len(prefix[4:]) % 4)).decode("utf-8")
        except (ValueError, UnicodeError):
            raise fail("VALIDATION_FAILED", "modelscope_revision_key_invalid") from None
        if not revision or len(revision) > 512:
            raise fail("VALIDATION_FAILED", "modelscope_revision_key_invalid")
        pinned = False
    else:
        raise fail("STALE_REVISION", "modelscope_revision_not_mapped")
    bundle = None
    if sep:
        try:
            bundle = base64.urlsafe_b64decode(encoded_bundle + "=" * (-len(encoded_bundle) % 4)).decode("utf-8")
        except (ValueError, UnicodeError):
            raise fail("VALIDATION_FAILED", "modelscope_revision_bundle_invalid") from None
        if not bundle or len(bundle) > 4096:
            raise fail("VALIDATION_FAILED", "modelscope_revision_bundle_invalid")
    return revision, bundle, pinned


def _model_limits(info: dict) -> dict:
    config = info.get("config") if isinstance(info.get("config"), dict) else {}
    def integer(*keys):
        for source in (config, info):
            for key in keys:
                value = source.get(key)
                if isinstance(value, int) and not isinstance(value, bool) and 0 <= value <= MAX_SQLITE_BYTES:
                    return value
        return None
    architecture = _redact(config)
    try:
        architecture_hash = _digest(architecture) if architecture else None
    except ValueError:
        architecture_hash = None
    return {
        "parameter_count": _decimal(integer("num_parameters", "parameter_count", "parameters")),
        "layers": integer("num_hidden_layers", "n_layer", "num_layers"),
        "kv_heads": integer("num_key_value_heads", "num_kv_heads"),
        "head_dim": integer("head_dim", "kv_channels"),
        "context_limit": integer("max_position_embeddings", "n_positions", "max_seq_len", "seq_length"),
        "kv_bits": None, "model_max_memory_bytes": None,
        "architecture_metadata_hash": architecture_hash,
    }


def _capabilities(info: dict) -> list[str]:
    task = info.get("Task") or info.get("task") or info.get("pipeline_tag")
    mapping = {
        "text-generation": "text.generate", "text2text-generation": "text.generate",
        "text-classification": "text.classify", "feature-extraction": "embedding.create",
        "image-classification": "vision.understand", "text-to-image": "image.generate",
        "automatic-speech-recognition": "audio.transcribe", "text-to-speech": "speech.synthesize",
    }
    return [mapping[task]] if isinstance(task, str) and task in mapping else []


def _license(info: dict) -> str | None:
    for key in ("License", "license", "license_name", "LicenseName"):
        value = info.get(key)
        if isinstance(value, str) and value.strip():
            return value.strip()[:256]
    return None


def _size_estimate(item: dict, evidence: str) -> dict:
    size, checksum = item.get("size_bytes"), item.get("sha256")
    if size is not None and checksum is not None:
        confidence, source, reason = "EXACT", "MANIFEST", "La API de archivos publica tamaño y SHA-256 para esta revisión."
        lower, upper, value = str(size), str(size), str(size)
    elif size is not None:
        confidence, source, reason = "HIGH", "SOURCE_METADATA", "Tamaño publicado por la API de archivos; no hay SHA-256 utilizable."
        lower, upper, value = None, str(size), str(size)
    else:
        confidence, source, reason = "UNKNOWN", "UNKNOWN", "La API no publica un tamaño verificable para este archivo."
        lower = upper = value = None
    return {"confidence": confidence, "bytes": value, "lower_bound_bytes": lower, "upper_bound_bytes": upper,
            "source": source, "evidence_refs": [evidence], "reason": reason}


def _descriptor(item: dict, required: bool, evidence: str, *, estimate: bool) -> dict:
    result = {"relative_path": item["relative_path"], "size_bytes": _decimal(item.get("size_bytes")),
              "sha256": item.get("sha256"), "required": bool(required), "role": _role(item["relative_path"])}
    if estimate:
        result["size_estimate"] = _size_estimate(item, evidence)
    return result


class IdentityStore:
    """Stable source-owned UUIDv7 bindings; credentials are never stored."""
    def __init__(self, path: Path):
        self.path, self.lock, self.connection = path, threading.RLock(), None

    def open(self):
        with self.lock:
            if self.connection is not None:
                return
            db = sqlite3.connect(self.path, timeout=5, isolation_level=None, check_same_thread=False)
            db.execute("PRAGMA journal_mode=WAL")
            db.execute("PRAGMA synchronous=FULL")
            db.execute("PRAGMA foreign_keys=ON")
            db.executescript("""
                CREATE TABLE IF NOT EXISTS distributions(repo_id TEXT PRIMARY KEY, id TEXT NOT NULL UNIQUE, created_at TEXT NOT NULL, private INTEGER NOT NULL, gated INTEGER NOT NULL);
                CREATE TABLE IF NOT EXISTS variants(repo_id TEXT NOT NULL, bundle_key TEXT NOT NULL, id TEXT NOT NULL UNIQUE, created_at TEXT NOT NULL, PRIMARY KEY(repo_id,bundle_key));
                CREATE TABLE IF NOT EXISTS revisions(repo_id TEXT NOT NULL, bundle_key TEXT NOT NULL, revision_key TEXT NOT NULL, id TEXT NOT NULL UNIQUE, created_at TEXT NOT NULL, manifest_hash TEXT NOT NULL, PRIMARY KEY(repo_id,bundle_key,revision_key));
            """)
            self.connection = db

    def close(self):
        with self.lock:
            if self.connection is not None:
                self.connection.close()
                self.connection = None

    def _db(self):
        if self.connection is None:
            raise fail("RESOURCE_UNAVAILABLE", "modelscope_identity_store_not_open")
        return self.connection

    def distribution(self, repo: str, private: bool, gated: bool):
        with self.lock:
            db = self._db()
            row = db.execute("SELECT id,created_at FROM distributions WHERE repo_id=?", (repo,)).fetchone()
            if row is None:
                row = (str(uuid.uuid7()), _now())
                db.execute("INSERT INTO distributions VALUES(?,?,?,?,?)", (repo, row[0], row[1], int(private), int(gated)))
            else:
                db.execute("UPDATE distributions SET private=?,gated=? WHERE repo_id=?", (int(private), int(gated), repo))
            return row

    def variant(self, repo: str, bundle: str):
        with self.lock:
            db = self._db()
            row = db.execute("SELECT id,created_at FROM variants WHERE repo_id=? AND bundle_key=?", (repo, bundle)).fetchone()
            if row is None:
                row = (str(uuid.uuid7()), _now())
                db.execute("INSERT INTO variants VALUES(?,?,?,?)", (repo, bundle, row[0], row[1]))
            return row

    def revision(self, repo: str, bundle: str, revision_key: str, manifest_hash: str):
        with self.lock:
            db = self._db()
            row = db.execute("SELECT id,created_at,manifest_hash FROM revisions WHERE repo_id=? AND bundle_key=? AND revision_key=?", (repo, bundle, revision_key)).fetchone()
            if row is None:
                identity = str(uuid.uuid7())
                created = _now()
                db.execute("INSERT INTO revisions VALUES(?,?,?,?,?,?)", (repo, bundle, revision_key, identity, created, manifest_hash))
                return identity, created
            if row[2] != manifest_hash:
                raise fail("INTEGRITY_FAILED", "modelscope_pinned_manifest_changed")
            return row[0], row[1]

    def variant_context(self, variant_id: str):
        with self.lock:
            row = self._db().execute("SELECT repo_id,bundle_key FROM variants WHERE id=?", (variant_id,)).fetchone()
            return None if row is None else tuple(row)

    def flags(self, repo: str):
        with self.lock:
            row = self._db().execute("SELECT private,gated FROM distributions WHERE repo_id=?", (repo,)).fetchone()
            return (False, False) if row is None else (bool(row[0]), bool(row[1]))


class ModelScopeHttp:
    """HTTP transport with explicit origin permission and no ambient proxy."""
    def __init__(self, base_url: str, allowed_origins: set[str]):
        self.base_url = base_url.rstrip("/")
        self.base_origin = _profile_origin(base_url)
        self.allowed_origins = allowed_origins
        if self.base_origin not in allowed_origins:
            raise fail("FORBIDDEN", "modelscope_api_origin_not_permitted")
        self.opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), NoRedirect())

    def _check_url(self, url: str, *, redirected: bool = False) -> str:
        origin = _origin(url)
        parsed = urllib.parse.urlsplit(url)
        if origin not in self.allowed_origins:
            raise fail("FORBIDDEN", "modelscope_network_origin_not_permitted")
        if parsed.scheme != "https" and not origin.startswith(("http://127.0.0.1:", "http://localhost:", "http://[::1]:")):
            raise fail("FORBIDDEN", "modelscope_https_required")
        if not redirected:
            for key, _ in urllib.parse.parse_qsl(parsed.query, keep_blank_values=True):
                lowered = key.lower()
                if lowered in SENSITIVE_KEYS or any(part in lowered for part in ("token", "signature", "secret", "credential", "password", "cookie")):
                    raise fail("FORBIDDEN", "modelscope_sensitive_api_query_forbidden")
        return origin

    def request(self, url: str, *, method: str = "GET", headers: dict | None = None, ctx=None, limit: int = MAX_JSON_BYTES, allow_416: bool = False):
        current = url
        request_headers = dict(headers or {})
        original_origin = self._check_url(url)
        for redirects in range(MAX_REDIRECTS + 1):
            self._check_url(current, redirected=(current != url))
            if ctx is not None:
                ctx.check_cancelled()
            request = urllib.request.Request(current, headers=request_headers, method=method)
            try:
                response = self.opener.open(request, timeout=HTTP_TIMEOUT_SECONDS)
            except urllib.error.HTTPError as error:
                response = error
            except (TimeoutError, OSError, urllib.error.URLError, ValueError):
                raise self._status_error(0, {}) from None
            status = getattr(response, "status", response.code)
            response_headers = dict(response.headers.items())
            if 300 <= status < 400:
                location = response.headers.get("Location")
                response.close()
                if not location or redirects >= MAX_REDIRECTS:
                    raise fail("PROTOCOL_ERROR", "modelscope_redirect_invalid")
                target = urllib.parse.urljoin(current, location)
                new_origin = self._check_url(target, redirected=True)
                if new_origin != _origin(current):
                    request_headers = {k: v for k, v in request_headers.items() if k.lower() in {"accept", "accept-encoding", "range", "if-range"}}
                current = target
                continue
            if status == 416 and allow_416:
                response.close()
                return status, response_headers, b""
            if status >= 400:
                response.close()
                raise self._status_error(status, response_headers)
            try:
                length = response.headers.get("Content-Length")
                if method.upper() != "HEAD" and length is not None and (not length.isdecimal() or int(length) > limit):
                    raise fail("PROTOCOL_ERROR", "modelscope_response_too_large")
                body = b"" if method.upper() == "HEAD" else response.read(limit + 1)
                if len(body) > limit:
                    raise fail("PROTOCOL_ERROR", "modelscope_response_too_large")
                return status, response_headers, body
            except (TimeoutError, OSError, urllib.error.URLError):
                raise self._status_error(0, {}) from None
            finally:
                response.close()
        raise fail("PROTOCOL_ERROR", "modelscope_redirect_limit")

    def _status_error(self, status: int, headers: dict):
        if status in (401, 403):
            code, reason, retryable = "AUTH_REQUIRED", "modelscope_access_requires_authorization", False
        elif status == 404:
            code, reason, retryable = "NOT_FOUND", "modelscope_repository_or_file_not_found", False
        elif status == 429:
            code, reason, retryable = "UPSTREAM_UNAVAILABLE", "modelscope_rate_limited", True
        elif status == 408 or status == 0 or 500 <= status <= 599:
            code, reason, retryable = "UPSTREAM_UNAVAILABLE", "modelscope_upstream_unavailable", True
        elif status == 400:
            code, reason, retryable = "VALIDATION_FAILED", "modelscope_request_rejected", False
        else:
            code, reason, retryable = "PROTOCOL_ERROR", "modelscope_http_status_invalid", False
        safe_headers = {str(k).lower(): str(v)[:200] for k, v in headers.items() if str(k).lower() in {"retry-after", "ratelimit", "ratelimit-policy", "x-request-id"}}
        err = fail(code, reason, "ModelScope rechazó la solicitud", retryable=retryable)
        err.data["details"]["original_error"] = {"system": "modelscope", "code": str(status), "message": json.dumps(safe_headers, separators=(",", ":"))}
        return err


class ModelScopeAdapter:
    """P047 ModelScope Hub adapter; private auth awaits P045/P141."""
    def __init__(self, package, work, manifest, contracts):
        self.package, self.work = Path(package).resolve(strict=True), Path(work).resolve(strict=True)
        self.manifest, self.contracts = manifest, contracts
        profile_path = path_allowed(self.work / "profile.json", [self.work])
        try:
            profile = json.loads(profile_path.read_text(encoding="utf-8"), object_pairs_hook=_unique_pairs, parse_constant=_reject_constant)
        except (OSError, ValueError, json.JSONDecodeError):
            raise fail("VALIDATION_FAILED", "modelscope_profile_invalid") from None
        if set(profile) != {"source_id", "base_url"} or not isinstance(profile.get("source_id"), str) or not UUID7.fullmatch(profile["source_id"]):
            raise fail("VALIDATION_FAILED", "modelscope_profile_fields_invalid")
        if not isinstance(profile.get("base_url"), str):
            raise fail("VALIDATION_FAILED", "modelscope_base_url_invalid")
        permissions = manifest.get("permissions") or {}
        origins = permissions.get("network_origins")
        if not isinstance(origins, list):
            raise fail("FORBIDDEN", "modelscope_network_permissions_missing")
        allowed = {_profile_origin(value) for value in origins if isinstance(value, str)}
        self.http = ModelScopeHttp(profile["base_url"], allowed)
        self.source_id = profile["source_id"]
        self.identities = IdentityStore(path_allowed(self.work / "modelscope-identities.sqlite", [self.work], writing=True))
        self.search_support = {"search_applicable": True, "supported_filters": ["source.search.q"], "limitations": [
            "public_model_metadata_only_until_p045_p141_credential_resolution", "capability_filters_are_deferred_to_core",
            "upstream_pages_are_a_moving_view", "download_chunks_are_returned_to_larm_download_manager",
        ]}
        self.contracts.request("source.auth_probe", {"source_id": self.source_id, "auth_mode": "NONE", "account_id": None, "credential_reference_id": None})

    def open_session(self):
        self.identities.open()

    def shutdown(self):
        self.identities.close()

    def _check_source(self, source_id):
        if source_id is not None and source_id != self.source_id:
            raise fail("VALIDATION_FAILED", "modelscope_source_id_mismatch")

    def _url(self, path: str, query: dict | None = None):
        url = self.http.base_url + "/" + path.lstrip("/")
        return url + ("?" + urllib.parse.urlencode(query, doseq=True) if query else "")

    def _json(self, path: str, query: dict | None = None, *, legacy: bool = False, ctx=None):
        prefix = "/api/v1/" if legacy else "/openapi/v1/"
        _, headers, body = self.http.request(self._url(prefix + path, query), headers={"Accept": "application/json", "Accept-Encoding": "identity"}, ctx=ctx)
        try:
            value = json.loads(body.decode("utf-8"), object_pairs_hook=_unique_pairs, parse_constant=_reject_constant)
        except (UnicodeError, ValueError, json.JSONDecodeError):
            raise fail("PROTOCOL_ERROR", "modelscope_json_invalid") from None
        if not isinstance(value, (dict, list)):
            raise fail("PROTOCOL_ERROR", "modelscope_json_shape_invalid")
        if isinstance(value, dict):
            code = value.get("Code", value.get("code"))
            if code not in (None, 0, 200, "0", "200", "success", "Success"):
                raise fail("PROTOCOL_ERROR", "modelscope_api_business_error")
            if "Data" in value:
                value = value["Data"]
            elif "data" in value:
                value = value["data"]
        return value, headers

    @staticmethod
    def _rows(value, keys):
        if isinstance(value, list):
            return value
        if isinstance(value, dict):
            for key in keys:
                if isinstance(value.get(key), list):
                    return value[key]
            for key, member in value.items():
                if isinstance(member, list) and key.lower() not in {"tags", "branches"}:
                    return member
        return []

    @staticmethod
    def _repo_from(info: dict) -> str:
        candidates = (info.get("repo_id"), info.get("RepoId"), info.get("model_id"), info.get("ModelId"), info.get("id"), info.get("Id"), info.get("Path"), info.get("path"), info.get("Name"), info.get("name"))
        for value in candidates:
            if isinstance(value, str) and value.count("/") == 1:
                return value
        owner = info.get("owner") or info.get("Owner") or info.get("username") or info.get("Username")
        name = info.get("repo_name") or info.get("model_name") or info.get("Name") or info.get("name")
        if isinstance(owner, str) and isinstance(name, str):
            return _repo_id(owner, name)
        raise fail("PROTOCOL_ERROR", "modelscope_model_id_missing")

    def _repo_info(self, repo: str, *, ctx=None):
        namespace, name = repo.split("/", 1)
        path = "/".join(urllib.parse.quote(part, safe="") for part in ("models", namespace, name))
        value, headers = self._json(path, ctx=ctx)
        if not isinstance(value, dict):
            raise fail("PROTOCOL_ERROR", "modelscope_model_info_shape_invalid")
        actual = self._repo_from(value)
        if actual.casefold() != repo.casefold():
            raise fail("PROTOCOL_ERROR", "modelscope_model_identity_mismatch")
        private = bool(value.get("private", value.get("Private", False)))
        gated = bool(value.get("gated", value.get("Gated", value.get("ProtectedMode") == 1)))
        self.identities.distribution(repo, private, gated)
        return value, headers

    def _repo_files(self, repo: str, revision: str, *, ctx=None):
        encoded_repo = "/".join(urllib.parse.quote(part, safe="") for part in repo.split("/"))
        value, _ = self._json(f"models/{encoded_repo}/repo/files", {"Revision": revision, "Recursive": "True"}, legacy=True, ctx=ctx)
        if isinstance(value, dict):
            value = value.get("Files") or value.get("files") or []
        if not isinstance(value, list):
            raise fail("PROTOCOL_ERROR", "modelscope_file_list_shape_invalid")
        files = []
        for row in value:
            if not isinstance(row, dict):
                raise fail("PROTOCOL_ERROR", "modelscope_file_entry_invalid")
            if (row.get("Type") or row.get("type") or "blob") == "tree":
                continue
            files.append(_file(row))
        return sorted(files, key=lambda row: row["relative_path"].casefold())

    @staticmethod
    def _revision_candidates(data: object) -> list[dict]:
        if isinstance(data, dict):
            revision_map = data.get("RevisionMap") or data.get("revision_map") or data
            if isinstance(revision_map, dict):
                out = []
                for kind, keys in (("branch", ("Branches", "branches")), ("tag", ("Tags", "tags"))):
                    rows = next((revision_map.get(key) for key in keys if isinstance(revision_map.get(key), list)), [])
                    for row in rows:
                        if isinstance(row, dict):
                            out.append({"kind": kind, **row})
                if out:
                    return out
        if isinstance(data, list):
            return [row for row in data if isinstance(row, dict)]
        return []

    def _list_refs(self, repo: str, *, ctx=None):
        encoded_repo = "/".join(urllib.parse.quote(part, safe="") for part in repo.split("/"))
        data, _ = self._json(f"models/{encoded_repo}/revisions", legacy=True, ctx=ctx)
        rows = self._revision_candidates(data)
        result, seen = [], set()
        for row in rows:
            name = row.get("Revision") or row.get("revision") or row.get("Name") or row.get("name") or row.get("RevisionName") or row.get("BranchName") or row.get("TagName")
            commit = row.get("CommitId") or row.get("commit_id") or row.get("TargetCommit") or row.get("target_commit") or row.get("Sha") or row.get("sha")
            if not isinstance(name, str) or not name or len(name) > 512:
                continue
            if not isinstance(commit, str) or not COMMIT.fullmatch(commit):
                # Some API responses use Revision itself as an immutable commit.
                commit = name if COMMIT.fullmatch(name) else None
            key = (commit or name, row.get("kind", "branch"))
            if key not in seen:
                seen.add(key)
                result.append({"name": name, "commit": commit.lower() if commit else None, "kind": row.get("kind", "branch"), "raw": _redact(row)})
        if len(result) > MAX_REVISIONS:
            raise fail("UPSTREAM_UNAVAILABLE", "modelscope_revision_count_exceeds_limit")
        if not result:
            info, _ = self._repo_info(repo, ctx=ctx)
            revision = info.get("Revision") or info.get("revision") or info.get("DefaultRevision") or info.get("default_revision") or "master"
            commit = info.get("CommitId") or info.get("commit_id") or info.get("Sha") or info.get("sha")
            result = [{"name": revision if isinstance(revision, str) else "master", "commit": commit.lower() if isinstance(commit, str) and COMMIT.fullmatch(commit) else None, "kind": "branch", "raw": {"fallback": True}}]
        return result

    def _best_revision(self, info: dict, refs: list[dict]) -> dict:
        preferred = info.get("Revision") or info.get("revision") or info.get("DefaultRevision") or info.get("default_revision")
        if isinstance(preferred, str):
            match = next((row for row in refs if row["name"] == preferred or row["commit"] == preferred), None)
            if match:
                return match
        main = next((row for row in refs if row["name"].lower() in ("master", "main")), None)
        return main or refs[0]

    def _manifest(self, repo: str, key: str, *, ctx=None):
        revision, bundle_key, pinned = _revision_parts(key)
        private, gated = self.identities.flags(repo)
        if private:
            raise fail("AUTH_REQUIRED", "modelscope_private_repository_requires_credentials")
        if gated:
            raise fail("LICENSE_REQUIRED", "modelscope_gated_repository_requires_access")
        files = self._repo_files(repo, revision, ctx=ctx)
        groups = _format_groups(files)
        if bundle_key is None:
            selected = files
        else:
            group = next((candidate for candidate in groups if candidate["key"] == bundle_key), None)
            if group is None:
                raise fail("STALE_REVISION", "modelscope_bundle_not_in_revision")
            selected = group["files"]
        evidence = "modelscope:revision:" + (revision if pinned else "unverified-ref")
        artifacts = [_descriptor(row, True, evidence + ":" + row["relative_path"], estimate=True) for row in selected]
        artifact_hashes = [{"relative_path": a["relative_path"], "sha256": a["sha256"], "size_bytes": int(a["size_bytes"]) if a["size_bytes"] is not None else None, "required": a["required"]} for a in artifacts]
        digest = _digest(artifact_hashes)
        return {"revision_key": _revision_key(revision, bundle_key), "manifest_hash": digest, "artifacts": artifacts}, files, groups, pinned

    def _estimate(self, artifacts: list[dict], evidence: str) -> dict:
        sizes, unknown, exact = [], False, bool(artifacts)
        for artifact in artifacts:
            size = artifact.get("size_bytes")
            if size is None:
                estimate = artifact.get("size_estimate")
                raw = estimate.get("bytes") if isinstance(estimate, dict) and estimate.get("confidence") in ("EXACT", "HIGH") else None
                size = int(raw) if isinstance(raw, str) and raw.isdecimal() else None
            elif isinstance(size, str) and re.fullmatch(r"(?:0|[1-9][0-9]{0,18})", size):
                size = int(size)
            if size is None:
                unknown = True
            elif isinstance(size, bool) or not isinstance(size, int) or size < 0 or size > MAX_SQLITE_BYTES:
                raise fail("VALIDATION_FAILED", "modelscope_size_invalid")
            else:
                sizes.append(size)
            if artifact.get("size_bytes") is None or artifact.get("sha256") is None:
                exact = False
        total = sum(sizes)
        if total > MAX_SQLITE_BYTES:
            raise fail("VALIDATION_FAILED", "modelscope_size_overflow")
        if not artifacts:
            unknown = True
        value = str(total) if not unknown else None
        confidence = "UNKNOWN" if unknown else "EXACT" if exact else "HIGH"
        return {"download_bytes": value, "final_artifact_bytes": value, "installed_bytes": value,
                "derived_cache_bytes": "0", "temporary_peak_bytes": None, "existing_bytes": "0",
                "peak_extra_bytes": None, "required_free_bytes": None, "upper_bound_bytes": value,
                "confidence": confidence, "reason": "Estimación del manifiesto ModelScope; Core calcula el destino y temporales.",
                "evidence_refs": [evidence], "per_volume": []}

    def _variant_rows(self, info: dict, *, ctx=None) -> tuple[str, list[dict], dict, bool]:
        repo = self._repo_from(info)
        private = bool(info.get("private", info.get("Private", False)))
        gated = bool(info.get("gated", info.get("Gated", info.get("ProtectedMode") == 1)))
        distribution_id, distribution_created = self.identities.distribution(repo, private, gated)
        refs = self._list_refs(repo, ctx=ctx)
        selected_revision = self._best_revision(info, refs)
        revision = selected_revision["commit"] or selected_revision["name"]
        files = self._repo_files(repo, revision, ctx=ctx)
        groups = _format_groups(files)
        if not groups:
            groups = [{"key": "metadata:unclassified", "format": None, "files": []}]
        created = _stamp(info.get("CreatedAt") or info.get("created_at") or info.get("CreatedDate"), distribution_created)
        updated = _stamp(info.get("LastModified") or info.get("last_modified") or info.get("UpdatedAt"), _now())
        architecture = info.get("ModelType") or info.get("model_type") or info.get("ModelTypes") or info.get("architecture")
        if isinstance(architecture, list):
            architecture = architecture[0] if architecture and isinstance(architecture[0], str) else None
        if not isinstance(architecture, str):
            architecture = None
        limits = _model_limits(info)
        safe = _redact(info)
        if not isinstance(safe, dict):
            safe = {}
        variants = []
        for group in groups:
            variant_id, variant_created = self.identities.variant(repo, group["key"])
            manifest, _, _, pinned = self._manifest(repo, _revision_key(revision, group["key"]), ctx=ctx)
            revision_id, revision_created = self.identities.revision(repo, group["key"], manifest["revision_key"], manifest["manifest_hash"])
            source_meta = {"original": safe, "normalized": {
                "source_type": "MODELSCOPE", "source_id": self.source_id, "repository_id": repo,
                "namespace": repo.split("/", 1)[0], "bundle_key": group["key"],
                "revision_key": manifest["revision_key"], "commit": selected_revision["commit"],
                "catalog_reference": {
                    "source_id": self.source_id, "upstream_namespace": repo.split("/", 1)[0],
                    "upstream_model_id": repo, "revision_key": manifest["revision_key"],
                },
                "ref_name": selected_revision["name"], "revision_pinned": pinned,
                "private": private, "gated": gated, "license": _license(info),
                "upstream_distribution_id": distribution_id,
            }, "limitations": self.search_support["limitations"],
            "rate_limit": {"limit": None, "remaining": None, "retry_after_seconds": None, "reset_at": None}}
            variants.append({"id": variant_id, "created_at": created,
                "updated_at": updated, "row_version": 1,
                "metadata": {"display_name": info.get("Name") or info.get("name") or repo, "source_v1": source_meta},
                "revision_id": revision_id, "format": group["format"], "quantization": None,
                "architecture": architecture[:256] if architecture else None, "bundle_hash": None,
                "lifecycle": "DISCOVERED", "availability": "UNKNOWN", "ownership": "EXTERNAL",
                "promotion": "NONE", "system_role": "GENERAL", "capabilities": _capabilities(info),
                "model_limits": limits, "system1_state": None, "update_available": False, "status": "DISCOVERED"})
        return repo, variants, info, bool(selected_revision["commit"])

    def _search(self, request: dict, ctx=None):
        if request["capabilities"]:
            raise fail("UNSUPPORTED_CAPABILITY", "modelscope_capability_filter_deferred_to_core")
        query_hash = _digest({"source_id": self.source_id, "q": request["q"], "capabilities": [], "limit": request["limit"]})
        page, snapshot = 1, _now()
        if request["cursor"] is not None:
            if len(request["cursor"]) > 8192:
                raise fail("VALIDATION_FAILED", "modelscope_cursor_too_large")
            try:
                raw = base64.urlsafe_b64decode(request["cursor"] + "=" * (-len(request["cursor"]) % 4))
                cursor = json.loads(raw.decode("utf-8"), object_pairs_hook=_unique_pairs, parse_constant=_reject_constant)
            except (ValueError, UnicodeError, json.JSONDecodeError):
                raise fail("VALIDATION_FAILED", "modelscope_cursor_invalid") from None
            if set(cursor) != {"query_hash", "page", "snapshot_at"} or cursor["query_hash"] != query_hash or not isinstance(cursor["page"], int) or not isinstance(cursor["snapshot_at"], str):
                raise fail("VALIDATION_FAILED", "modelscope_cursor_query_mismatch")
            page, snapshot = cursor["page"], cursor["snapshot_at"]
        limit = request["limit"]
        page_size = min(limit, 50)
        payload, headers = self._json("models", {"search": request["q"], "page_number": page, "page_size": page_size}, ctx=ctx)
        rows = self._rows(payload, ("models", "items", "list", "results", "data"))
        if not all(isinstance(row, dict) for row in rows):
            raise fail("PROTOCOL_ERROR", "modelscope_search_entry_invalid")
        output = []
        for row in rows:
            if ctx is not None:
                ctx.check_cancelled()
            try:
                repo = self._repo_from(row)
                info, _ = self._repo_info(repo, ctx=ctx)
                _repo, variants, _info, _pinned = self._variant_rows(info, ctx=ctx)
                output.extend(variants)
            except RpcError as error:
                if error.data.get("code") in ("AUTH_REQUIRED", "LICENSE_REQUIRED"):
                    continue
                raise
        total = payload.get("total_count", payload.get("TotalCount", payload.get("total", payload.get("Total")))) if isinstance(payload, dict) else None
        has_more = len(rows) >= page_size and (not isinstance(total, int) or page * page_size < total)
        next_cursor = None
        if has_more:
            next_cursor = base64.urlsafe_b64encode(_canonical({"query_hash": query_hash, "page": page + 1, "snapshot_at": snapshot})).decode("ascii").rstrip("=")
        response = {"items": output, "next_cursor": next_cursor, "snapshot_at": snapshot}
        self.contracts.response("source.search", response)
        return response

    def _get_model(self, request: dict, *, ctx=None):
        repo = _repo_id(request["upstream_namespace"], request["upstream_model_id"])
        info, _ = self._repo_info(repo, ctx=ctx)
        revision_rows = self._list_refs(repo, ctx=ctx)
        selected = self._best_revision(info, revision_rows)
        files = self._repo_files(repo, selected["commit"] or selected["name"], ctx=ctx)
        groups = _format_groups(files)
        artifacts = [_descriptor(item, required=(len(groups) <= 1), evidence="modelscope:model-info:" + repo + ":" + item["relative_path"], estimate=False) for item in files]
        architecture = info.get("ModelType") or info.get("model_type") or info.get("ModelTypes")
        if isinstance(architecture, list):
            architecture = architecture[0] if architecture and isinstance(architecture[0], str) else None
        if not isinstance(architecture, str):
            architecture = None
        safe = _redact(info)
        inspection = {"upstream_identity": "modelscope:model:" + repo, "architecture": architecture,
            "format": groups[0]["format"] if len(groups) == 1 else None, "quantization": None,
            "capabilities": _capabilities(info), "limits": _model_limits(info) | {"source_v1": {
                "source_type": "MODELSCOPE", "repository_id": repo, "private": bool(info.get("private", info.get("Private", False))),
                "gated": bool(info.get("gated", info.get("Gated", info.get("ProtectedMode") == 1))),
                "commit": selected["commit"], "ref_name": selected["name"], "revision_pinned": bool(selected["commit"]),
                "raw": safe, "limitations": self.search_support["limitations"]}},
            "artifacts": artifacts, "license": _license(info), "evidence_refs": ["modelscope:model-info:" + repo + (":" + selected["commit"] if selected["commit"] else ":ref:" + selected["name"])]}
        self.contracts.response("source.get_model", inspection)
        return inspection

    def _list_revisions(self, request: dict, *, ctx=None):
        repo = _repo_id(request["upstream_namespace"], request["upstream_model_id"])
        info, _ = self._repo_info(repo, ctx=ctx)
        private = bool(info.get("private", info.get("Private", False)))
        gated = bool(info.get("gated", info.get("Gated", info.get("ProtectedMode") == 1)))
        distribution_id, _ = self.identities.distribution(repo, private, gated)
        rows, seen = [], set()
        for ref in self._list_refs(repo, ctx=ctx):
            if ctx is not None:
                ctx.check_cancelled()
            key = _revision_key(ref["commit"] or ref["name"])
            manifest, files, groups, pinned = self._manifest(repo, key, ctx=ctx)
            if private or gated:
                continue
            for group in groups:
                bundle_key = group["key"]
                manifest_group, _, _, _ = self._manifest(repo, _revision_key(ref["commit"] or ref["name"], bundle_key), ctx=ctx)
                revision_id, created = self.identities.revision(repo, bundle_key, manifest_group["revision_key"], manifest_group["manifest_hash"])
                identity_key = (revision_id, bundle_key)
                if identity_key in seen:
                    continue
                seen.add(identity_key)
                rows.append({"id": revision_id, "created_at": created, "updated_at": _now(), "row_version": 1,
                    "metadata": {"source_v1": {"ref_name": ref["name"], "ref_kind": ref["kind"], "bundle_key": bundle_key,
                        "source_type": "MODELSCOPE", "revision_pinned": pinned, "raw": ref["raw"]}},
                    "distribution_id": distribution_id, "revision_key": manifest_group["revision_key"],
                    "upstream_commit": ref["commit"], "manifest_hash": manifest_group["manifest_hash"], "state": "CANDIDATE"})
        response = {"items": rows, "next_cursor": None, "snapshot_at": _now()}
        self.contracts.response("source.list_revisions", response)
        return response

    def _revision_manifest(self, request: dict, *, ctx=None):
        repo = _repo_id(request["upstream_namespace"], request["upstream_model_id"])
        manifest, _, _, pinned = self._manifest(repo, request["revision_key"], ctx=ctx)
        if not pinned:
            # A movable branch/tag may be inspected, but cannot be resumed or downloaded.
            for artifact in manifest["artifacts"]:
                artifact["size_estimate"]["reason"] = "La revisión es un alias móvil; el tamaño publicado no fija un commit descargable."
        return repo, manifest, pinned

    def _download_plan(self, request: dict, *, ctx=None):
        self._check_source(request.get("source_id"))
        if request.get("storage_location_id") is None:
            raise fail("UNSUPPORTED_CAPABILITY", "modelscope_destination_preflight_owned_by_core")
        acquisition = validate_source_acquisition(request)
        if acquisition is not None:
            revision = dict(acquisition["revision"])
            repo = _repo_id(revision["upstream_namespace"], revision["upstream_model_id"])
            self._repo_info(repo, ctx=ctx)  # Refresh private/license flags before the pinned manifest.
        else:
            if request.get("optional_artifact_ids"):
                raise fail("UNSUPPORTED_CAPABILITY", "modelscope_optional_artifact_binding_owned_by_core")
            context = self.identities.variant_context(request["variant_id"])
            if context is None:
                raise fail("NOT_FOUND", "modelscope_variant_unknown")
            repo, bundle = context
            private, gated = self.identities.flags(repo)
            if private:
                raise fail("AUTH_REQUIRED", "modelscope_private_repository_requires_credentials")
            if gated:
                raise fail("LICENSE_REQUIRED", "modelscope_gated_repository_requires_access")
            info, _ = self._repo_info(repo, ctx=ctx)
            refs = self._list_refs(repo, ctx=ctx)
            selected = self._best_revision(info, refs)
            if not selected["commit"]:
                raise fail("STALE_REVISION", "modelscope_revision_not_pinned_for_download")
            revision = {"source_id": self.source_id, "upstream_namespace": repo.split("/", 1)[0], "upstream_model_id": repo,
                        "revision_key": _revision_key(selected["commit"], bundle)}
        _repo, manifest, pinned = self._revision_manifest(revision, ctx=ctx)
        if not pinned or not manifest["artifacts"]:
            raise fail("UNSUPPORTED_CAPABILITY", "modelscope_bundle_not_downloadable")
        artifacts = select_source_acquisition(request, manifest) if acquisition is not None else manifest["artifacts"]
        estimate = self._estimate(artifacts, "modelscope:manifest:" + manifest["manifest_hash"])
        spec = {"variant_id": request["variant_id"], "storage_location_id": request["storage_location_id"],
            "revision": revision, "artifacts": artifacts,
            "auth": {"authenticated": False, "auth_required": False, "license_required": False, "reason": "modelscope_public_repository"},
            "resumable": True, "priority": request["priority"], "hard_budget_bytes": request["hard_budget_bytes"]}
        plan = {"plan_id": str(uuid.uuid7()), "plan_hash": "0" * 64, "operation": "source.download",
            "resource_ids": [request["variant_id"], request["storage_location_id"]], "storage_estimate": estimate,
            "steps": [{"phase": "DOWNLOAD", "adapter_id": self.manifest["id"], "request": source_acquisition_payload(request, spec)}],
            "expires_at": (datetime.now(timezone.utc) + timedelta(seconds=900)).isoformat(timespec="milliseconds").replace("+00:00", "Z"),
            "approval_required": False}
        plan["plan_hash"] = _digest({key: value for key, value in plan.items() if key != "plan_hash"})
        self.contracts.response("source.download_plan", plan)
        return plan

    def _download_chunk(self, request: dict, *, ctx=None):
        self._check_source(request["source_id"])
        revision = request["revision"]
        repo = _repo_id(revision["upstream_namespace"], revision["upstream_model_id"])
        key_revision, bundle, pinned = _revision_parts(revision["revision_key"])
        if not pinned:
            raise fail("STALE_REVISION", "modelscope_download_requires_pinned_commit")
        manifest, _, _, _ = self._manifest(repo, revision["revision_key"], ctx=ctx)
        artifact = next((row for row in manifest["artifacts"] if row["relative_path"] == request["relative_path"]), None)
        if artifact is None:
            raise fail("VALIDATION_FAILED", "modelscope_file_not_in_pinned_bundle")
        offset, max_bytes = int(request["offset"]), int(request["max_bytes"])
        if max_bytes < 1 or max_bytes > CHUNK_MAX_BYTES or offset > MAX_SQLITE_BYTES - max_bytes:
            raise fail("VALIDATION_FAILED", "modelscope_chunk_range_invalid")
        expected_size = int(artifact["size_bytes"]) if artifact["size_bytes"] is not None else None
        if expected_size is not None and offset > expected_size:
            raise fail("VALIDATION_FAILED", "modelscope_offset_past_eof")
        validator_request = request["validator"]
        if offset > 0 and validator_request is None:
            raise fail("CONFLICT", "modelscope_resume_validator_missing")
        if validator_request and validator_request["kind"] == "LAST_MODIFIED_AND_SHA256" and artifact["sha256"] != validator_request["sha256"]:
            raise fail("CONFLICT", "modelscope_resume_checksum_changed")
        query = {"Revision": key_revision, "FilePath": _relative(request["relative_path"])}
        url = self._url("/api/v1/models/" + "/".join(urllib.parse.quote(part, safe="") for part in repo.split("/")) + "/repo", query)
        headers = {"Accept": "application/octet-stream", "Accept-Encoding": "identity", "Range": f"bytes={offset}-{offset + max_bytes - 1}"}
        if validator_request:
            headers["If-Range"] = validator_request.get("value") or validator_request.get("last_modified")
        status, response_headers, body = self.http.request(url, headers=headers, ctx=ctx, limit=max_bytes, allow_416=True)
        lower = {str(key).lower(): str(value) for key, value in response_headers.items()}
        if lower.get("content-encoding", "identity").lower() != "identity":
            raise fail("PROTOCOL_ERROR", "modelscope_content_encoding_not_identity")
        total, data = expected_size, body
        if status == 416:
            if expected_size is not None and offset == expected_size:
                result = {"offset": str(offset), "data_base64": "", "eof": True, "total_bytes": str(offset), "validator": validator_request}
                self.contracts.response("source.download_chunk", result)
                return result
            raise fail("CONFLICT", "modelscope_range_not_satisfiable")
        if status == 206:
            match = CONTENT_RANGE.fullmatch(lower.get("content-range", ""))
            if not match or int(match.group(1)) != offset or int(match.group(2)) - offset + 1 != len(body):
                raise fail("PROTOCOL_ERROR", "modelscope_content_range_invalid")
            total = int(match.group(3)) if match.group(3) != "*" else total
        elif status == 200:
            if offset:
                raise fail("CONFLICT", "modelscope_server_ignored_range_restart_required")
            content_length = lower.get("content-length")
            if content_length and content_length.isdecimal() and int(content_length) <= MAX_SQLITE_BYTES:
                total = int(content_length)
            if len(body) > max_bytes:
                raise fail("PROTOCOL_ERROR", "modelscope_chunk_exceeds_limit")
        else:
            raise fail("PROTOCOL_ERROR", "modelscope_download_status_invalid")
        if expected_size is not None and total is not None and expected_size != total:
            raise fail("INTEGRITY_FAILED", "modelscope_size_changed_for_pinned_revision")
        etag = lower.get("etag")
        modified = lower.get("last-modified")
        validator = {"kind": "STRONG_ETAG", "value": etag} if etag and ETAG.fullmatch(etag) else ({"kind": "LAST_MODIFIED_AND_SHA256", "last_modified": modified, "sha256": artifact["sha256"]} if modified and artifact["sha256"] else None)
        if validator_request is not None and validator != validator_request:
            raise fail("CONFLICT", "modelscope_resume_validator_changed")
        eof = total is not None and offset + len(data) == total
        if status == 200 and total is None and len(data) < max_bytes:
            total, eof = len(data), True
        if status == 206 and total is None and len(data) < max_bytes:
            total, eof = offset + len(data), True
        if not data and not eof:
            raise fail("UPSTREAM_UNAVAILABLE", "modelscope_empty_nonterminal_chunk", retryable=True)
        result = {"offset": str(offset), "data_base64": base64.b64encode(data).decode("ascii"), "eof": bool(eof),
                  "total_bytes": str(total) if total is not None else None, "validator": validator}
        self.contracts.response("source.download_chunk", result)
        return result

    def _sync(self, request: dict, *, ctx=None):
        query = request["query"]
        if not isinstance(query, dict) or set(query) - {"q", "capabilities"}:
            raise fail("UNSUPPORTED_CAPABILITY", "modelscope_sync_query_unsupported")
        page = self._search({"source_id": self.source_id, "q": query.get("q", ""), "capabilities": query.get("capabilities", []), "cursor": request["cursor"], "limit": 50}, ctx)
        digest = _digest([{"id": item["id"], "revision_id": item["revision_id"], "metadata": item["metadata"]["source_v1"]["normalized"]} for item in page["items"]])
        changed = request["etag"] != digest
        models = []
        if changed:
            seen = set()
            for item in page["items"]:
                normalized = item["metadata"]["source_v1"]["normalized"]
                repo = normalized["repository_id"]
                if repo in seen:
                    continue
                seen.add(repo)
                models.append(self._get_model({"source_id": self.source_id, "upstream_namespace": repo.split("/", 1)[0], "upstream_model_id": repo}, ctx=ctx))
        result = {"cursor": page["next_cursor"], "etag": digest, "changed": changed, "models": models}
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
                result = self._get_model(params, ctx=ctx)
                return result
            if method == "source.list_revisions":
                return self._list_revisions(params, ctx=ctx)
            if method == "source.get_manifest":
                repo, manifest, _pinned = self._revision_manifest(params, ctx=ctx)
                self.contracts.response("source.get_manifest", manifest)
                return manifest
            if method == "source.estimate_size":
                _repo, manifest, _pinned = self._revision_manifest(params, ctx=ctx)
                result = self._estimate(manifest["artifacts"], "modelscope:manifest:" + manifest["manifest_hash"])
                self.contracts.response("source.estimate_size", result)
                return result
            if method == "source.download_plan":
                return self._download_plan(params, ctx=ctx)
            if method == "source.download":
                raise fail("UNSUPPORTED_CAPABILITY", "modelscope_download_execution_owned_by_download_manager")
            if method == "source.download_chunk":
                return self._download_chunk(params, ctx=ctx)
            if method == "source.sync":
                return self._sync(params, ctx=ctx)
            if method == "source.auth_probe":
                if params["auth_mode"] == "NONE" and params["account_id"] is None and params["credential_reference_id"] is None:
                    result = {"authenticated": False, "auth_required": False, "license_required": False, "reason": "modelscope_anonymous_public_metadata"}
                else:
                    result = {"authenticated": False, "auth_required": True, "license_required": False, "reason": "modelscope_credentials_pending_p045_p141"}
                self.contracts.response("source.auth_probe", result)
                return result
            raise fail("UNSUPPORTED_CAPABILITY", "modelscope_method_not_supported")
        except ContractViolation:
            raise fail("VALIDATION_FAILED", "modelscope_contract_document_invalid") from None
        except sqlite3.Error:
            raise fail("RESOURCE_UNAVAILABLE", "modelscope_identity_store_failed", retryable=True) from None
        except RpcError:
            raise
        except (OSError, ValueError, TypeError, KeyError):
            raise fail("RESOURCE_UNAVAILABLE", "modelscope_adapter_io_failed", retryable=True) from None
