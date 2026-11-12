#!/usr/bin/env python3
"""P018 - Storage analyzer, volúmenes offline y rebuild registry: pruebas y evidencia."""
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from evidence import Evidence, cargo_ok  # noqa: E402

PY = sys.executable


def main() -> int:
    ev = Evidence("P018")
    ev.run("P018-UNIT-niveles-h09", "UNIT", ["cargo", "test", "-p", "larm-domain", "--lib", "--", "locations::tests"],
           "NORMAL/WARNING/CRITICAL por volumen sin duplicar raíces, observación más reciente, PERCENT y BYTES, igualdad sin "
           "cruce y transiciones una vez por volumen", ok=cargo_ok)
    ev.run("P018-UNIT-analizador", "UNIT", ["cargo", "test", "-p", "larm-storage", "--lib", "--", "analyzer::tests"],
           "categorías MODELS/PARTIALS/TEMPORARIES/CACHES/RUNTIMES/ORPHANS; job vivo bloquea; copias EXTERNAL y ficheros del "
           "usuario nunca recuperables; determinista", ok=cargo_ok)
    ev.run("P018-UNIT-manifiestos", "UNIT", ["cargo", "test", "-p", "larm-storage", "--lib", "--", "rebuild::tests"],
           "manifiestos atómicos; blobs releídos OK/MISSING/CORRUPT; manifiesto ilegible informado; blobs sin citar", ok=cargo_ok)
    tests = [
        ("P018-INTEGRATION-offline-online", "volumen_offline_y_vuelta_con_reverificacion",
         "OFFLINE sin borrar ni tocar la etapa; de vuelta, bytes releídos y disponibilidad restaurada; CORRUPT; MISSING"),
        ("P018-INTEGRATION-huerfanos-y-parciales", "huerfanos_parciales_y_limpieza_aprobada",
         "blob huérfano y parcial de job terminado recuperables; job vivo y fichero del usuario intactos; plan de un uso; lo que cambió se salta"),
        ("P018-INTEGRATION-borrado", "borrar_una_variante",
         "DELETE_FILE/KEEP_SHARED/DEREGISTER; If-Match; ACTIVE_LEASE; variant_in_use sin borrar nada; manifiesto retirado"),
        ("P018-INTEGRATION-rebuild", "reconstruir_el_registro_desde_manifiestos",
         "reconstrucción en staging: completa recuperada, incompleta informada, blob sin citar; la base viva intacta"),
    ]
    for test_id, filt, expected in tests:
        ev.run(test_id, "INTEGRATION", ["cargo", "test", "-p", "larm-models", "--test", "maintenance", "--", filt, "--exact"],
               expected, ok=cargo_ok)
    ev.run("P018-INTEGRATION-restore-instalacion-nueva", "INTEGRATION",
           ["cargo", "test", "-p", "larm-db", "--test", "integration", "--", "restore_en_una_instalacion_nueva_sin_carpeta"],
           "defecto heredado de P004 corregido: restore en una instalación sin carpeta de datos", ok=cargo_ok)
    ev.run("P018-INTEGRATION-api", "INTEGRATION", ["cargo", "test", "-p", "larm-api", "--test", "maintenance"],
           "scopes storage:write/models:write, Probe, ExecutePlan, aprobación obligatoria ligada al plan, If-Match 428, "
           "plan_hash requerido, parámetros no previstos", ok=cargo_ok)
    ev.run("P018-UNIT-cargo-fmt", "UNIT", ["cargo", "fmt", "--all", "--check"], "sin diferencias de formato")
    ev.run("P018-UNIT-cargo-clippy", "UNIT", ["cargo", "clippy", "--workspace", "--all-targets", "--", "-D", "warnings"], "clippy sin avisos")
    ev.run("P018-UNIT-cargo-test", "UNIT", ["cargo", "test", "--workspace"], "pruebas del workspace en verde", ok=cargo_ok)
    ev.run("P018-REAL-architecture", "REAL",
           ["powershell", "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", "tools/check_architecture.ps1"], "grafo de crates intacto")
    ev.run("P018-REAL-build-daemon", "REAL", ["cargo", "build", "-p", "larm-daemon"], "daemon compilado")
    ev.run("P018-REAL-openapi", "REAL", [PY, "tools/p005_real.py", "openapi"],
           "docs/api/openapi.json al día con las 5 operaciones de P018 implementadas")
    ev.run("P018-REAL-mantenimiento", "REAL", [PY, "tools/p018_real.py", "serve"],
           "PC IA: copia con manifiestos, análisis, limpieza aprobada, borrado de variante, raíz que desaparece y vuelve con "
           "bytes releídos, reconstrucción en staging y db-restore en otra carpeta servido por el daemon", timeout=900)
    ev.run("P018-REAL-regresion-p017", "REAL", [PY, "tools/p017_real.py", "serve"],
           "importación y CAS (P017) siguen en verde con los manifiestos", timeout=900)
    ev.blocked("P018-REAL-disco-desconectado", "REAL",
               "desconectar físicamente un disco con modelos y volver a conectarlo",
               "artefactos OFFLINE y variante OFFLINE sin borrar nada; al volver, bytes releídos y disponibilidad restaurada",
               "el PC IA no tiene un segundo volumen ni un disco extraíble (bloqueo de P000); se probó con la raíz que "
               "desaparece (MISSING) y con estados OFFLINE en integración")
    changed = [
        "crates/larm-storage/src/reconcile.rs", "crates/larm-storage/src/analyzer.rs", "crates/larm-storage/src/cleanup.rs",
        "crates/larm-storage/src/rebuild.rs", "crates/larm-storage/src/lib.rs", "crates/larm-models/src/rebuild.rs",
        "crates/larm-models/src/import.rs", "crates/larm-models/src/lib.rs", "crates/larm-models/Cargo.toml",
        "crates/larm-models/tests/maintenance.rs", "crates/larm-domain/src/ports.rs", "crates/larm-domain/src/locations.rs",
        "crates/larm-db/src/artifacts.rs", "crates/larm-db/src/variants.rs", "crates/larm-db/src/models.rs",
        "crates/larm-db/src/backup.rs", "crates/larm-db/tests/integration.rs", "crates/larm-api/src/maintenance.rs",
        "crates/larm-api/src/lib.rs", "crates/larm-api/src/openapi.rs", "crates/larm-api/tests/maintenance.rs",
        "apps/daemon/src/main.rs", "apps/daemon/src/serve.rs", "apps/daemon/src/hardware.rs", "docs/api/openapi.json",
        "docs/operations/storage-maintenance.md", "tools/p018_real.py", "tools/p018_evidence.py",
    ]
    ev.finish(changed, [])
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
