#!/usr/bin/env python3
"""P020 - Adapter manifest y protocolo v1: pruebas y evidencia."""
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from evidence import Evidence, cargo_ok  # noqa: E402

PY = sys.executable


def main() -> int:
    ev = Evidence("P020")
    ev.run("P020-UNIT-manifiesto", "UNIT", ["cargo", "test", "-p", "larm-adapter-sdk", "--lib", "--", "manifest::tests"],
           "40 firmas del contrato; manifiesto válido con roundtrip exacto; capacidad desconocida conservada; estrategia no "
           "anunciada rechazada (422) antes del worker; negativos de schema, rutas, métodos, estrategias, secretos y orígenes",
           ok=cargo_ok)
    ev.run("P020-UNIT-protocolo", "UNIT", ["cargo", "test", "-p", "larm-adapter-sdk", "--lib", "--", "protocol::tests"],
           "framing NDJSON de una línea y 1 MiB; -32700/-32600; progreso de forma cerrada; error de dominio con LarmError; "
           "handshake con versión mayor y nonce exacto; traducción de initialize", ok=cargo_ok)
    ev.run("P020-INTEGRATION-sesion", "INTEGRATION", ["cargo", "test", "-p", "larm-adapter-sdk", "--test", "session"],
           "handshake obligatorio, versión incompatible en ambos sentidos, JSON mal formado sin cortar la sesión, métodos no "
           "anunciados -32601, parámetros inválidos, progreso, heartbeat y cancelación con una respuesta por id", ok=cargo_ok)
    ev.run("P020-INTEGRATION-ejemplos-rust", "INTEGRATION", ["cargo", "test", "-p", "larm-adapter-sdk", "--test", "examples"],
           "los tipos del SDK dan el mismo veredicto que el JSON Schema en los 24 ejemplos", ok=cargo_ok)
    ev.run("P020-UNIT-schemas", "UNIT", [PY, "tools/p020_schemas.py", "check"],
           "docs/adapters/schemas generados desde larm.schema.json y al día; ejemplos válidos pasan e inválidos fallan (jsonschema)")
    ev.run("P020-UNIT-cargo-fmt", "UNIT", ["cargo", "fmt", "--all", "--check"], "sin diferencias de formato")
    ev.run("P020-UNIT-cargo-clippy", "UNIT", ["cargo", "clippy", "--workspace", "--all-targets", "--", "-D", "warnings"], "clippy sin avisos")
    ev.run("P020-UNIT-cargo-test", "UNIT", ["cargo", "test", "--workspace"], "pruebas del workspace en verde", ok=cargo_ok)
    ev.run("P020-REAL-architecture", "REAL",
           ["powershell", "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", "tools/check_architecture.ps1"], "grafo de crates intacto")
    ev.run("P020-REAL-build-adaptador", "REAL", ["cargo", "build", "-p", "larm-adapter-sdk", "--example", "demo_adapter"],
           "adaptador de ejemplo compilado")
    ev.run("P020-REAL-stdio", "REAL", [PY, "tools/p020_real.py", "stdio"],
           "PC IA: host Python independiente contra el adaptador como proceso; cada frame validado con los schemas; manifiesto "
           "con sha256 real; handshake, -32700, -32601, progreso, heartbeat, cancel, stdout solo frames, versión 2.0 rechazada",
           timeout=600)
    changed = [
        "crates/larm-adapter-sdk/src/lib.rs", "crates/larm-adapter-sdk/src/manifest.rs", "crates/larm-adapter-sdk/src/protocol.rs",
        "crates/larm-adapter-sdk/src/server.rs", "crates/larm-adapter-sdk/Cargo.toml", "crates/larm-adapter-sdk/examples/demo_adapter.rs",
        "crates/larm-adapter-sdk/tests/session.rs", "crates/larm-adapter-sdk/tests/examples.rs",
        "tests/fixtures/adapters/demo-benchmark.manifest.json", "docs/adapters/protocol-v1.md", "docs/adapters/schemas",
        "docs/adapters/examples", "tools/p020_schemas.py", "tools/p020_real.py", "tools/p020_evidence.py",
    ]
    ev.finish(changed, [])
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
