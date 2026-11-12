#!/usr/bin/env python3
"""P002 - reglas de arquitectura, ADR y contratos de dependencia: pruebas y evidencia."""
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from evidence import REPO, Evidence, cargo_ok  # noqa: E402

PY = sys.executable


def main() -> int:
    ev = Evidence("P002", extra_inputs=["contracts/crate-graph.json"])
    graph = REPO / "docs/contracts/crate-graph.json"
    ev.run("P002-CONTRACT-graph-pristine", "CONTRACT", [PY, "tools/verify_p002.py", "graph-pristine"],
           "crate-graph.json del repo idéntico al del paquete v3", fixtures=[graph])
    ev.run("P002-CONTRACT-dependencies-doc", "CONTRACT", [PY, "tools/gen_dependencies_doc.py", "--check"],
           "docs/architecture/dependencies.md generado del contrato, sin deriva", fixtures=[graph])
    ev.run("P002-CONTRACT-adrs", "CONTRACT", [PY, "tools/verify_p002.py", "adrs"],
           "ADR 0001-0008 aceptados y alineados con contratos 00/03/07")
    ev.run("P002-CONTRACT-conventions", "CONTRACT", [PY, "tools/verify_p002.py", "conventions"],
           "código de producción sin UUIDv4 ni timestamps fuera de formato")
    ev.run("P002-FAULT-architecture-negatives", "FAULT", [PY, "tools/check_architecture.py", "--selftest"],
           "7 workspaces reales: domain con sqlx/windows/std::fs, runtime->core, crate fuera del grafo y GUI->crate "
           "interno fallan; el grafo válido pasa", fixtures=[graph])
    ev.run("P002-UNIT-cargo-fmt", "UNIT", ["cargo", "fmt", "--all", "--check"], "sin diferencias de formato")
    ev.run("P002-UNIT-cargo-clippy", "UNIT", ["cargo", "clippy", "--workspace", "--all-targets", "--", "-D", "warnings"],
           "clippy sin avisos")
    ev.run("P002-UNIT-cargo-test", "UNIT", ["cargo", "test", "--workspace"], "pruebas del workspace en verde", ok=cargo_ok)
    ev.run("P002-REAL-cargo-metadata", "REAL",
           ["powershell", "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", "tools/check_architecture.ps1"],
           "cargo metadata real del workspace (16 crates) y del shell Tauri conforme al contrato", fixtures=[graph])
    changed = [
        "docs/contracts/crate-graph.json", "docs/architecture/dependencies.md", "docs/adr/0007-identificadores-uuidv7.md",
        "docs/adr/0008-versionado-api.md", "crates/larm-domain/src/ports.rs", "crates/larm-domain/src/lib.rs",
        "crates/larm-domain/Cargo.toml", "crates/larm-core/src/bridge.rs", "crates/larm-runtime/Cargo.toml",
        "crates/larm-runtime/src/credentials_pipe.rs", "crates/larm-runtime/src/external_adoption.rs",
        "tools/check_architecture.py", "tools/check_architecture.ps1", "tools/gen_dependencies_doc.py",
        "tools/verify_p002.py", "tools/p002_evidence.py", "tools/gen_p002_docs.py (eliminado)",
    ]
    ev.finish(changed, [])
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
