#!/usr/bin/env python3
"""Freeze/check public source inputs and already-built native executables.

No build, private state, credentials or worker qualification is performed.
Usage: ... freeze|check CHECKPOINT EXECUTABLE [EXECUTABLE ...]
"""
import hashlib
import json
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parents[1]
TREES = (
    "apps", "crates", "adapters", "migrations", "tests/fixtures", "tools",
    "docs/contracts", "docs/spec", "docs/adapters", "docs/adr",
)
SUFFIXES = {".rs", ".toml", ".py", ".json", ".sql", ".md", ".lock", ".ps1", ".bat",
            ".ts", ".tsx", ".js", ".jsx", ".mjs", ".cjs", ".css", ".html", ".svg"}


def inputs(executables):
    files = {REPO / name for name in ("Cargo.toml", "Cargo.lock", "environment.lock.json", "docs/api/openapi.json")}
    for name in TREES:
        files.update(path for path in (REPO / name).rglob("*") if path.is_file()
                     and path.suffix in SUFFIXES
                     and not any(part in {"target", "node_modules", "__pycache__", ".git"}
                                 or part.startswith(".venv") for part in path.relative_to(REPO).parts))
    for name in executables:
        path = (REPO / name).resolve()
        path.relative_to(REPO)
        if path.suffix != ".exe" or not path.is_file():
            raise ValueError("An already-built executable in this checkout is required")
        files.add(path)
    return {path.relative_to(REPO).as_posix(): hashlib.sha256(path.read_bytes()).hexdigest()
            for path in sorted(files)}


def main():
    mode, name, *executables = sys.argv[1:]
    if mode not in {"freeze", "check"} or not executables:
        raise ValueError(__doc__)
    checkpoint = Path(name)
    current = inputs(executables)
    if mode == "freeze":
        checkpoint.parent.mkdir(parents=True, exist_ok=True)
        with checkpoint.open("x", encoding="utf-8", newline="\n") as stream:
            json.dump({"scope": "Public implementation/contract/fixture inputs and built executables; no private state or qualification", "files": current}, stream, ensure_ascii=False, indent=2)
            stream.write("\n")
    else:
        expected = json.loads(checkpoint.read_text(encoding="utf-8"))["files"]
        changed = sorted(key for key in current.keys() | expected.keys()
                         if current.get(key) != expected.get(key))
        if changed:
            print(json.dumps({"status": "FAIL", "changed_public_paths": changed}))
            return 1
    print(json.dumps({"status": "PASS", "mode": mode, "public_inputs_and_executables": len(current), "executables": len(executables)}))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
