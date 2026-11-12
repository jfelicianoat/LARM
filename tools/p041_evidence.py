#!/usr/bin/env python3
"""P041 - ModelBundle, artefactos requeridos e identidad canónica: pruebas y evidencia."""
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from evidence import Evidence, cargo_ok  # noqa: E402

PY = sys.executable


def main() -> int:
    ev = Evidence("P041")
    ev.run("P041-UNIT-bundle", "UNIT", ["cargo", "test", "-p", "larm-domain", "--lib", "--", "model::bundle_tests"],
           "hash del bundle determinista e independiente del orden; identidad solo con requeridos; rutas POSIX, duplicados "
           "sin mayúsculas, sha256 y rango; estados VERIFIED/INCOMPLETE/CORRUPT; roles necesarios por formato", ok=cargo_ok)
    ev.run("P041-UNIT-identidad", "UNIT", ["cargo", "test", "-p", "larm-models", "--lib", "--", "identity::tests"],
           "IDENTICAL por upstream o hashes+config+tokenizador; UNRELATED por arquitectura; derivada por base/hashes padre; "
           "PROBABLE sin fusión; nombre solo = UNKNOWN; serialización", ok=cargo_ok)
    tests = [
        ("P041-INTEGRATION-hashes-identicos", "hashes_identicos_agrupan", "otra fuente y nombre con los mismos bytes: un modelo; identidad sin opcionales"),
        ("P041-INTEGRATION-mismo-nombre", "mismo_nombre_modelo_distinto", "mismo nombre y otra arquitectura: UNRELATED, sin duplicado, motivos guardados"),
        ("P041-INTEGRATION-derivada", "cuantizacion_derivada_conserva_linaje", "cuantización con base_model y hashes padre: DERIVED_MODEL con linaje"),
        ("P041-INTEGRATION-probable", "probable_no_se_fusiona", "PROBABLE_SAME_MODEL no fusiona: possible_duplicates en los dos"),
        ("P041-INTEGRATION-probable-por-evidencia", "probable_por_evidencia_sin_nombre_ni_bytes_comunes", "candidatos por evidencia declarada (regresión del defecto hallado en real)"),
        ("P041-INTEGRATION-sin-evidencia", "sin_evidencia_suficiente_no_se_anota_duplicado", "UNKNOWN por falta de evidencia no se anota"),
        ("P041-INTEGRATION-bundle-en-disco", "bundle_verificado_incompleto_y_corrupto", "ficheros reales: VERIFIED, shard ausente INCOMPLETE, byte cambiado CORRUPT, opcional ausente VERIFIED; CAS; idempotente"),
        ("P041-INTEGRATION-completitud", "completitud_segun_variante_y_negativos", "rol mmproj faltante INCOMPLETE; sin hash no hay bundle; rutas no POSIX o duplicadas sin escribir; formato desconocido sin requisitos inventados"),
    ]
    for test_id, filt, expected in tests:
        ev.run(test_id, "INTEGRATION", ["cargo", "test", "-p", "larm-models", "--test", "bundles", "--", filt, "--exact"],
               expected, ok=cargo_ok)
    ev.run("P041-INTEGRATION-registro-p040", "INTEGRATION", ["cargo", "test", "-p", "larm-models", "--test", "registry"],
           "las reglas de P040 siguen con el registro ampliado", ok=cargo_ok)
    ev.run("P041-INTEGRATION-api", "INTEGRATION", ["cargo", "test", "-p", "larm-api", "--test", "models"],
           "GET /variants, /models/{id}/variants (404 si el modelo no existe), /model-bundles?state, /model-artifacts?q, "
           "detalle, filtros no previstos 400 y 404", ok=cargo_ok)
    ev.run("P041-UNIT-cargo-fmt", "UNIT", ["cargo", "fmt", "--all", "--check"], "sin diferencias de formato")
    ev.run("P041-UNIT-cargo-clippy", "UNIT", ["cargo", "clippy", "--workspace", "--all-targets", "--", "-D", "warnings"], "clippy sin avisos")
    ev.run("P041-UNIT-cargo-test", "UNIT", ["cargo", "test", "--workspace"], "pruebas del workspace en verde", ok=cargo_ok)
    ev.run("P041-REAL-architecture", "REAL",
           ["powershell", "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", "tools/check_architecture.ps1"], "grafo de crates intacto")
    ev.run("P041-REAL-build-daemon", "REAL", ["cargo", "build", "-p", "larm-daemon"], "daemon compilado")
    ev.run("P041-REAL-openapi", "REAL", [PY, "tools/p005_real.py", "openapi"],
           "docs/api/openapi.json al día con las 7 rutas de lectura de P041 implementadas")
    ev.run("P041-REAL-identidad-y-bundle", "REAL", [PY, "tools/p041_real.py", "serve"],
           "PC IA: IDENTICAL por bytes agrupa, DERIVED con linaje, PROBABLE sin fusión, UNRELATED aparte; bundle-verify "
           "sobre ficheros reales VERIFIED/INCOMPLETE/CORRUPT; rol faltante; API", timeout=900)
    ev.run("P041-REAL-regresion-p040", "REAL", [PY, "tools/p040_real.py", "serve"],
           "la prueba real de P040 sigue en verde con el registro ampliado", timeout=900,
           fixtures=[Path("tests/fixtures/models/import-multifuente.json")])
    changed = [
        "crates/larm-domain/src/model.rs", "crates/larm-domain/src/ports.rs", "crates/larm-db/src/variants.rs",
        "crates/larm-db/src/models.rs", "crates/larm-db/src/lib.rs", "crates/larm-models/src/bundle.rs",
        "crates/larm-models/src/identity.rs", "crates/larm-models/src/registry.rs", "crates/larm-models/src/lib.rs",
        "crates/larm-models/Cargo.toml", "crates/larm-models/tests/bundles.rs", "crates/larm-models/tests/registry.rs",
        "crates/larm-api/src/models.rs", "crates/larm-api/src/openapi.rs", "crates/larm-api/tests/models.rs",
        "apps/daemon/src/main.rs", "apps/daemon/src/serve.rs", "docs/api/openapi.json", "docs/operations/models.md",
        "tools/p041_real.py", "tools/p041_evidence.py",
    ]
    ev.finish(changed, [])
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
