"""Entry point for the packaged, isolated Python SDK runtime worker."""
import json
from pathlib import Path
import sys

from larm_adapter_sdk import Contracts, ProtocolServer
from adapter import Adapter


def main():
    try:
        package = Path(__file__).resolve().parent.parent
        contracts = Contracts(package / "docs")
        manifest = contracts.manifest(json.loads((package / "manifest.json").read_bytes()))
        adapter = Adapter(package, Path.cwd(), manifest, contracts)
        ProtocolServer(contracts, manifest, adapter).serve(sys.stdin.buffer, sys.stdout.buffer)
        return 0
    except Exception:
        print("ollama-runtime: inicio o cierre no completado", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
