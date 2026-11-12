#!/usr/bin/env python3
"""Attempt one anonymous, read-only Hugging Face search and classify availability."""
from __future__ import annotations

import json
import subprocess
import sys
import tempfile
import uuid
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(REPO / "adapters/source/huggingface"))
from package import create  # noqa: E402


def main() -> int:
    target = REPO / "target"
    target.mkdir(exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="p046-live-", dir=target) as temporary:
        base = Path(temporary)
        package_root, work = base / "package", base / "work"
        manifest = create(REPO, package_root, work)
        source_id = json.loads((work / "profile.json").read_text(encoding="utf-8"))["source_id"]
        process = subprocess.Popen(
            [str(package_root / "bin/python.exe"), "-I", "-B", "-m", "main"],
            cwd=work, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            text=True, encoding="utf-8", bufsize=1,
        )

        def request(method: str, params: dict) -> dict:
            assert process.stdin is not None and process.stdout is not None
            frame = {"jsonrpc": "2.0", "id": str(uuid.uuid7()), "method": method, "params": params}
            process.stdin.write(json.dumps(frame, separators=(",", ":")) + "\n")
            process.stdin.flush()
            line = process.stdout.readline()
            if not line:
                stderr = process.stderr.read() if process.stderr else ""
                raise RuntimeError(f"worker closed before reply: {stderr[:300]}")
            return json.loads(line)

        handshake = request("core.handshake", {
            "protocol_version": "1.0", "adapter_id": manifest["id"], "nonce": "p046-public-read-only",
        })
        if "result" not in handshake:
            print(json.dumps({"status": "FAIL", "reason": "worker_handshake_failed"}))
            return 1
        search = request("source.search", {
            "source_id": source_id, "q": "llama", "capabilities": [], "cursor": None, "limit": 1,
        })
        if process.stdin:
            process.stdin.close()
        process.wait(timeout=12)
        if process.returncode != 0:
            print(json.dumps({"status": "FAIL", "reason": "worker_exit_failed"}))
            return 1
        if "result" in search:
            print(json.dumps({"status": "PASS", "items": len(search["result"].get("items", [])), "read_only": True}))
            return 0
        data = search.get("error", {}).get("data", {})
        code = data.get("code")
        details = data.get("details", {})
        reason = details.get("reason")
        if code == "UPSTREAM_UNAVAILABLE":
            print(json.dumps({"status": "BLOCKED", "code": code, "reason": reason}))
            return 2
        print(json.dumps({"status": "FAIL", "code": code, "reason": reason}))
        return 1


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, RuntimeError, subprocess.SubprocessError, ValueError) as error:
        # Keep diagnostics free of request URLs, response bodies, and secrets.
        print(json.dumps({"status": "FAIL", "reason": type(error).__name__}))
        raise SystemExit(1)
