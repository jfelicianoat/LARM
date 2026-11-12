#!/usr/bin/env python3
"""P002 - Decisiones de arquitectura cerradas (exit 0 = PASS).

  adrs           los ADR exigidos existen y fijan lo que dicen los contratos (UUIDv7, RFC3339 Z con
                 milisegundos, /api/v1, Adapter Protocol "1.0", SQLite WAL, side-by-side...)
  conventions    el código de producción cumple esas decisiones: sin Uuid::new_v4 ni to_rfc3339()
                 (que emite +00:00 y nanosegundos) fuera de tests
  graph-pristine crate-graph.json es byte a byte el del paquete v3
"""
import hashlib
import re
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
PACKAGE = REPO.parent / "LARM_prompts_v3"
ADRS = {
    "0001-windows-first.md": ["Windows"],
    "0002-separacion-larm-ai-broker.md": ["AI Broker"],
    "0003-adaptadores-fuera-de-proceso.md": ["fuera de proceso"],
    "0004-multidisco-content-addressed.md": ["GUID"],
    "0005-actualizaciones-side-by-side.md": ["side-by-side"],
    "0006-sqlite-wal.md": ["WAL", "foreign_keys"],
    "0007-identificadores-uuidv7.md": ["UUID v7", "milisegundos", "RFC 8785", "string decimal"],
    "0008-versionado-api.md": ["/api/v1", '"1.0"', "schema_version = 1"],
    "0009-excepcion-unsafe-dacl-pipes.md": ["winsec.rs", "D:P"],
    "0010-excepcion-unsafe-sondeo-nativo.md": ["native.rs", "probe-accelerators"],
}
VIOLATIONS = {
    r"Uuid::new_v4\(": "UUIDv4 en lugar de UUIDv7 (ADR-0007)",
    r"\.to_rfc3339\(\)": "to_rfc3339() emite +00:00 y nanosegundos; el contrato exige Z y milisegundos (ADR-0007)",
}


def adrs() -> str:
    problems = []
    for name, needles in ADRS.items():
        path = REPO / "docs/adr" / name
        if not path.is_file():
            problems.append(f"falta {name}")
            continue
        text = path.read_text(encoding="utf-8")
        problems += [f"{name} no fija {n!r}" for n in needles if n not in text]
        if "## Decisión" not in text or "**Estado**: aceptado" not in text:
            problems.append(f"{name} sin decisión aceptada")
    if problems:
        raise ValueError("; ".join(problems))
    return f"{len(ADRS)} ADR aceptados con las decisiones del contrato"


def production_lines(path: Path):
    """Líneas de código de producción: excluye tests.rs, tests/ y lo que sigue a #[cfg(test)]."""
    if path.name == "tests.rs" or "tests" in path.relative_to(REPO).parts:
        return
    for n, line in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
        if line.strip().startswith("#[cfg(test)]"):
            return
        yield n, line.split("//")[0]


def conventions() -> str:
    hits = []
    files = [p for root in ("crates", "apps/daemon/src") for p in (REPO / root).rglob("*.rs")]
    for path in files:
        for n, line in production_lines(path):
            for pattern, why in VIOLATIONS.items():
                if re.search(pattern, line):
                    hits.append(f"{path.relative_to(REPO).as_posix()}:{n} {why}")
    if hits:
        raise ValueError("; ".join(hits))
    return f"{len(files)} ficheros de producción sin UUIDv4 ni to_rfc3339()"


def graph_pristine() -> str:
    a = hashlib.sha256((REPO / "docs/contracts/crate-graph.json").read_bytes()).hexdigest()
    b = hashlib.sha256((PACKAGE / "contracts/crate-graph.json").read_bytes()).hexdigest()
    if a != b:
        raise ValueError(f"crate-graph.json del repo ({a[:12]}) difiere del paquete ({b[:12]})")
    return f"crate-graph.json idéntico al paquete (sha256 {a})"


COMMANDS = {"adrs": adrs, "conventions": conventions, "graph-pristine": graph_pristine}

if __name__ == "__main__":
    try:
        print("PASS " + COMMANDS[sys.argv[1]]())
        raise SystemExit(0)
    except (ValueError, KeyError, IndexError, OSError) as e:
        print(f"FAIL {type(e).__name__}: {e}")
        raise SystemExit(1)
