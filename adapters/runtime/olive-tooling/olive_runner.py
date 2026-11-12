#!/usr/bin/env python3
"""Child process that invokes only the locked Olive optimization entry point."""
from __future__ import annotations

import sys
from pathlib import Path


def main() -> int:
    if len(sys.argv) != 5:
        return 2
    input_path, output_path = Path(sys.argv[1]), Path(sys.argv[2])
    provider, precision = sys.argv[3], sys.argv[4]
    if not input_path.is_absolute() or not output_path.is_absolute() or not input_path.is_file() or not output_path.is_dir():
        return 2
    try:
        import olive

        optimize = getattr(olive, "optimize", None)
        if not callable(optimize):
            return 3
        optimize(
            model_name_or_path=str(input_path),
            output_path=str(output_path),
            provider=provider,
            precision=precision,
            trust_remote_code=False,
        )
    except Exception:
        return 4
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
