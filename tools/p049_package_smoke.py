"""Build and exercise a self-contained P049 Civitai worker offline."""
from __future__ import annotations

import json

from p049_worker import run


if __name__ == "__main__":
    output = run(live=False)
    print(json.dumps(output, ensure_ascii=False))
    raise SystemExit(0 if output["status"] == "PASS" else 1)
