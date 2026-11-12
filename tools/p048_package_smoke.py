#!/usr/bin/env python3
"""Run the packaged P048 local worker through its real JSON-RPC process."""
from __future__ import annotations

import base64
import json
import subprocess
import tempfile
import uuid
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
sys_path = str(REPO / "adapters/source/p048")
import sys
sys.path.insert(0, sys_path)
from package import create, package_hash  # noqa: E402


def main() -> int:
    executable = REPO / "target/debug/larm-source-p048.exe"
    if not executable.is_file():
        raise FileNotFoundError("P048 worker has not been built")
    target = REPO / "target"
    with tempfile.TemporaryDirectory(prefix="p048-worker-", dir=target) as temporary:
        base = Path(temporary)
        package_root, work, source_root = base / "package", base / "work", base / "source"
        source_root.mkdir()
        contents = b"GGUF\x00P048-local-worker-fixture"
        (source_root / "weights.gguf").write_bytes(contents)
        manifest_result = create(REPO, package_root, work, "LOCAL", root=str(source_root), binary=executable)
        manifest = manifest_result["manifest"]
        source_id = manifest_result["profile"]["source_id"]
        if package_hash(package_root) != manifest["package_sha256"]:
            raise RuntimeError("package hash did not roundtrip")
        http_result = create(
            REPO,
            base / "http-package",
            base / "http-work",
            "HTTP",
            url="https://example.invalid/private/model",
            binary=executable,
        )
        http_manifest = http_result["manifest"]
        http_source_id = http_result["profile"]["source_id"]
        if http_manifest["permissions"]["secret_refs"] != [f"larm/source/{http_source_id}"]:
            raise RuntimeError("HTTP manifest did not scope its credential reference to the source")
        process = subprocess.Popen(
            [str(package_root / "bin/larm-source-p048.exe"), "--manifest", "manifest.json"],
            cwd=work, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            text=True, encoding="utf-8", bufsize=1,
        )

        progress = []

        def request(method: str, params: dict) -> dict:
            if process.stdin is None or process.stdout is None:
                raise RuntimeError("worker pipes are unavailable")
            request_id = str(uuid.uuid7())
            process.stdin.write(json.dumps({"jsonrpc": "2.0", "id": request_id, "method": method, "params": params}, separators=(",", ":")) + "\n")
            process.stdin.flush()
            for _ in range(1024):
                line = process.stdout.readline()
                if not line:
                    diagnostic = process.stderr.read() if process.stderr else ""
                    raise RuntimeError(f"worker closed without a reply: {diagnostic[:200]}")
                frame = json.loads(line)
                if frame.get("method") == "core.progress":
                    if frame.get("params", {}).get("request_id") != request_id:
                        raise RuntimeError("progress belongs to another request")
                    progress.append(frame["params"])
                    continue
                if frame.get("id") != request_id:
                    raise RuntimeError("reply belongs to another request")
                return frame
            raise RuntimeError("progress limit exceeded")

        try:
            handshake = request("core.handshake", {"protocol_version": "1.0", "adapter_id": manifest["id"], "nonce": "p048-local-worker"})
            if handshake.get("result", {}).get("nonce") != "p048-local-worker":
                raise RuntimeError("protocol handshake failed")
            sync = request("source.sync", {"source_id": source_id, "cursor": None, "etag": None, "query": {}})
            result = sync.get("result", {})
            models = result.get("models", [])
            if len(models) != 1 or models[0].get("format") != "GGUF":
                error = sync.get("error", {}).get("data", {})
                reason = error.get("details", {}).get("reason", "")
                raise RuntimeError(f"P017 scanner did not return the local GGUF variant ({reason}; models={len(models)})")
            artifact = models[0]["artifacts"][0]
            if artifact["relative_path"] != "weights.gguf" or artifact["size_bytes"] is not None:
                raise RuntimeError("local sync did not preserve the stable ModelInspection schema")
            if str(source_root) in json.dumps(result):
                raise RuntimeError("source response disclosed an absolute local path")
            ref = {"source_id": source_id, "upstream_namespace": "local", "upstream_model_id": models[0]["upstream_identity"]}
            inspection = request("source.get_model", ref).get("result", {})
            if inspection.get("artifacts", [{}])[0].get("relative_path") != "weights.gguf" or "size_estimate" in inspection["artifacts"][0]:
                raise RuntimeError("source.get_model did not resolve the scanned variant")
            revisions = request("source.list_revisions", ref).get("result", {}).get("items", [])
            if len(revisions) != 1:
                raise RuntimeError("source.list_revisions did not return the scanned revision")
            revision_key = revisions[0]["revision_key"]
            revision = {**ref, "revision_key": revision_key}
            manifest_reply = request("source.get_manifest", revision).get("result", {})
            if manifest_reply.get("artifacts", [{}])[0].get("relative_path") != "weights.gguf":
                raise RuntimeError("source.get_manifest did not preserve the P017 scan")
            estimate = request("source.estimate_size", revision).get("result", {})
            if estimate.get("confidence") != "ESTIMATED" or estimate.get("download_bytes") is not None or estimate.get("upper_bound_bytes") != str(len(contents)):
                raise RuntimeError("local source size confidence or bytes are invalid")
            chunk = request("source.download_chunk", {
                "source_id": source_id, "job_id": str(uuid.uuid7()), "plan_id": str(uuid.uuid7()),
                "plan_hash": "a" * 64, "approval_id": None, "revision": revision,
                "relative_path": "weights.gguf", "offset": "0", "max_bytes": "524288", "validator": None,
            }).get("result", {})
            if base64.b64decode(chunk.get("data_base64", ""), validate=True) != contents or chunk.get("eof") is not True:
                raise RuntimeError("bounded local download chunk differs from its source")
            if [event.get("message_code") for event in progress] != ["source_chunk_start", "source_chunk_complete"]:
                raise RuntimeError("bounded read did not report its start and completion")
            unsupported = request("source.search", {"source_id": source_id, "q": "", "capabilities": [], "cursor": None, "limit": 10})
            error = unsupported.get("error", {}).get("data", {})
            if error.get("code") != "UNSUPPORTED_CAPABILITY":
                raise RuntimeError("import-only source.search did not reject unsupported discovery")
            probe = request("source.auth_probe", {"source_id": source_id, "auth_mode": "NONE", "account_id": None, "credential_reference_id": None})
            if probe.get("result", {}).get("auth_required") is not False:
                raise RuntimeError("local auth probe failed")
            if (source_root / "weights.gguf").read_bytes() != contents or len(list(source_root.iterdir())) != 1:
                raise RuntimeError("local source was modified")
            if process.stdin:
                process.stdin.close()
            process.wait(timeout=15)
            if process.returncode != 0:
                diagnostic = process.stderr.read() if process.stderr else ""
                raise RuntimeError(f"worker exit={process.returncode}: {diagnostic[:200]}")
            print(json.dumps({"status": "PASS", "handshake": True, "p017_models": len(models), "manifest_revisions_estimates_chunks": True, "search_rejected": True, "source_read_only": True, "source_read_progress": True, "package_sha256": manifest["package_sha256"]}))
            return 0
        finally:
            if process.poll() is None:
                if process.stdin:
                    process.stdin.close()
                try:
                    process.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=3)


if __name__ == "__main__":
    raise SystemExit(main())
