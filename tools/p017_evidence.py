#!/usr/bin/env python3
"""P017 - Content addressed store, deduplicación e importación: pruebas y evidencia."""
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from evidence import Evidence, cargo_ok  # noqa: E402

PY = sys.executable


def main() -> int:
    ev = Evidence("P017")
    ev.run("P017-UNIT-cas", "UNIT", ["cargo", "test", "-p", "larm-storage", "--lib", "--", "cas::tests"],
           "SHA-256 en streaming; caché por tamaño+mtime que se descarta al cambiar; blob verificado, de solo lectura y sin "
           "duplicar; hash falso descartado sin blob; vista con hard links e idempotente", ok=cargo_ok)
    ev.run("P017-UNIT-recorrido", "UNIT", ["cargo", "test", "-p", "larm-storage", "--lib", "--", "import::tests"],
           "recorrido determinista con cabeceras acotadas; junction (mklink /J) no seguida e informada", ok=cargo_ok)
    ev.run("P017-UNIT-escaner", "UNIT", ["cargo", "test", "-p", "larm-models", "--lib", "--", "scan::tests"],
           "cabecera GGUF con límites; detección por contenido de GGUF/SafeTensors/índices/ONNX/external_data/PyTorch (pickle "
           "marcado)/config/tokenizer; desconocidos ignorados con motivo; agrupación determinista en variantes", ok=cargo_ok)
    tests = [
        ("P017-INTEGRATION-carpeta-mixta", "carpeta_mixta_adoptada_sin_tocar_nada",
         "adopción de carpeta mixta: 3 variantes EXTERNAL/ADOPTED, desconocidos informados, ni un byte ni una fecha cambiados"),
        ("P017-INTEGRATION-cache", "reimportar_usa_la_cache_y_no_duplica",
         "reimportación desde la caché sin filas nuevas; cambio releído; revisión anterior SUPERSEDED; caché no verifica"),
        ("P017-INTEGRATION-duplicados", "duplicados_por_hash_sin_borrar", "duplicado por hash con ahorro posible; nada borrado"),
        ("P017-INTEGRATION-cas-y-vista", "copia_al_cas_compartido_y_vista",
         "copia al CAS: blobs inmutables, compartidos SHARED, original intacto, vista con enlaces, repetir no copia"),
        ("P017-INTEGRATION-volumenes", "entre_volumenes_no_se_asume_ahorro_fisico",
         "copias en volúmenes distintos: duplicate_bytes sí, reclaimable 0"),
        ("P017-INTEGRATION-negativos", "negativos", "carpeta fuera de la ubicación, propiedad inválida al adoptar, carpeta inexistente; HOST_MANAGED"),
    ]
    for test_id, filt, expected in tests:
        ev.run(test_id, "INTEGRATION", ["cargo", "test", "-p", "larm-models", "--test", "import", "--", filt, "--exact"],
               expected, ok=cargo_ok)
    ev.run("P017-INTEGRATION-modelos", "INTEGRATION", ["cargo", "test", "-p", "larm-models"],
           "registro, bundles e identidad siguen en verde con la sustitución de revisiones", ok=cargo_ok)
    ev.run("P017-UNIT-cargo-fmt", "UNIT", ["cargo", "fmt", "--all", "--check"], "sin diferencias de formato")
    ev.run("P017-UNIT-cargo-clippy", "UNIT", ["cargo", "clippy", "--workspace", "--all-targets", "--", "-D", "warnings"], "clippy sin avisos")
    ev.run("P017-UNIT-cargo-test", "UNIT", ["cargo", "test", "--workspace"], "pruebas del workspace en verde", ok=cargo_ok)
    ev.run("P017-REAL-architecture", "REAL",
           ["powershell", "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", "tools/check_architecture.ps1"], "grafo de crates intacto")
    ev.run("P017-REAL-build-daemon", "REAL", ["cargo", "build", "-p", "larm-daemon"], "daemon compilado")
    ev.run("P017-REAL-importacion", "REAL", [PY, "tools/p017_real.py", "serve"],
           "PC IA: adopción de carpeta mixta sin tocar nada, caché, duplicados, copia al CAS con blobs inmutables y "
           "compartidos, vista con hard links en C: y referencias en \\\\localhost\\Temp", timeout=900)
    ev.run("P017-REAL-regresion-p041", "REAL", [PY, "tools/p041_real.py", "serve"],
           "bundles e identidad (P041) siguen en verde", timeout=900)
    changed = [
        "crates/larm-storage/src/cas.rs", "crates/larm-storage/src/import.rs", "crates/larm-storage/src/lib.rs",
        "crates/larm-models/src/scan.rs", "crates/larm-models/src/import.rs", "crates/larm-models/src/registry.rs",
        "crates/larm-models/src/lib.rs", "crates/larm-models/Cargo.toml", "crates/larm-models/tests/import.rs",
        "crates/larm-domain/src/ports.rs", "crates/larm-db/src/artifacts.rs", "crates/larm-db/src/models.rs",
        "crates/larm-api/tests/artifacts.rs", "crates/larm-api/tests/models.rs", "apps/daemon/src/main.rs",
        "docs/operations/import-cas.md", "tools/p017_real.py", "tools/p017_evidence.py",
    ]
    ev.finish(changed, [])
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
