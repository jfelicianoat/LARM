"""Shared packaged-worker smoke and live probe for P049."""
from __future__ import annotations

import json
import subprocess
import sys
import tempfile
import uuid
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(REPO / "adapters/source/p049"))
from package import create, package_hash  # noqa: E402


def run(*, live: bool) -> dict:
    target = REPO / "target"
    target.mkdir(exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="p049-worker-", dir=target) as directory:
        root = Path(directory)
        package, work = root / "package", root / "work"
        manifest = create(REPO, package, work, "CIVITAI")
        digest = package_hash(package)
        if digest != manifest["package_sha256"]:
            return {"status": "FAIL", "reason": "package_hash_mismatch"}
        source_id = json.loads((work / "profile.json").read_text(encoding="utf-8"))["source_id"]
        process = subprocess.Popen(
            [str(package / "bin/python.exe"), "-I", "-B", "-m", "main"],
            cwd=work, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            text=True, encoding="utf-8", bufsize=1,
        )

        def request(method: str, params: dict) -> dict:
            if process.stdin is None or process.stdout is None:
                raise RuntimeError("worker pipes unavailable")
            frame = {"jsonrpc": "2.0", "id": str(uuid.uuid7()), "method": method, "params": params}
            process.stdin.write(json.dumps(frame, separators=(",", ":")) + "\n")
            process.stdin.flush()
            line = process.stdout.readline()
            if not line:
                raise RuntimeError("worker closed before replying")
            return json.loads(line)

        try:
            handshake = request("core.handshake", {
                "protocol_version": "1.0", "adapter_id": manifest["id"], "nonce": "p049-worker-smoke",
            })
            if handshake.get("result", {}).get("nonce") != "p049-worker-smoke":
                raise RuntimeError("packaged worker handshake failed")
            auth = request("source.auth_probe", {
                "source_id": source_id, "auth_mode": "NONE", "account_id": None, "credential_reference_id": None,
            })
            auth_result = auth.get("result", {})
            if auth_result.get("authenticated") is not False or auth_result.get("auth_required") is not False:
                raise RuntimeError("Civitai anonymous metadata probe contract failed")
            result = {
                "status": "PASS", "package_sha256": digest,
                "handshake": True, "anonymous_metadata_probe": True,
            }
            if live:
                search = request("source.search", {
                    "source_id": source_id, "q": "SDXL", "capabilities": [], "cursor": None, "limit": 5,
                })
                if "result" in search:
                    page = search["result"]
                    result["live_search"] = "PASS"
                    result["items"] = len(page.get("items", []))
                    result["snapshot_at"] = page.get("snapshot_at")
                else:
                    error = search.get("error", {}).get("data", {})
                    code = error.get("code") if isinstance(error, dict) else None
                    if code in ("RESOURCE_UNAVAILABLE", "AUTH_REQUIRED"):
                        result["status"] = "BLOCKED"
                        result["live_search"] = "BLOCKED"
                        result["reason"] = "public_api_unavailable_or_access_restricted"
                    else:
                        result["status"] = "FAIL"
                        result["live_search"] = "FAIL"
                        result["reason"] = "public_api_contract_rejected"
            if process.stdin:
                process.stdin.close()
            process.wait(timeout=20)
            if process.returncode != 0:
                return {"status": "FAIL", "reason": "worker_exit_nonzero"}
            return result
        finally:
            if process.poll() is None:
                if process.stdin and not process.stdin.closed:
                    process.stdin.close()
                process.kill()
                process.wait(timeout=10)


if __name__ == "__main__":
    output = run(live="--live" in sys.argv[1:])
    print(json.dumps(output, ensure_ascii=False))
    raise SystemExit(0 if output["status"] == "PASS" else 2 if output["status"] == "BLOCKED" else 1)
