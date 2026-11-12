"""Portable JSON-RPC entry point for one P049 provider profile."""
from __future__ import annotations

import json
import sys
from pathlib import Path

from larm_adapter_sdk import Contracts, ProtocolServer
from provider_adapter import Adapter


def main() -> int:
    package = Path(__file__).resolve().parent.parent
    adapter = None
    try:
        contracts = Contracts(package / "docs")
        manifest = contracts.manifest(json.loads((package / "manifest.json").read_text(encoding="utf-8")))
        adapter = Adapter(package, Path.cwd(), manifest, contracts)
        ProtocolServer(contracts, manifest, adapter).serve(sys.stdin.buffer, sys.stdout.buffer)
        return 0
    except Exception:
        print("p049-source: inicio o cierre no completado", file=sys.stderr)
        if adapter is not None:
            try:
                adapter.shutdown()
            except Exception:
                pass
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
