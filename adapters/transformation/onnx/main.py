"""Worker aislado del entorno Python del usuario; stdout sólo NDJSON."""
import json
import sys
from pathlib import Path

from larm_adapter_sdk import Contracts, ProtocolServer
from adapter import Adapter


def main():
    try:
        package = Path(__file__).resolve().parent.parent
        contracts = Contracts(package / "docs")
        manifest = contracts.manifest(json.loads((package / "manifest.json").read_text(encoding="utf-8")))
        adapter = Adapter(package,Path.cwd(),manifest,contracts)
        ProtocolServer(contracts,manifest,adapter).serve(sys.stdin.buffer,sys.stdout.buffer)
        return 0
    except Exception:
        print("product-onnx-transformation: inicio o cierre no completado",file=sys.stderr)
        return 2


if __name__=="__main__":
    raise SystemExit(main())
