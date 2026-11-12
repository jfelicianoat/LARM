#!/usr/bin/env python3
"""P040 - Dominio de modelos, variantes y procedencia: pruebas y evidencia."""
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from evidence import Evidence, cargo_ok  # noqa: E402

PY = sys.executable


def main() -> int:
    ev = Evidence("P040")
    ev.run("P040-UNIT-dominio", "UNIT", ["cargo", "test", "-p", "larm-domain", "--lib", "--", "model::tests"],
           "capacidades extensibles con alias y desconocidas no cualificadas; formatos/cuantizaciones como identificadores; "
           "claves de revisión upstream/pending/content; reglas de estado; roundtrip y campos desconocidos rechazados; "
           "clase de origen de la procedencia", ok=cargo_ok)
    tests = [
        ("P040-INTEGRATION-multifuente", "varias_distribuciones_un_modelo_canonico",
         "misma identidad upstream por dos fuentes = un modelo con dos distribuciones; filtros; metadata original; eventos en outbox"),
        ("P040-INTEGRATION-sin-fusion-por-nombre", "mismo_nombre_sin_upstream_no_se_fusiona",
         "mismo nombre sin upstream: dos modelos con possible_duplicates mutuos; reimportar no duplica"),
        ("P040-INTEGRATION-identidad", "identidad_inmutable_y_conflictos",
         "idempotente; revision_key inmutable; una CURRENT por distribución; publicación de otro modelo; pending solo PROVISIONAL; capacidad inválida sin escribir"),
        ("P040-INTEGRATION-roundtrip", "revision_roundtrip_y_paginacion", "roundtrip de revisión y paginación estable"),
        ("P040-INTEGRATION-procedencia", "procedencia_completa", "procedencia de §19 completa en roundtrip; linaje sin versión rechazado"),
    ]
    for test_id, filt, expected in tests:
        ev.run(test_id, "INTEGRATION", ["cargo", "test", "-p", "larm-models", "--test", "registry", "--", filt, "--exact"],
               expected, ok=cargo_ok)
    ev.run("P040-INTEGRATION-api", "INTEGRATION", ["cargo", "test", "-p", "larm-api", "--test", "models"],
           "GET /models, /model-distributions y /model-revisions: paginación con cursor ligado al filtro, alias de capacidad, "
           "filtros no previstos 400, id inválido 400, 404 y 401", ok=cargo_ok)
    ev.run("P040-UNIT-cargo-fmt", "UNIT", ["cargo", "fmt", "--all", "--check"], "sin diferencias de formato")
    ev.run("P040-UNIT-cargo-clippy", "UNIT", ["cargo", "clippy", "--workspace", "--all-targets", "--", "-D", "warnings"], "clippy sin avisos")
    ev.run("P040-UNIT-cargo-test", "UNIT", ["cargo", "test", "--workspace"], "pruebas del workspace en verde", ok=cargo_ok)
    ev.run("P040-REAL-architecture", "REAL",
           ["powershell", "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", "tools/check_architecture.ps1"], "grafo de crates intacto")
    ev.run("P040-REAL-build-daemon", "REAL", ["cargo", "build", "-p", "larm-daemon"], "daemon compilado")
    ev.run("P040-REAL-openapi", "REAL", [PY, "tools/p005_real.py", "openapi"],
           "docs/api/openapi.json al día con las 6 rutas de lectura de P040 implementadas")
    ev.run("P040-REAL-registro", "REAL", [PY, "tools/p040_real.py", "serve"],
           "PC IA: models-import multifuente agrupa por upstream y no por nombre, idempotente, revisión inmutable; "
           "API de lectura con filtros, alias, 404/400/401 y eventos", timeout=900,
           fixtures=[Path("tests/fixtures/models/import-multifuente.json")])
    changed = [
        "crates/larm-domain/src/model.rs", "crates/larm-domain/src/ports.rs", "crates/larm-domain/src/lib.rs",
        "crates/larm-db/src/models.rs", "crates/larm-db/src/lib.rs", "crates/larm-models/src/registry.rs",
        "crates/larm-models/src/lib.rs", "crates/larm-models/Cargo.toml", "crates/larm-models/tests/registry.rs",
        "crates/larm-api/src/models.rs", "crates/larm-api/src/lib.rs", "crates/larm-api/src/openapi.rs",
        "crates/larm-api/tests/models.rs", "apps/daemon/src/main.rs", "apps/daemon/src/serve.rs", "apps/daemon/Cargo.toml",
        "docs/api/openapi.json", "docs/operations/models.md", "tests/fixtures/models/import-multifuente.json",
        "tools/p040_real.py", "tools/p040_evidence.py",
    ]
    ev.finish(changed, [])
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
