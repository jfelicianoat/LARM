#!/usr/bin/env python3
"""P005 - API administrativa Axum v1 y OpenAPI: pruebas y evidencia."""
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from evidence import REPO, Evidence, cargo_ok  # noqa: E402

PY = sys.executable


def main() -> int:
    ev = Evidence("P005")
    fx = [REPO / "docs/contracts/api-operations.json", REPO / "docs/contracts/schemas/larm.schema.json",
          REPO / "docs/contracts/schemas/domain.schema.json"]
    app_tests = [
        ("P005-INTEGRATION-health", "live_publico", "live público; ready exige read; READY/DEGRADED; 503 si no se comprueba"),
        ("P005-CONTRACT-envelope", "envelope_en_rutas", "404 con envelope del contrato para rutas desconocidas y no implementadas; 413 -> 400 body_too_large"),
        ("P005-FAULT-timeout", "timeout_devuelve", "timeout -> 504 TIMEOUT reintentable sin esperar al handler"),
        ("P005-CONTRACT-cors", "cors_desactivado", "CORS desactivado por defecto; con orígenes, solo coincidencia exacta"),
        ("P005-INTEGRATION-gateway", "gateway_fuera_del_guard", "el gateway de inferencia queda fuera del guard y sin normalizar"),
        ("P005-INTEGRATION-sse", "sse_conecta", "SSE exige read, entrega eventos del bus y libera la suscripción al cerrar"),
        ("P005-FAULT-sse-overflow", "sse_cola_desbordada", "cola de 1024 desbordada -> resync_required y cierre"),
        ("P005-FAULT-sse-shutdown", "cerrar_el_bus", "cerrar el bus termina los streams (apagado ordenado)"),
    ]
    for test_id, filt, expected in app_tests:
        kind = test_id.split("-")[1]
        ev.run(test_id, kind, ["cargo", "test", "-p", "larm-api", "--test", "app", "--", filt], expected, ok=cargo_ok, fixtures=fx)
    ev.run("P005-CONTRACT-openapi", "CONTRACT", ["cargo", "test", "-p", "larm-api", "--lib", "openapi"],
           "142 operaciones, sin referencias rotas, Search en las dos rutas, ids únicos, determinista, implementadas en el contrato",
           ok=cargo_ok, fixtures=fx)
    ev.run("P005-CONTRACT-search-h05", "CONTRACT", ["cargo", "test", "-p", "larm-domain", "--lib", "search"],
           "Search acepta todos los filtros/rangos/toggles y rechaza propiedades, tipos y rangos inválidos; digest sin cursor/limit",
           ok=cargo_ok, fixtures=fx)
    ev.run("P005-UNIT-cargo-fmt", "UNIT", ["cargo", "fmt", "--all", "--check"], "sin diferencias de formato")
    ev.run("P005-UNIT-cargo-clippy", "UNIT", ["cargo", "clippy", "--workspace", "--all-targets", "--", "-D", "warnings"], "clippy sin avisos")
    ev.run("P005-UNIT-cargo-test", "UNIT", ["cargo", "test", "--workspace"], "pruebas del workspace en verde", ok=cargo_ok)
    ev.run("P005-REAL-daemon-build", "REAL", ["cargo", "build", "-p", "larm-daemon"], "binario del daemon compilado")
    ev.run("P005-REAL-openapi", "REAL", [PY, "tools/p005_real.py", "openapi"],
           "docs/api/openapi.json coincide con el generado y valida con openapi-spec-validator", fixtures=fx)
    ev.run("P005-REAL-bind", "REAL", [PY, "tools/p005_real.py", "bind"],
           "bind 0.0.0.0 sin remote_enabled no arranca ni escucha")
    ev.run("P005-REAL-api", "REAL", [PY, "tools/p005_real.py", "serve"],
           "daemon real: escucha solo en 127.0.0.1, live/ready, envelope y correlación, límite de cuerpo, CORS off, "
           "SSE con evento de una mutación real, apagado con SSE abierto, log de peticiones sin secretos", timeout=600)
    changed = [
        "crates/larm-api/src/app.rs", "crates/larm-api/src/openapi.rs", "crates/larm-api/src/lib.rs",
        "crates/larm-api/src/broker.rs", "crates/larm-api/src/broker/tests.rs", "crates/larm-api/src/security.rs",
        "crates/larm-api/tests/app.rs", "crates/larm-api/Cargo.toml", "crates/larm-domain/src/search.rs",
        "crates/larm-domain/src/lib.rs", "crates/larm-db/src/events.rs", "apps/daemon/src/serve.rs",
        "apps/daemon/src/main.rs", "apps/daemon/Cargo.toml", "docs/api/openapi.json", "tools/p005_real.py",
        "tools/p005_evidence.py", "tools/requirements.txt",
    ]
    ev.finish(changed, [])
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
