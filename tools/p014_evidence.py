#!/usr/bin/env python3
"""P014 - StorageLocation registry y políticas de colocación: pruebas y evidencia."""
import shutil
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from evidence import REPO, Evidence, cargo_ok  # noqa: E402

PY = sys.executable
NPM = shutil.which("npm.cmd") or "npm"


def main() -> int:
    ev = Evidence("P014")
    lib = [
        ("P014-UNIT-niveles-h09", "larm-domain", "locations::tests::niveles", "PERCENT y BYTES; igualdad no cruza; sin capacidad no hay nivel; sin desbordamiento"),
        ("P014-UNIT-espacio-reservado", "larm-domain", "locations::tests::espacio", "usable = libre - reserva, nunca negativo; volumen: mayor reserva, no la suma"),
        ("P014-UNIT-politicas", "larm-domain", "locations::tests::politicas",
         "ASK_ALWAYS, PREFERRED_LOCATION, MOST_FREE_SPACE, FASTEST_STORAGE, BY_MODEL_TYPE y CUSTOM_RULES (solo modelo); empates por prioridad e id"),
        ("P014-UNIT-hard-gates", "larm-domain", "locations::tests::hard_gates", "LOW_SPACE, sin auto-placement, identidad desconocida y FAT32 > 4 GiB excluidos"),
        ("P014-CONTRACT-dto", "larm-domain", "locations::tests::registro", "DTO de escritura cerrado: GUID no editable; estados del schema"),
        ("P014-UNIT-estados", "larm-domain", "storage::tests::estado", "OFFLINE, READ_ONLY, MISSING, UNKNOWN, LOW_SPACE y ONLINE según lo observado"),
        ("P014-UNIT-sintaxis-raiz", "larm-storage", "locations::tests::sintaxis", "relativa, ADS, '..', dispositivo, GLOBALROOT y espacios rechazados"),
        ("P014-UNIT-solapamiento", "larm-storage", "locations::tests::solapamiento", "por componentes y sin distinguir mayúsculas; C:\\IA no contiene C:\\IA2"),
        ("P014-FAULT-raiz-real", "larm-storage", "locations::tests::raiz_real",
         "carpeta inexistente, fichero y junction real (mklink /J) rechazados; el fichero de prueba de escritura se borra"),
        ("P014-UNIT-presupuesto", "larm-storage", "locations::tests::presupuesto", "presupuesto compartido por volumen; offline sin espacio"),
    ]
    for test_id, crate, filt, expected in lib:
        kind = test_id.split("-")[1]
        ev.run(test_id, kind, ["cargo", "test", "-p", crate, "--lib", "--", filt], expected, ok=cargo_ok)
    ev.run("P014-INTEGRATION-sqlite-crud", "INTEGRATION", ["cargo", "test", "-p", "larm-db", "--test", "locations"],
           "CRUD con ida y vuelta exacta, If-Match, NOT_FOUND, eventos created/updated/deleted en outbox; borrado con artefactos "
           "rechazado sin evento; paginación y filtros q/state", ok=cargo_ok)
    ev.run("P014-INTEGRATION-sqlite-estado", "INTEGRATION", ["cargo", "test", "-p", "larm-db", "--test", "storage"],
           "volumen offline, MISSING, LOW_SPACE con igualdad que no cruza, observaciones simultáneas", ok=cargo_ok)
    ev.run("P014-INTEGRATION-api", "INTEGRATION", ["cargo", "test", "-p", "larm-api", "--test", "storage"],
           "guard real: 201 idempotente y 409 con otro cuerpo, 428/409 de If-Match, 409 con artefactos, 204; schema cerrado; "
           "403 sin storage:write; alias /storage; filtros y cursor ligado a los filtros; sondeo como job", ok=cargo_ok)
    ev.run("P014-CONTRACT-openapi", "CONTRACT",
           [str(REPO / "target/debug/larm-daemon.exe"), "openapi", "--out", "docs/api/openapi.json", "--check"],
           "OpenAPI al día con las 8 operaciones de P014")
    ev.run("P014-UNIT-cargo-fmt", "UNIT", ["cargo", "fmt", "--all", "--check"], "sin diferencias de formato")
    ev.run("P014-UNIT-cargo-clippy", "UNIT", ["cargo", "clippy", "--workspace", "--all-targets", "--", "-D", "warnings"], "clippy sin avisos")
    ev.run("P014-UNIT-cargo-test", "UNIT", ["cargo", "test", "--workspace"], "pruebas del workspace en verde", ok=cargo_ok)
    for script, expected in (("lint", "eslint sin avisos"), ("typecheck", "TypeScript sin errores"),
                             ("test", "vitest: Almacenamiento vacía, estados, presupuesto compartido, alta con bytes exactos, "
                                      "edición con If-Match, quitar con confirmación y análisis"),
                             ("build", "build de producción del frontend")):
        ev.run(f"P014-UNIT-desktop-{script}", "UNIT", [NPM, "run", script, "--silent"], expected)
    ev.run("P014-REAL-architecture", "REAL",
           ["powershell", "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", "tools/check_architecture.ps1"],
           "larm-storage solo depende de larm-domain; la API llega por puertos")
    ev.run("P014-REAL-daemon", "REAL", [PY, "tools/p014_real.py", "serve"],
           "PC IA: alta con GUID y capacidad reales; 6 rechazos antes de guardar (junction incluida); presupuesto compartido; "
           "reserva > libre = 0; UNC; MISSING real; If-Match; artefactos protegidos; eventos; LOW_SPACE real; limpieza",
           timeout=1200)
    ev.run("P014-REAL-p012-sin-regresion", "REAL", [PY, "tools/p012_real.py", "serve"],
           "el último estado de P012 sigue funcionando con MISSING y umbrales", timeout=900)
    ev.blocked("P014-REAL-volumen-offline", "REAL",
               "registrar una ubicación en un segundo volumen (disco externo o VHDX) y desconectarlo",
               "la ubicación pasa a OFFLINE conservando sus datos y last_seen_at; al volver, ONLINE",
               "el PC IA solo tiene un volumen escribible (bloqueo de P000: segundo volumen con GUID propio). "
               "OFFLINE está probado con observaciones en SQLite real; crear un VHDX necesita autorización del operador")
    changed = [
        "crates/larm-domain/src/locations.rs", "crates/larm-domain/src/storage.rs", "crates/larm-domain/src/ports.rs",
        "crates/larm-domain/src/lib.rs", "crates/larm-storage/src/locations.rs", "crates/larm-storage/src/lib.rs",
        "crates/larm-storage/Cargo.toml", "crates/larm-db/src/locations.rs", "crates/larm-db/src/jobs.rs",
        "crates/larm-db/src/storage.rs", "crates/larm-db/src/hardware.rs", "crates/larm-db/src/lib.rs",
        "crates/larm-db/tests/locations.rs", "crates/larm-db/tests/storage.rs", "crates/larm-db/tests/hardware.rs",
        "crates/larm-core/src/locations.rs", "crates/larm-core/src/hardware.rs", "crates/larm-core/src/lib.rs",
        "crates/larm-api/src/storage.rs", "crates/larm-api/src/openapi.rs", "crates/larm-api/src/lib.rs",
        "crates/larm-api/tests/storage.rs", "crates/larm-api/tests/hardware.rs", "apps/daemon/src/hardware.rs",
        "apps/daemon/src/serve.rs", "apps/daemon/Cargo.toml", "docs/api/openapi.json", "docs/operations/storage.md",
        "apps/desktop/src/storage.tsx", "apps/desktop/src/storage.test.tsx", "apps/desktop/src/App.tsx",
        "apps/desktop/src/styles.css", "tools/p014_real.py", "tools/p014_evidence.py",
    ]
    ev.finish(changed, [])
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
