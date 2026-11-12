#!/usr/bin/env python3
"""Attempt one anonymous, read-only public ModelScope search."""
from __future__ import annotations

import json
import subprocess
import sys
import tempfile
import uuid
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(REPO / "adapters/source/modelscope"))
from package import create  # noqa: E402


def main() -> int:
    target = REPO / "target"
    target.mkdir(exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="p047-live-", dir=target) as temporary:
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
                raise RuntimeError("worker_closed_without_reply")
            return json.loads(line)

        try:
            handshake = request("core.handshake", {
                "protocol_version": "1.0", "adapter_id": manifest["id"], "nonce": "p047-public-read-only",
            })
            if handshake.get("result", {}).get("nonce") != "p047-public-read-only":
                print(json.dumps({"status": "FAIL", "reason": "worker_handshake_failed"}))
                return 1
            search = request("source.search", {
                "source_id": source_id, "q": "llama", "capabilities": [], "cursor": None, "limit": 1,
            })
            if process.stdin:
                process.stdin.close()
            process.wait(timeout=20)
            if process.returncode != 0:
                print(json.dumps({"status": "FAIL", "reason": "worker_exit_failed"}))
                return 1
            if "result" in search:
                items = search["result"].get("items", [])
                print(json.dumps({"status": "PASS", "items": len(items), "read_only": True}))
                return 0
            data = search.get("error", {}).get("data", {})
            code = data.get("code")
            reason = data.get("details", {}).get("reason")
            if code == "UPSTREAM_UNAVAILABLE":
                print(json.dumps({"status": "BLOCKED", "code": code, "reason": reason}))
                return 2
            print(json.dumps({"status": "FAIL", "code": code, "reason": reason}))
            return 1
        finally:
            if process.poll() is None:
                try:
                    if process.stdin:
                        process.stdin.close()
                    process.wait(timeout=3)
                except (OSError, subprocess.TimeoutExpired):
                    process.kill()
                    process.wait(timeout=3)


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, RuntimeError, subprocess.SubprocessError, ValueError) as error:
        # Diagnostics omit request URLs, response bodies, and credentials.
        print(json.dumps({"status": "BLOCKED", "reason": type(error).__name__}))
        raise SystemExit(2)
