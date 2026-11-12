#!/usr/bin/env python3
"""Build and launch the portable P046 worker without network access."""
from __future__ import annotations

import json
import subprocess
import sys
import tempfile
import uuid
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(REPO / "adapters/source/huggingface"))
from package import create, package_hash  # noqa: E402


def main() -> int:
    target = REPO / "target"
    target.mkdir(exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="p046-package-", dir=target) as temporary:
        base = Path(temporary)
        package_root, work = base / "package", base / "work"
        manifest = create(REPO, package_root, work)
        if package_hash(package_root) != manifest["package_sha256"]:
            raise SystemExit("portable package hash mismatch")
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
            "protocol_version": "1.0", "adapter_id": manifest["id"], "nonce": "p046-package-smoke",
        })
        if handshake.get("result", {}).get("nonce") != "p046-package-smoke":
            raise RuntimeError("portable worker handshake failed")
        probe = request("source.auth_probe", {
            "source_id": source_id, "auth_mode": "NONE", "account_id": None, "credential_reference_id": None,
        })
        result = probe.get("result", {})
        if result.get("authenticated") is not False or result.get("auth_required") is not False:
            raise RuntimeError("anonymous public auth probe returned an invalid result")
        if process.stdin:
            process.stdin.close()
        process.wait(timeout=10)
        if process.returncode != 0:
            stderr = process.stderr.read() if process.stderr else ""
            raise RuntimeError(f"portable worker exit={process.returncode}: {stderr[:300]}")
        print("PASS portable package hash, isolated worker handshake, and public auth probe")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
