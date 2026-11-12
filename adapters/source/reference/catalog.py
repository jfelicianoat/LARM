"""Catálogo local de referencia: DTOs publicados, snapshot y metadatos abiertos."""
import base64
import copy
import hashlib
import json
import re
from datetime import datetime, timezone

from larm_adapter_sdk import ContractViolation
from transfer import fail, path_allowed, relative


def stamp(moment=None):
    return (moment or datetime.now(timezone.utc)).isoformat(timespec="milliseconds").replace("+00:00", "Z")


def canonical(value):
    """Subconjunto JCS de planes: sin floats; claves por unidades UTF-16.

    No se presenta como sustituto genérico del canonizador completo de Core.
    Los planes de esta referencia sólo contienen enteros pequeños y strings.
    """
    if value is None or isinstance(value, (str, bool)):
        return value
    if isinstance(value, int) and abs(value) <= (1 << 53) - 1:
        return value
    if isinstance(value, list):
        return [canonical(item) for item in value]
    if isinstance(value, dict) and all(isinstance(key, str) for key in value):
        return {key: canonical(value[key]) for key in sorted(value, key=lambda key: key.encode("utf-16-be"))}
    raise fail("VALIDATION_FAILED", "reference_canonical_subset")


def digest(value):
    raw = json.dumps(canonical(value), ensure_ascii=False, allow_nan=False, separators=(",", ":")).encode()
    return hashlib.sha256(raw).hexdigest()


def public_metadata(value):
    if isinstance(value, dict):
        sensitive = {"token", "access_token", "refresh_token", "api_token", "api_key", "apikey", "password", "passwd", "secret", "authorization", "credential", "credentials", "cookie", "cookies"}
        return {key: "[REDACTED]" if key.lower() in sensitive or key.lower().endswith(("_token", "_password", "_secret")) else public_metadata(item) for key, item in value.items()}
    if isinstance(value, list):
        return [public_metadata(item) for item in value]
    if isinstance(value, str) and (re.search(r"(?:Bearer\s+|sk-proj-|hf_|ghp_|github_pat_)[A-Za-z0-9_-]{12,}", value) or len(value) > 65536):
        return "[REDACTED]"
    return value


def unique(pairs):
    value = {}
    for key, item in pairs:
        if key in value:
            raise ValueError("duplicate key")
        value[key] = item
    return value


def reject_constant(_value):
    raise ValueError("nonfinite number")


class Catalog:
    def __init__(self, path, roots, contracts, source_id):
        path = path_allowed(path, roots)
        with path.open("rb") as stream:
            raw = stream.read((1 << 20) + 1)
        if len(raw) > 1 << 20:
            raise fail("VALIDATION_FAILED", "reference_catalog_too_large")
        try:
            value = json.loads(raw, object_pairs_hook=unique, parse_constant=reject_constant)
            if set(value) != {"schema_version", "source_id", "entries"} or value["schema_version"] != "1.0" or value["source_id"] != source_id or not 1 <= len(value["entries"]) <= 100:
                raise ValueError("invalid catalog")
            self.entries = value["entries"]
            ids, identities = set(), set()
            for entry in self.entries:
                if set(entry) != {"upstream_namespace", "upstream_model_id", "model", "inspection", "revision", "downloads", "original"}:
                    raise ValueError("invalid entry")
                identity = (entry["upstream_namespace"], entry["upstream_model_id"])
                contracts.request("source.get_model", {"source_id": source_id, "upstream_namespace": identity[0], "upstream_model_id": identity[1]})
                contracts.response("source.get_model", entry["inspection"])
                contracts.response("source.search", {"items": [entry["model"]], "next_cursor": None, "snapshot_at": stamp()})
                contracts.response("source.list_revisions", {"items": [entry["revision"]], "next_cursor": None, "snapshot_at": stamp()})
                if identity in identities or entry["model"]["id"] in ids or entry["model"]["revision_id"] != entry["revision"]["id"]:
                    raise ValueError("duplicate identity")
                ids.add(entry["model"]["id"])
                identities.add(identity)
                files = entry["inspection"]["artifacts"]
                if not 1 <= len(files) <= 100 or len(files) != len(entry["downloads"]):
                    raise ValueError("invalid artifacts")
                paths = set()
                total = 0
                for artifact, download in zip(files, entry["downloads"], strict=True):
                    name = relative(artifact["relative_path"])
                    if name.lower() in paths or not artifact["role"] or download["relative_path"] != name or set(download) != {"relative_path", "url", "etag"}:
                        raise ValueError("invalid download binding")
                    if artifact["size_bytes"] is None or artifact["sha256"] is None:
                        raise ValueError("reference requires pinned fixture")
                    total += int(artifact["size_bytes"])
                    paths.add(name.lower())
                if total > 64 << 20:
                    raise ValueError("reference fixture limit")
                manifest = self.manifest(entry)
                if entry["revision"]["manifest_hash"] != manifest["manifest_hash"]:
                    raise ValueError("manifest mismatch")
                if entry["revision"]["revision_key"] != "content:" + manifest["manifest_hash"]:
                    raise ValueError("revision is not pinned to fixture")
        except (ValueError, TypeError, KeyError, ContractViolation, RecursionError):
            raise fail("VALIDATION_FAILED", "reference_catalog_invalid") from None
        self.hash = hashlib.sha256(raw).hexdigest()
        self.source_id = source_id
        self.snapshot = stamp(datetime.fromtimestamp(path.stat().st_mtime, timezone.utc))

    @staticmethod
    def manifest(entry):
        files = entry["inspection"]["artifacts"]
        descriptors = [{"relative_path": f["relative_path"], "sha256": f["sha256"], "size_bytes": int(f["size_bytes"]), "required": f["required"]} for f in sorted(files, key=lambda f: f["relative_path"])]
        return {"revision_key": entry["revision"]["revision_key"], "manifest_hash": digest(descriptors), "artifacts": copy.deepcopy(files)}

    def find(self, request):
        entry = next((e for e in self.entries if e["upstream_namespace"] == request["upstream_namespace"] and e["upstream_model_id"] == request["upstream_model_id"]), None)
        if entry is None:
            raise fail("MODEL_NOT_FOUND", "reference_model_not_found")
        if "revision_key" in request and request["revision_key"] != entry["revision"]["revision_key"]:
            raise fail("MODEL_NOT_FOUND", "reference_revision_not_found")
        return entry

    def search(self, request, support):
        if not support["search_applicable"] or (request["q"] and "source.search.q" not in support["supported_filters"]) or (request["capabilities"] and "source.search.capabilities" not in support["supported_filters"]):
            raise fail("UNSUPPORTED_CAPABILITY", "source_filter_not_supported")
        query_hash = digest({"q": request["q"], "capabilities": sorted(set(request["capabilities"])), "source_id": request["source_id"]})
        after = 0
        if request["cursor"] is not None:
            try:
                encoded = request["cursor"]
                if len(encoded) > 4096:
                    raise ValueError()
                cursor = json.loads(base64.urlsafe_b64decode(encoded + "=" * (-len(encoded) % 4)), object_pairs_hook=unique)
                if set(cursor) != {"catalog", "filter", "after"} or cursor["catalog"] != self.hash or cursor["filter"] != query_hash or type(cursor["after"]) is not int or cursor["after"] <= 0:
                    raise ValueError()
                after = cursor["after"]
            except (ValueError, TypeError, KeyError):
                raise fail("VALIDATION_FAILED", "source_cursor_invalid") from None
        matches = [e for e in sorted(self.entries, key=lambda e: (e["model"]["created_at"], e["model"]["id"])) if (not request["q"] or request["q"].lower() in str(e["model"]["metadata"].get("display_name", "")).lower()) and (not request["capabilities"] or set(request["capabilities"]) & set(e["inspection"]["capabilities"]))]
        if after > len(matches):
            raise fail("VALIDATION_FAILED", "source_cursor_invalid")
        next_index = after + request["limit"]
        items = []
        for entry in matches[after:next_index]:
            model = copy.deepcopy(entry["model"])
            model["metadata"] = public_metadata(model["metadata"])
            model["metadata"]["source_v1"] = {"original": public_metadata(entry["original"]), "normalized": {
                "architecture": entry["inspection"]["architecture"], "format": entry["inspection"]["format"],
                "catalog_reference": {"source_id": self.source_id, "upstream_namespace": entry["upstream_namespace"],
                    "upstream_model_id": entry["upstream_model_id"], "revision_key": entry["revision"]["revision_key"]},
            }, "limitations": support["limitations"], "rate_limit": None}
            items.append(model)
        next_cursor = None
        if next_index < len(matches):
            next_cursor = base64.urlsafe_b64encode(json.dumps({"catalog": self.hash, "filter": query_hash, "after": next_index}, separators=(",", ":")).encode()).decode().rstrip("=")
        return {"items": items, "next_cursor": next_cursor, "snapshot_at": self.snapshot}
