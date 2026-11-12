"""Build the isolated OPENXLAB source worker package."""
from pathlib import Path
import sys

REPO = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(REPO / "adapters/source/p049"))
from package import main  # noqa: E402


if __name__ == "__main__":
    raise SystemExit(main("OPENXLAB"))
