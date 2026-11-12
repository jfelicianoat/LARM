#!/usr/bin/env python3
"""Ejecuta todas las suites unittest de Python del repo (tools y adaptadores).

Cada suite corre en su propio proceso con su raíz de importación: las de tools/
importan módulos hermanos y las de adapters/runtime/<id>/tests importan su worker.
Sin esta puerta, un oráculo Python desfasado del de Rust pasaba inadvertido.
"""
import os
import subprocess
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent


def suites() -> list[tuple[str, Path, Path]]:
    found = [("tools", REPO / "tools", REPO / "tools")]
    for tests in sorted((REPO / "adapters").glob("*/*/tests")):
        if any(tests.glob("test_*.py")):
            # tests/ no es paquete: es su propia raíz y el cwd del adaptador aporta el worker.
            found.append((tests.parent.relative_to(REPO).as_posix(), tests, tests))
    return found


def main() -> int:
    env = {**os.environ, "PYTHONIOENCODING": "utf-8"}
    failed = []
    for name, start, top in suites():
        print(f"==> {name}", flush=True)
        result = subprocess.run(
            [sys.executable, "-m", "unittest", "discover", "-s", str(start), "-t", str(top), "-p", "test_*.py"],
            cwd=top if name == "tools" else start.parent,
            env=env,
            check=False,
        )
        if result.returncode != 0:
            failed.append(name)
    if failed:
        print(f"FAIL: {', '.join(failed)}", flush=True)
        return 1
    print(f"PASS: {len(suites())} suites", flush=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
