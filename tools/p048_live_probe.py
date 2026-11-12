#!/usr/bin/env python3
"""Query one public GitHub Releases repository through the packaged P048 worker."""
from __future__ import annotations

import json
import subprocess
import sys
import tempfile
import uuid
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(REPO / "adapters/source/p048"))
from package import create  # noqa: E402


def main() -> int:
    executable = REPO / "target/debug/larm-source-p048.exe"
    if not executable.is_file():
        raise FileNotFoundError("P048 worker has not been built")
    target = REPO / "target"
    target.mkdir(exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="p048-github-live-", dir=target) as temporary:
        base = Path(temporary)
        package_root, work = base / "package", base / "work"
        result = create(
            REPO,
            package_root,
            work,
            "GITHUB",
            repository="ggml-org/whisper.cpp",
            binary=executable,
        )
        manifest = result["manifest"]
        source_id = result["profile"]["source_id"]
        process = subprocess.Popen(
            [str(package_root / "bin/larm-source-p048.exe"), "--manifest", "manifest.json"],
            cwd=work,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            encoding="utf-8",
            bufsize=1,
        )

        def request(method: str, params: dict) -> dict:
            if process.stdin is None or process.stdout is None:
                raise RuntimeError("worker pipes are unavailable")
            frame = {"jsonrpc": "2.0", "id": str(uuid.uuid7()), "method": method, "params": params}
            process.stdin.write(json.dumps(frame, separators=(",", ":")) + "\n")
            process.stdin.flush()
            line = process.stdout.readline()
            if not line:
                raise RuntimeError("worker closed without a reply")
            return json.loads(line)

        try:
            handshake = request(
                "core.handshake",
                {"protocol_version": "1.0", "adapter_id": manifest["id"], "nonce": "p048-github-live"},
            )
            if handshake.get("result", {}).get("nonce") != "p048-github-live":
                data = handshake.get("error", {}).get("data", {})
                print(json.dumps({
                    "status": "FAIL",
                    "reason": data.get("details", {}).get("reason", "worker_handshake_failed"),
                    "code": data.get("code"),
                }))
                return 1
            search = request(
                "source.search",
                {"source_id": source_id, "q": "", "capabilities": [], "cursor": None, "limit": 1},
            )
            if process.stdin:
                process.stdin.close()
            process.wait(timeout=30)
            if process.returncode != 0:
                print(json.dumps({"status": "FAIL", "reason": "worker_exit_failed"}))
                return 1
            if "result" in search:
                items = search["result"].get("items", [])
                if not items:
                    print(json.dumps({"status": "BLOCKED", "reason": "repository_has_no_releases_with_assets"}))
                    return 2
                print(json.dumps({"status": "PASS", "repo": "ggml-org/whisper.cpp", "items": len(items), "read_only": True}))
                return 0
            data = search.get("error", {}).get("data", {})
            if data.get("retryable") is True:
                print(json.dumps({"status": "BLOCKED", "code": data.get("code"), "reason": data.get("details", {}).get("reason")}))
                return 2
            print(json.dumps({"status": "FAIL", "code": data.get("code"), "reason": data.get("details", {}).get("reason")}))
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
        print(json.dumps({"status": "BLOCKED", "reason": type(error).__name__}))
        raise SystemExit(2)
