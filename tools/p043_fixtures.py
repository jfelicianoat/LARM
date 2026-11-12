#!/usr/bin/env python3
"""Fixture de protocolo source; hashes calculados por Python, fuera del SDK."""
import hashlib
import json
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
OUT = REPO / "tests/fixtures/source/protocol.json"


def fixture():
    def uid(n):
        return f"0192aaaa-0000-7000-8000-{n:012d}"
    stamp = "2026-10-16T00:00:00.000Z"
    source, storage, variant = uid(1), uid(2), uid(11)
    artifact = {"relative_path": "weights.gguf", "size_bytes": "18", "sha256": hashlib.sha256(b"GGUFsource-fixture").hexdigest(), "required": True, "role": "weights"}
    inspection = {"upstream_identity": None, "architecture": None, "format": "GGUF", "quantization": None,
                  "capabilities": ["text.generate"], "limits": {"vendor_new_field": {"context": None, "evidence": ["declared"]}},
                  "artifacts": [artifact], "license": None, "evidence_refs": ["fixture:source/1"]}
    model = {"id": variant, "created_at": stamp, "updated_at": stamp, "row_version": 1,
             "metadata": {"vendor_unrecognized": {"nested": [1, True, None]}, "display_name": "llama fixture"},
             "revision_id": uid(21), "format": "GGUF", "quantization": None, "architecture": None, "bundle_hash": None,
             "lifecycle": "DISCOVERED", "availability": "UNKNOWN", "ownership": "EXTERNAL", "promotion": "NONE", "system_role": "GENERAL",
             "capabilities": ["text.generate"], "model_limits": dict.fromkeys(["parameter_count", "layers", "kv_heads", "head_dim", "context_limit", "kv_bits", "model_max_memory_bytes", "architecture_metadata_hash"]),
             "system1_state": None, "update_available": False, "status": "DISCOVERED"}
    upstream = {"source_id": source, "upstream_namespace": "fixture", "upstream_model_id": "llama"}
    revision = upstream | {"revision_key": "commit:fixture-v1"}
    estimate = {"download_bytes": "18", "final_artifact_bytes": "18", "installed_bytes": "0", "derived_cache_bytes": "0", "temporary_peak_bytes": "0", "existing_bytes": "0",
                "peak_extra_bytes": "18", "required_free_bytes": str((1 << 30) + 18), "upper_bound_bytes": "18", "confidence": "EXACT", "reason": "fixture_exact",
                "evidence_refs": ["fixture:source/1"], "per_volume": [{"volume_key": "fixture-volume", "peak_extra_bytes": "18", "margin_bytes": str(1 << 30), "other_reservations_bytes": "0", "usable_free_bytes": "2000000000", "eligible": True, "reason": "fixture_only"}]}
    auth = {"authenticated": False, "auth_required": False, "license_required": False, "reason": "public_fixture"}
    spec = {"variant_id": variant, "storage_location_id": storage, "revision": revision, "artifacts": [artifact], "auth": auth, "resumable": False, "priority": "NORMAL", "hard_budget_bytes": None}
    plan = {"plan_id": uid(31), "operation": "source.download", "resource_ids": [variant, storage], "storage_estimate": estimate,
            "steps": [{"phase": "DOWNLOAD", "adapter_id": "fixture.source", "request": {"source_download_v1": spec}}], "expires_at": "2099-01-01T00:00:00.000Z", "approval_required": False}
    # Sin floats ni claves Unicode: esta serialización es RFC8785 para la fixture.
    plan["plan_hash"] = hashlib.sha256(json.dumps(plan, sort_keys=True, ensure_ascii=False, separators=(",", ":")).encode()).hexdigest()
    manifest_hash = hashlib.sha256(json.dumps([{"relative_path": "weights.gguf", "sha256": artifact["sha256"], "size_bytes": 18, "required": True}], sort_keys=True, separators=(",", ":")).encode()).hexdigest()
    requests = {"source.search": {"source_id": source, "q": "llama", "capabilities": [], "cursor": None, "limit": 1},
                "source.get_model": upstream, "source.list_revisions": upstream, "source.get_manifest": revision, "source.estimate_size": revision,
                "source.download_plan": {"variant_id": variant, "storage_location_id": storage, "hard_budget_bytes": None, "source_id": source, "priority": "NORMAL"},
                "source.download": {"plan_id": plan["plan_id"], "plan_hash": plan["plan_hash"], "approval_id": None},
                "source.sync": {"source_id": source, "cursor": None, "etag": None, "query": {"vendor_query": {"unknown": True}}},
                "source.auth_probe": {"source_id": source, "auth_mode": "NONE", "account_id": None, "credential_reference_id": None}}
    responses = {"source.search": {"items": [model], "next_cursor": "page-2", "snapshot_at": stamp}, "source.get_model": inspection,
                 "source.list_revisions": {"items": [{"id": uid(21), "created_at": stamp, "updated_at": stamp, "row_version": 1, "metadata": {}, "distribution_id": uid(41), "revision_key": revision["revision_key"], "upstream_commit": "fixture-v1", "manifest_hash": manifest_hash, "state": "CANDIDATE"}], "next_cursor": None, "snapshot_at": stamp},
                 "source.get_manifest": {"revision_key": revision["revision_key"], "manifest_hash": manifest_hash, "artifacts": [artifact]}, "source.estimate_size": estimate,
                 "source.download_plan": plan, "source.download": {"job_id": uid(51), "status_url": f"/api/v1/jobs/{uid(51)}", "state": "QUEUED"},
                 "source.sync": {"cursor": "sync-2", "etag": "fixture-etag", "changed": True, "models": [inspection]}, "source.auth_probe": auth}
    return {"scope": "protocol fixture only; does not download or qualify an upstream", "source_id": source,
            "support": {"search_applicable": True, "supported_filters": ["source.search.q"], "limitations": ["fixture_only", "core_postfilter_required"]},
            "requests": requests, "responses": responses,
            "rate_limit": {"limit": "100", "remaining": "0", "reset_at": "2099-01-01T00:00:00.000Z", "retry_after_seconds": "900"}}


def main():
    expected = json.dumps(fixture(), indent=2, ensure_ascii=False) + "\n"
    if len(sys.argv) > 1 and sys.argv[1] == "check":
        assert OUT.read_text(encoding="utf-8") == expected, "fixture no reproducible"
        print("PASS: fixture source y hashes independientes al día")
    else:
        OUT.parent.mkdir(parents=True, exist_ok=True)
        OUT.write_text(expected, encoding="utf-8")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
