"""Public Core artifact mapping; no ledger, network, filesystem or permission grant."""
import copy
import re
import unicodedata

from .access import fail, relative

UUID7 = re.compile(r"^[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$")
SHA256 = re.compile(r"^[0-9a-f]{64}$")


def _bad(reason="source_acquisition_binding_invalid"):
    return fail("VALIDATION_FAILED", reason, "El vínculo público de adquisición no es válido")


def _text(value):
    return isinstance(value, str) and 1 <= len(value) <= 4096 and value.strip() == value and not any(
        unicodedata.category(char) == "Cc" for char in value)


def validate_source_acquisition(request):
    if "acquisition" not in request:
        return None
    binding = request["acquisition"]
    if not isinstance(binding, dict) or set(binding) != {"revision", "manifest_hash", "artifacts"}:
        raise _bad()
    revision = binding["revision"]
    if not isinstance(revision, dict) or set(revision) != {
        "source_id", "upstream_namespace", "upstream_model_id", "revision_key"
    } or not isinstance(revision["source_id"], str) or not UUID7.fullmatch(revision["source_id"]):
        raise _bad()
    if any(not _text(revision[key]) for key in ("upstream_namespace", "upstream_model_id", "revision_key")):
        raise _bad()
    if request.get("source_id") != revision["source_id"] or revision["revision_key"].startswith("pending:"):
        raise _bad()
    if not isinstance(binding["manifest_hash"], str) or not SHA256.fullmatch(binding["manifest_hash"]):
        raise _bad()
    artifacts = binding["artifacts"]
    if not isinstance(artifacts, list) or not 1 <= len(artifacts) <= 4096:
        raise _bad()
    ids, paths, optional, ordering = set(), set(), set(), []
    for artifact in artifacts:
        if not isinstance(artifact, dict) or set(artifact) != {"artifact_id", "relative_path", "required"}:
            raise _bad()
        identity, path = artifact["artifact_id"], artifact["relative_path"]
        if not isinstance(identity, str) or not UUID7.fullmatch(identity) or identity in ids:
            raise _bad("source_acquisition_artifacts_invalid")
        if not _text(path) or type(artifact["required"]) is not bool:
            raise _bad("source_acquisition_artifacts_invalid")
        relative(path)
        if path.lower() in paths:
            raise _bad("source_acquisition_artifacts_invalid")
        ids.add(identity)
        paths.add(path.lower())
        ordering.append((path, identity))
        if not artifact["required"]:
            optional.add(identity)
    if not any(row["required"] for row in artifacts) or ordering != sorted(ordering):
        raise _bad()
    selected = request.get("optional_artifact_ids", [])
    if not isinstance(selected, list) or any(not isinstance(value, str) for value in selected):
        raise _bad("source_acquisition_optional_selection_invalid")
    if len(set(selected)) != len(selected) or not set(selected) <= optional:
        raise _bad("source_acquisition_optional_selection_invalid")
    return binding


def select_source_acquisition(request, manifest):
    binding = validate_source_acquisition(request)
    if binding is None:
        raise _bad("source_acquisition_binding_missing")
    if not isinstance(manifest, dict):
        raise fail("STALE_REVISION", "source_acquisition_manifest_changed")
    files = manifest.get("artifacts")
    if manifest.get("revision_key") != binding["revision"]["revision_key"] or (
        manifest.get("manifest_hash") != binding["manifest_hash"]
    ) or not isinstance(files, list) or len(files) != len(binding["artifacts"]):
        raise fail("STALE_REVISION", "source_acquisition_manifest_changed")
    by_path = {}
    for artifact in files:
        if not isinstance(artifact, dict):
            raise fail("STALE_REVISION", "source_acquisition_manifest_changed")
        path = relative(artifact.get("relative_path"))
        if path.lower() in by_path or type(artifact.get("required")) is not bool:
            raise fail("STALE_REVISION", "source_acquisition_manifest_changed")
        by_path[path.lower()] = artifact
    selected = set(request.get("optional_artifact_ids", []))
    result = []
    for mapping in binding["artifacts"]:
        artifact = by_path.get(mapping["relative_path"].lower())
        if artifact is None or artifact["relative_path"] != mapping["relative_path"] or (
            artifact["required"] != mapping["required"]
        ):
            raise fail("STALE_REVISION", "source_acquisition_manifest_changed")
        if mapping["required"] or mapping["artifact_id"] in selected:
            result.append(copy.deepcopy(artifact))
    return result


def source_acquisition_payload(request, spec):
    payload = {"source_download_v1": copy.deepcopy(spec)}
    binding = validate_source_acquisition(request)
    if binding is None:
        return payload
    if spec.get("revision") != binding["revision"] or any(
        spec.get(key) != request.get(key)
        for key in ("variant_id", "storage_location_id", "priority", "hard_budget_bytes")
    ):
        raise _bad("source_acquisition_revision_changed")
    selected = set(request.get("optional_artifact_ids", []))
    expected = {(row["relative_path"], row["required"]) for row in binding["artifacts"]
                if row["required"] or row["artifact_id"] in selected}
    files = spec.get("artifacts")
    if not isinstance(files, list) or any(
        not isinstance(row, dict) or not isinstance(row.get("relative_path"), str)
        or type(row.get("required")) is not bool for row in files
    ):
        raise _bad("source_acquisition_selection_changed")
    actual = {(row.get("relative_path"), row["required"]) for row in files}
    if len(actual) != len(files) or actual != expected:
        raise _bad("source_acquisition_selection_changed")
    payload["source_acquisition_v1"] = copy.deepcopy(binding)
    return payload
