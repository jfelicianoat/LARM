"""Query one public Civitai model page through the isolated Windows worker."""
from __future__ import annotations

import json

from p049_worker import run


if __name__ == "__main__":
    output = run(live=True)
    print(json.dumps(output, ensure_ascii=False))
    raise SystemExit(0 if output["status"] == "PASS" else 2 if output["status"] == "BLOCKED" else 1)
