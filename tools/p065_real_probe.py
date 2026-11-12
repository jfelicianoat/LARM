#!/usr/bin/env python3
"""Observe the current Windows host without using or changing a global Python."""
from __future__ import annotations

import json
import os
import subprocess
import sys
from pathlib import Path


REPO = Path(__file__).resolve().parent.parent
WORKER = REPO / "adapters" / "runtime" / "olive-tooling" / "worker.py"


def main() -> int:
    env = os.environ.copy()
    env.pop("LARM_PYTHON_ROOT", None)
    env.pop("LARM_OLIVE_OUTPUT_ROOT", None)
    request = {"protocol_version": "1.0", "operation": "probe"}
    process = subprocess.run(
        [sys.executable, str(WORKER)], input=json.dumps(request), text=True,
        capture_output=True, env=env, check=False, timeout=15,
    )
    if process.returncode != 0:
        print("El probe del worker no terminó correctamente.")
        return 1
    response = json.loads(process.stdout)
    if response.get("ok") is not True or response.get("result", {}).get("managed_python") is not False:
        print("El worker no identificó la ausencia de un entorno Python de producto gestionado.")
        return 1
    print(json.dumps({
        "managed_python": response["result"]["managed_python"],
        "olive_ai_installed": response["result"]["olive_ai_installed"],
        "onnxruntime_installed": response["result"]["onnxruntime_installed"],
        "global_install_attempted": False,
    }, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
