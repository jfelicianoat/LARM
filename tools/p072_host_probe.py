#!/usr/bin/env python3
"""Read-only P072 probe; it must not import Diffusers or Torch on unmanaged Python."""
from __future__ import annotations

import json
import platform
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "adapters" / "runtime" / "diffusers"))
import worker  # noqa: E402


def main() -> int:
    managed = worker._managed_interpreter()
    loaded = sorted(name for name in ("torch", "diffusers") if name in sys.modules)
    result = {
        "managed_python": managed,
        "python_version": sys.version.split()[0],
        "python_prefix": str(Path(sys.prefix).resolve()),
        "platform": platform.platform(),
        "torch_or_diffusers_imported": loaded,
    }
    print(json.dumps({"ok": not loaded and not managed, "result": result}, sort_keys=True))
    return 0 if not loaded and not managed else 1


if __name__ == "__main__":
    raise SystemExit(main())
