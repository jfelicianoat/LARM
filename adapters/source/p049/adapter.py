"""P049 source workers with capability support fixed per provider profile."""
from __future__ import annotations

import hashlib
import json
import re
import sqlite3
import threading
import urllib.error
import urllib.parse
import urllib.request
import uuid
from contextlib import contextmanager
from datetime import datetime, timezone
from decimal import Decimal, InvalidOperation, ROUND_CEILING, ROUND_FLOOR
from pathlib import Path, PurePosixPath

from larm_adapter_sdk.access import NoRedirect, fail

MAX_JSON_BYTES = 8 << 20
HTTP_TIMEOUT_SECONDS = 12
SOURCE_ORIGIN = "https://civitai.com:443"
UUID7 = re.compile(r"^[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$")
SHA256 = re.compile(r"^[0-9a-f]{64}$")


def _unique_pairs(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError("duplicate JSON key")
        result[key] = value
    return result


def _now() -> str:
    return datetime.now(timezone.utc).isoformat(timespec="milliseconds").replace("+00:00", "Z")


def _stamp(value, fallback=None):
    if not isinstance(value, str) or not value:
        return fallback or _now()
    try:
        parsed = datetime.fromisoformat(value.replace("Z", "+00:00"))
        if parsed.tzinfo is None:
            return fallback or _now()
        return parsed.astimezone(timezone.utc).isoformat(timespec="milliseconds").replace("+00:00", "Z")
    except ValueError:
        return fallback or _now()


def _sha256(value):
    if not isinstance(value, str):
        return None
    value = value.lower()
    return value if SHA256.fullmatch(value) else None


def _model_id(value):
    if isinstance(value, bool):
        return None
    if isinstance(value, int) and value > 0:
        return str(value)
    if isinstance(value, str) and value.isascii() and value.isdecimal() and int(value) > 0:
        return value
    return None


def _safe_filename(value):
    if not isinstance(value, str) or not value or len(value) > 240:
        return None
    if value in (".", "..") or "/" in value or "\\" in value or any(ord(char) < 32 for char in value):
        return None
    return value


def _file_size_estimate(version, entry, name):
    """Keep Civitai's rounded KiB metadata visibly estimated, never exact."""
    version_id = _model_id(version.get("id")) if isinstance(version, dict) else None
    evidence = "civitai:model-version:" + str(version_id or "unknown") + ":" + name
    try:
        raw = entry.get("sizeKB")
        if isinstance(raw, bool) or raw is None:
            raise InvalidOperation
        size_kib = Decimal(str(raw))
        if not size_kib.is_finite() or size_kib <= 0:
            raise InvalidOperation
        lower = max(0, int((size_kib * 1024 - 512).to_integral_value(rounding=ROUND_FLOOR)))
        upper = int((size_kib * 1024 + 512).to_integral_value(rounding=ROUND_CEILING))
        if upper > 9_223_372_036_854_775_807:
            raise InvalidOperation
        return {
            "confidence": "ESTIMATED", "bytes": None,
            "lower_bound_bytes": str(lower), "upper_bound_bytes": str(upper),
            "source": "SOURCE_METADATA", "evidence_refs": [evidence],
            "reason": "Civitai publica sizeKB; se convierte desde KiB y se conserva un margen de ±512 bytes.",
        }
    except (InvalidOperation, ValueError, TypeError, OverflowError):
        return {
            "confidence": "UNKNOWN", "bytes": None,
            "lower_bound_bytes": None, "upper_bound_bytes": None,
            "source": "UNKNOWN", "evidence_refs": [evidence],
            "reason": "La API no publicó un sizeKB válido para este archivo.",
        }


def _version_key(version_id):
    value = _model_id(version_id)
    if value is None:
        raise fail("PROTOCOL_ERROR", "civitai_version_id_invalid")
    return "version:" + value


def _version_id(revision_key):
    if not isinstance(revision_key, str) or not revision_key.startswith("version:"):
        raise fail("VALIDATION_FAILED", "civitai_revision_key_invalid")
    value = _model_id(revision_key.removeprefix("version:"))
    if value is None:
        raise fail("VALIDATION_FAILED", "civitai_revision_key_invalid")
    return value


def _version_files(version):
    files = version.get("files") if isinstance(version, dict) else None
    if not isinstance(files, list):
        return []
    rows = []
    for entry in files[:512]:
        if not isinstance(entry, dict):
            continue
        name = _safe_filename(entry.get("name"))
        if name is None:
            continue
        file_type = entry.get("type") if isinstance(entry.get("type"), str) else None
        hashes = entry.get("hashes") if isinstance(entry.get("hashes"), dict) else {}
        primary = entry.get("primary") if isinstance(entry.get("primary"), bool) else False
        rows.append({
            "relative_path": "files/" + name,
            "size_bytes": None,
            "sha256": _sha256(hashes.get("SHA256") or hashes.get("sha256")),
            "required": primary,
            "role": "weights" if file_type == "Model" else "other",
            "size_estimate": _file_size_estimate(version, entry, name),
        })
    rows.sort(key=lambda item: item["relative_path"].casefold())
    return rows


def _manifest_hash(version, files):
    basis = {
        "version_id": _model_id(version.get("id")) if isinstance(version, dict) else None,
        "artifacts": files,
    }
    encoded = json.dumps(basis, ensure_ascii=False, sort_keys=True, separators=(",", ":")).encode("utf-8")
    return hashlib.sha256(encoded).hexdigest()


class IdentityStore:
    """Stable core-owned UUIDv7 identities for repeated catalog pages."""

    def __init__(self, path: Path):
        self.path = path
        self.lock = threading.RLock()
        with self._db() as connection:
            connection.execute("CREATE TABLE IF NOT EXISTS identities (key TEXT PRIMARY KEY, value TEXT NOT NULL UNIQUE)")

    @contextmanager
    def _db(self):
        connection = sqlite3.connect(self.path, timeout=5)
        try:
            connection.execute("PRAGMA journal_mode=WAL")
            connection.execute("PRAGMA busy_timeout=5000")
            with connection:
                yield connection
        finally:
            connection.close()

    def get(self, key: str) -> str:
        with self.lock, self._db() as connection:
            row = connection.execute("SELECT value FROM identities WHERE key=?", (key,)).fetchone()
            if row:
                return row[0]
            value = str(uuid.uuid7())
            connection.execute("INSERT INTO identities(key,value) VALUES(?,?)", (key, value))
            return value


class CivitaiAdapter:
    """Anonymous, metadata-only use of Civitai Site API v1."""

    def __init__(self, package: Path, work: Path, manifest: dict, contracts):
        self.package = package.resolve(strict=True)
        self.work = work.resolve(strict=True)
        self.manifest = manifest
        self.contracts = contracts
        profile_path = self.work / "profile.json"
        try:
            profile = json.loads(profile_path.read_text(encoding="utf-8"), object_pairs_hook=_unique_pairs)
        except (OSError, ValueError, json.JSONDecodeError):
            raise fail("VALIDATION_FAILED", "civitai_profile_invalid") from None
        if set(profile) != {"provider", "source_id"} or profile.get("provider") != "CIVITAI":
            raise fail("VALIDATION_FAILED", "civitai_profile_fields_invalid")
        if not isinstance(profile["source_id"], str) or not UUID7.fullmatch(profile["source_id"]):
            raise fail("VALIDATION_FAILED", "civitai_source_id_invalid")
        origins = (manifest.get("permissions") or {}).get("network_origins")
        if origins != [SOURCE_ORIGIN]:
            raise fail("FORBIDDEN", "civitai_network_origin_not_permitted")
        self.source_id = profile["source_id"]
        self.base_url = "https://civitai.com/api/v1"
        self.opener = urllib.request.build_opener(NoRedirect())
        self.identities = IdentityStore(self.work / "civitai-identities.sqlite")
        self.search_support = {
            "search_applicable": True,
            "supported_filters": ["source.search.q"],
            "limitations": [
                "civitai_public_metadata_only",
                "nsfw_filter_requested_false; region restrictions may further filter results",
                "download_requires_provider_credentials_and_is_not_enabled_until_p045",
                "file_size_is_unknown_until_an_exact_byte_count_is_available",
            ],
        }

    def shutdown(self):
        return None

    def _check_source(self, value):
        if value is not None and value != self.source_id:
            raise fail("VALIDATION_FAILED", "civitai_source_id_mismatch")

    def _api_json(self, path: str, params: dict, ctx):
        if not path.startswith("/") or ".." in path.split("/"):
            raise fail("FORBIDDEN", "civitai_api_path_invalid")
        query = urllib.parse.urlencode(params)
        url = self.base_url + path + ("?" + query if query else "")
        ctx.check_cancelled()
        request = urllib.request.Request(url, headers={"Accept": "application/json", "Accept-Encoding": "identity", "User-Agent": "LARM/1.0"})
        try:
            response = self.opener.open(request, timeout=HTTP_TIMEOUT_SECONDS)
            with response:
                status = getattr(response, "status", response.code)
                headers = response.headers
                body = response.read(MAX_JSON_BYTES + 1)
        except urllib.error.HTTPError as error:
            status = error.code
            headers = error.headers
            body = error.read(MAX_JSON_BYTES + 1)
        except (TimeoutError, OSError, urllib.error.URLError, ValueError):
            raise fail("RESOURCE_UNAVAILABLE", "civitai_upstream_unavailable", retryable=True) from None
        ctx.check_cancelled()
        if len(body) > MAX_JSON_BYTES:
            raise fail("PROTOCOL_ERROR", "civitai_response_too_large")
        if status == 429:
            raise fail("RESOURCE_UNAVAILABLE", "civitai_rate_limited", retryable=True)
        if status in (401, 403):
            raise fail("AUTH_REQUIRED", "civitai_public_metadata_access_denied")
        if status == 404:
            raise fail("NOT_FOUND", "civitai_model_not_found")
        if status >= 500:
            raise fail("RESOURCE_UNAVAILABLE", "civitai_upstream_server_error", retryable=True)
        if status < 200 or status >= 300:
            raise fail("VALIDATION_FAILED", "civitai_request_rejected")
        try:
            document = json.loads(body.decode("utf-8"), object_pairs_hook=_unique_pairs, parse_constant=lambda _value: (_ for _ in ()).throw(ValueError("non-finite JSON")))
        except (UnicodeError, ValueError, json.JSONDecodeError):
            raise fail("PROTOCOL_ERROR", "civitai_json_invalid") from None
        if not isinstance(document, dict):
            raise fail("PROTOCOL_ERROR", "civitai_json_shape_invalid")
        return document, headers

    def _cursor(self, value, query):
        if value is None:
            return None
        if not isinstance(value, str) or len(value) > 8192:
            raise fail("VALIDATION_FAILED", "civitai_cursor_invalid")
        try:
            import base64
            raw = base64.urlsafe_b64decode(value + "=" * (-len(value) % 4))
            parsed = json.loads(raw.decode("utf-8"), object_pairs_hook=_unique_pairs)
        except (ValueError, UnicodeError, json.JSONDecodeError):
            raise fail("VALIDATION_FAILED", "civitai_cursor_invalid") from None
        if not isinstance(parsed, dict) or set(parsed) != {"query", "cursor"} or parsed["query"] != query or not isinstance(parsed["cursor"], str) or not parsed["cursor"]:
            raise fail("VALIDATION_FAILED", "civitai_cursor_query_mismatch")
        return parsed["cursor"]

    @staticmethod
    def _encode_cursor(query, cursor):
        import base64
        value = json.dumps({"query": query, "cursor": cursor}, ensure_ascii=False, sort_keys=True, separators=(",", ":")).encode("utf-8")
        return base64.urlsafe_b64encode(value).decode("ascii").rstrip("=")

    @staticmethod
    def _versions(model):
        value = model.get("modelVersions") if isinstance(model, dict) else None
        return [item for item in value if isinstance(item, dict) and _model_id(item.get("id"))] if isinstance(value, list) else []

    @staticmethod
    def _latest(model):
        versions = CivitaiAdapter._versions(model)
        return versions[0] if versions else None

    def _variant(self, model, version):
        model_id, version_id = _model_id(model.get("id")), _model_id(version.get("id"))
        if model_id is None or version_id is None:
            return None
        distribution_id = self.identities.get("distribution:" + model_id)
        revision_id = self.identities.get("revision:" + model_id + ":" + version_id)
        variant_id = self.identities.get("variant:" + model_id + ":" + version_id)
        files = _version_files(version)
        formats = sorted({PurePosixPath(item["relative_path"]).suffix.lstrip(".").lower() for item in files if PurePosixPath(item["relative_path"]).suffix})
        creator = model.get("creator") if isinstance(model.get("creator"), dict) else {}
        license_name = version.get("license") if isinstance(version.get("license"), str) else model.get("license")
        metadata = {
            "display_name": model.get("name") if isinstance(model.get("name"), str) else "Civitai model " + model_id,
            "source_v1": {
                "source_type": "CIVITAI",
                "source_id": self.source_id,
                "model_id": model_id,
                "version_id": version_id,
                "creator": creator.get("username") if isinstance(creator.get("username"), str) else None,
                "base_model": version.get("baseModel") if isinstance(version.get("baseModel"), str) else None,
                "license": license_name if isinstance(license_name, str) else None,
                "nsfw": model.get("nsfw") if isinstance(model.get("nsfw"), bool) else None,
                "nsfw_level": model.get("nsfwLevel") if isinstance(model.get("nsfwLevel"), int) and not isinstance(model.get("nsfwLevel"), bool) else None,
                "tags": [tag for tag in model.get("tags", []) if isinstance(tag, str)][:256] if isinstance(model.get("tags"), list) else [],
                "api_evidence": "https://civitai.com/api/v1/models/" + model_id,
            },
        }
        return {
            "id": variant_id,
            "created_at": _stamp(version.get("createdAt")),
            "updated_at": _stamp(version.get("updatedAt") or model.get("updatedAt")),
            "row_version": 1,
            "metadata": metadata,
            "revision_id": revision_id,
            "format": formats[0] if len(formats) == 1 else None,
            "quantization": None,
            "architecture": None,
            "bundle_hash": None,
            "lifecycle": "DISCOVERED",
            "availability": "UNKNOWN",
            "ownership": "EXTERNAL",
            "promotion": "NONE",
            "system_role": "GENERAL",
            "capabilities": [],
            "model_limits": {
                "parameter_count": None, "layers": None, "kv_heads": None, "head_dim": None,
                "context_limit": None, "kv_bits": None, "model_max_memory_bytes": None,
                "architecture_metadata_hash": None,
            },
            "system1_state": None,
            "update_available": False,
            "status": "DISCOVERED",
            "_distribution_id": distribution_id,
        }

    def _model(self, model_id, ctx):
        document, headers = self._api_json("/models/" + urllib.parse.quote(model_id, safe=""), {}, ctx)
        if _model_id(document.get("id")) != model_id:
            raise fail("PROTOCOL_ERROR", "civitai_model_identity_mismatch")
        return document, headers

    def _inspect(self, model, ctx):
        model_id = _model_id(model.get("id"))
        latest = self._latest(model)
        files = _version_files(latest) if latest else []
        artifacts = [{key: item[key] for key in ("relative_path", "size_bytes", "sha256", "required", "role")} for item in files]
        version_id = _model_id(latest.get("id")) if latest else None
        version_license = latest.get("license") if isinstance(latest, dict) else None
        license_name = version_license if isinstance(version_license, str) else model.get("license")
        refs = ["civitai:model:" + model_id]
        if version_id:
            refs.append("civitai:model-version:" + version_id)
        return {
            "upstream_identity": "civitai:model:" + model_id,
            "architecture": None,
            "format": None,
            "quantization": None,
            "capabilities": [],
            "limits": {
                "source_type": "CIVITAI",
                "model_id": model_id,
                "version_id": version_id,
                "nsfw": model.get("nsfw") if isinstance(model.get("nsfw"), bool) else None,
                "nsfw_level": model.get("nsfwLevel") if isinstance(model.get("nsfwLevel"), int) and not isinstance(model.get("nsfwLevel"), bool) else None,
                "base_model": latest.get("baseModel") if isinstance(latest, dict) and isinstance(latest.get("baseModel"), str) else None,
                "tags": [tag for tag in model.get("tags", []) if isinstance(tag, str)][:256] if isinstance(model.get("tags"), list) else [],
            },
            "artifacts": artifacts,
            "license": license_name if isinstance(license_name, str) and license_name else None,
            "evidence_refs": refs,
        }

    def _revisions(self, model):
        model_id = _model_id(model.get("id"))
        distribution_id = self.identities.get("distribution:" + model_id)
        rows = []
        for version in self._versions(model):
            version_id = _model_id(version.get("id"))
            files = _version_files(version)
            rows.append({
                "id": self.identities.get("revision:" + model_id + ":" + version_id),
                "created_at": _stamp(version.get("createdAt")),
                "updated_at": _stamp(version.get("updatedAt") or version.get("createdAt")),
                "row_version": 1,
                "metadata": {
                    "source_v1": {
                        "source_type": "CIVITAI", "model_id": model_id, "version_id": version_id,
                        "name": version.get("name") if isinstance(version.get("name"), str) else None,
                        "base_model": version.get("baseModel") if isinstance(version.get("baseModel"), str) else None,
                        "license": version.get("license") if isinstance(version.get("license"), str) else None,
                        "nsfw_level": version.get("nsfwLevel") if isinstance(version.get("nsfwLevel"), int) else None,
                    }
                },
                "distribution_id": distribution_id,
                "revision_key": _version_key(version_id),
                "upstream_commit": version_id,
                "manifest_hash": _manifest_hash(version, files),
                "state": "CANDIDATE",
            })
        return rows

    def _manifest(self, model, version_id):
        version = next((item for item in self._versions(model) if _model_id(item.get("id")) == version_id), None)
        if version is None:
            raise fail("NOT_FOUND", "civitai_model_version_not_found")
        artifacts = _version_files(version)
        return {
            "revision_key": _version_key(version_id),
            "manifest_hash": _manifest_hash(version, artifacts),
            "artifacts": artifacts,
        }

    def call(self, method, params, ctx):
        if not isinstance(params, dict):
            raise fail("VALIDATION_FAILED", "civitai_request_invalid")
        self._check_source(params.get("source_id"))
        ctx.check_cancelled()
        if method == "source.search":
            if params.get("capabilities"):
                raise fail("UNSUPPORTED_CAPABILITY", "civitai_capability_filter_deferred_to_core")
            query = params.get("q", "")
            if not isinstance(query, str) or len(query) > 500:
                raise fail("VALIDATION_FAILED", "civitai_search_query_invalid")
            page_size = min(max(1, params.get("limit", 20)), 100)
            query_binding = json.dumps({"q": query, "limit": page_size}, ensure_ascii=False, sort_keys=True, separators=(",", ":"))
            upstream_cursor = self._cursor(params.get("cursor"), query_binding)
            query_params = {"limit": page_size, "nsfw": "false", "sort": "Most Downloaded"}
            if query:
                query_params["query"] = query
            if upstream_cursor:
                query_params["cursor"] = upstream_cursor
            page, _headers = self._api_json("/models", query_params, ctx)
            raw_items = page.get("items")
            if not isinstance(raw_items, list):
                raise fail("PROTOCOL_ERROR", "civitai_models_page_invalid")
            items = []
            for model in raw_items[:page_size]:
                if not isinstance(model, dict):
                    continue
                version = self._latest(model)
                if version is None:
                    continue
                row = self._variant(model, version)
                if row is not None:
                    row.pop("_distribution_id", None)
                    items.append(row)
            metadata = page.get("metadata") if isinstance(page.get("metadata"), dict) else {}
            next_cursor = metadata.get("nextCursor")
            result = {
                "items": items,
                "next_cursor": self._encode_cursor(query_binding, next_cursor) if isinstance(next_cursor, str) and next_cursor else None,
                "snapshot_at": _now(),
            }
            self.contracts.response(method, result)
            return result

        if method in ("source.get_model", "source.list_revisions", "source.get_manifest", "source.estimate_size"):
            model_id = _model_id(params.get("upstream_model_id"))
            if model_id is None or params.get("upstream_namespace", "civitai") not in ("civitai", "models"):
                raise fail("VALIDATION_FAILED", "civitai_model_reference_invalid")
            model, _headers = self._model(model_id, ctx)
            if method == "source.get_model":
                result = self._inspect(model, ctx)
            elif method == "source.list_revisions":
                rows = self._revisions(model)
                result = {"items": rows, "next_cursor": None, "snapshot_at": _now()}
            else:
                revision_key = params.get("revision_key")
                version_id = _version_id(revision_key) if isinstance(revision_key, str) else None
                manifest = self._manifest(model, version_id) if version_id else self._manifest(model, _model_id(self._latest(model).get("id")) if self._latest(model) else "")
                if method == "source.get_manifest":
                    result = manifest
                else:
                    estimates = [item["size_estimate"] for item in manifest["artifacts"]]
                    evidence = [ref for estimate in estimates for ref in estimate["evidence_refs"]]
                    known = bool(estimates) and all(item["confidence"] == "ESTIMATED" for item in estimates)
                    midpoint = sum((int(item["lower_bound_bytes"]) + int(item["upper_bound_bytes"])) // 2 for item in estimates) if known else None
                    upper = sum(int(item["upper_bound_bytes"]) for item in estimates) if known else None
                    result = {
                        "download_bytes": str(midpoint) if midpoint is not None else None,
                        "final_artifact_bytes": str(midpoint) if midpoint is not None else None,
                        "installed_bytes": str(midpoint) if midpoint is not None else None,
                        "derived_cache_bytes": "0", "temporary_peak_bytes": None, "existing_bytes": None,
                        "peak_extra_bytes": None, "required_free_bytes": None,
                        "upper_bound_bytes": str(upper) if upper is not None else None,
                        "confidence": "ESTIMATED" if known else "UNKNOWN",
                        "reason": "Suma estimada de sizeKB de Civitai con margen por archivo; no garantiza bytes transferidos ni espacio total requerido." if known else "Al menos un archivo carece de sizeKB utilizable; los bytes agregados siguen desconocidos.",
                        "evidence_refs": evidence or ["civitai:model-version:" + version_id], "per_volume": [],
                    }
            self.contracts.response(method, result)
            return result

        if method == "source.auth_probe":
            if params.get("auth_mode") == "NONE" and params.get("account_id") is None and params.get("credential_reference_id") is None:
                result = {"authenticated": False, "auth_required": False, "license_required": False, "reason": "civitai_public_metadata_anonymous; downloads require a provider credential"}
            else:
                result = {"authenticated": False, "auth_required": True, "license_required": False, "reason": "civitai_credential_resolution_unavailable_until_p045"}
            self.contracts.response(method, result)
            return result
        if method in ("source.download_plan", "source.download"):
            raise fail("UNSUPPORTED_CAPABILITY", "civitai_download_requires_provider_auth_and_p045")
        if method == "source.sync":
            raise fail("UNSUPPORTED_CAPABILITY", "civitai_sync_not_qualified")
        raise fail("UNSUPPORTED_CAPABILITY", "p049_provider_method_unsupported")


UNSUPPORTED_REASONS = {
    "KAGGLE": "kaggle_model_catalog_requires_official_cli_mapping_and_p045_credential_resolution",
    "OLLAMA_LIBRARY": "ollama_runtime_tags_are_not_a_stable_ollama_library_catalog_api",
    "OPENXLAB": "openxlab_model_center_search_has_no_published_stable_catalog_api; scraping_is_disabled",
    "NGC": "ngc_catalog_search_has_no_published_stable_query_contract_for_this_adapter",
}


class UnsupportedProviderAdapter:
    """Profile-specific fail-closed adapter while stable APIs are unqualified."""

    def __init__(self, package: Path, work: Path, manifest: dict, contracts, expected_provider=None):
        self.package, self.work = package, work
        self.manifest, self.contracts = manifest, contracts
        profile = json.loads((work / "profile.json").read_text(encoding="utf-8"), object_pairs_hook=_unique_pairs)
        self.provider = profile.get("provider")
        self.source_id = profile.get("source_id")
        if self.provider not in UNSUPPORTED_REASONS or self.provider != expected_provider or not isinstance(self.source_id, str) or not UUID7.fullmatch(self.source_id):
            raise fail("VALIDATION_FAILED", "p049_provider_profile_invalid")
        if manifest.get("capabilities"):
            raise fail("VALIDATION_FAILED", "p049_unsupported_profile_announced_capability")

    def shutdown(self):
        return None

    def call(self, method, params, ctx):
        del ctx
        if not isinstance(params, dict) or params.get("source_id") not in (None, self.source_id):
            raise fail("VALIDATION_FAILED", "p049_source_id_mismatch")
        if method == "source.auth_probe":
            result = {
                "authenticated": False, "auth_required": True, "license_required": False,
                "reason": "p049_credentials_or_catalog_api_not_qualified_until_dependencies_pass",
            }
            self.contracts.response(method, result)
            return result
        raise fail("UNSUPPORTED_CAPABILITY", UNSUPPORTED_REASONS[self.provider])


def create_adapter(provider, package, work, manifest, contracts):
    if provider == "CIVITAI":
        return CivitaiAdapter(package, work, manifest, contracts)
    return UnsupportedProviderAdapter(package, work, manifest, contracts, expected_provider=provider)
