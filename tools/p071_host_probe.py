#!/usr/bin/env python3
"""Real host probe for P071; verifies that an unmanaged interpreter is refused."""
from __future__ import annotations

import importlib.util
import json
import sys
from pathlib import Path


def main() -> int:
    worker_path = Path(__file__).resolve().parents[1] / "adapters/runtime/transformers/worker.py"
    spec = importlib.util.spec_from_file_location("larm_transformers_worker", worker_path)
    if spec is None or spec.loader is None:
        return 2
    worker = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(worker)
    response = worker.handle({"protocol_version": "1", "operation": "probe"})
    print(json.dumps(response, ensure_ascii=False, sort_keys=True))
    result = response["result"]
    if result["managed_python"]:
        return 1
    if result["torch_version"] is not None or result["transformers_version"] is not None:
        return 1
    if not Path(sys.prefix).is_absolute():
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
