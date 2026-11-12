#!/usr/bin/env python3
"""P043: SDK source, catálogo transaccional, API/worker Windows e interfaz."""
import shutil
import sys
from pathlib import Path

from evidence import Evidence, REPO, cargo_ok


def main():
    ev = Evidence("P043")
    ev.run("P043-REAL-build", "REAL", ["cargo", "build", "-p", "larm-daemon", "-p", "larm-adapter-host", "--example", "fake_source", "--bin", "larm-daemon"],
           "daemon y fixture de protocolo compilados; la fixture no simula una descarga ni una cualificación remota")
    groups = [
        ("source-boundary", "larm-adapter-sdk", ["--test", "source"], "7 pruebas: SourceSearch exacto, negociación, Search completo conservado, filtros diferidos y digest"),
        ("source-protocol", "larm-adapter-sdk", ["--test", "source_protocol"], "11 pruebas: nueve firmas, nullable required, metadata/429, paginación, manifest y plan/hashes/presupuesto"),
        ("full-search", "larm-domain", ["search::tests"], "5 pruebas de Search y todos sus campos/rangos/toggles, sin perder precisión ni required nullable"),
        ("catalog-dto", "larm-domain", ["sources::tests"], "2 pruebas: DTO cerrado y datos públicos sin credenciales ni IDs/tipos/límites inválidos"),
        ("locator", "larm-core", ["sources::tests"], "2 pruebas: URL y raíz absoluta normalizadas, sin secretos, traversal, dispositivos ni autenticación insegura"),
        ("catalog-database", "larm-db", ["--test", "sources"], "8 pruebas SQLite: registro/binding e índice atómicos, CAS concurrente, replay original 30 días, separación de actores, cuentas y referencias, gates de cualificación, filtros/reapertura, fault injection outbox/idempotencia"),
    ]
    for name, package, args, expected in groups:
        ev.run(f"P043-CONTRACT-{name}", "CONTRACT", ["cargo", "test", "-p", package, *args], expected, ok=cargo_ok)
    ev.run("P043-REAL-worker", "REAL", ["cargo", "test", "-p", "larm-adapter-host", "--test", "source"],
           "6 pruebas con procesos Windows: páginas, metadata, permisos/filtros antes del callback, 429/auth/plan, respuesta inválida, crash/reinicio, timeout/cancel y UUID RPC duplicado sin reemplazar la llamada activa", ok=cargo_ok)
    ev.run("P043-CONTRACT-fixtures", "CONTRACT", [sys.executable, "tools/p043_fixtures.py", "check"],
           "fixture reproducible y hash canónico del plan calculado por Python, independiente del SDK", fixtures=[REPO / "tests/fixtures/source/protocol.json"])
    ev.run("P043-CONTRACT-schemas", "CONTRACT", [sys.executable, "tools/p043_schemas.py"],
           "20 documentos válidos y 119 negativos contra los schemas canónicos, no un sustituto de conducta real")
    ev.run("P043-REAL-catalog-http", "REAL", [sys.executable, "tools/p043_sources_real.py"],
           "17 pasos HTTP/CLI/SQLite reales: registro público para UI, guard/DTO sin efectos, altas/CAS concurrentes, replay original tras reinicio/borrado, cuenta ajena, identidad/referencias, outbox fallido sin mutaciones; INSTALLED/UNTRUSTED fixture sin promoción",
           fixtures=[REPO / "tools/p043_sources_real.py"])
    npm = shutil.which("npm.cmd") or "npm"
    ev.run("P043-INTEGRATION-ui", "INTEGRATION", [npm, "test", "--", "src/sources.test.tsx"],
           "8 casos de interfaz: vacío/conectores, binding correcto, idempotencia al reintentar/cambiar, conformidad, preservar cuenta/política y borrador tras conflicto, cancelar/recargar, confirmar borrado/referencias, páginas y permisos",
           cwd=REPO / "apps/desktop")
    ev.run("P043-CONTRACT-ui-receipt", "CONTRACT", [sys.executable, "tools/p043_ui_verify.py"],
           "recibo de sesión CUA real vigente contra hashes y SQLite/outbox; POST201/PATCH200/DELETE204 y limpieza. Comprueba un recibo: no reproduce la sesión ni acredita WebView nativo",
           fixtures=[REPO / "docs/reports/P043_runs/ui-real.json", REPO / "docs/reports/P043_runs/ui-real-created.png", REPO / "docs/reports/P043_runs/ui-real-deleted.png", REPO / "docs/reports/P043_runs/ui-form-375.png", REPO / "docs/reports/P043_runs/ui-form-800.png"])
    ev.run("P043-CONTRACT-openapi", "CONTRACT", ["target/debug/larm-daemon.exe", "openapi", "--out", "docs/api/openapi.json", "--check"],
           "OpenAPI generado coincide con contrato; las cinco rutas de fuentes figuran implementadas")
    ev.run("P043-REAL-architecture", "REAL", ["powershell", "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", "tools/check_architecture.ps1"],
           "grafo de crates intacto: DB depende sólo de dominio, Core sin SQL, daemon compone host+DB")
    ev.run("P043-REAL-lock", "REAL", [sys.executable, "tools/verify_p000.py", "lock-coherence"], "46 dependencias fijadas coherentes con environment.lock.json")
    ev.run("P043-REAL-secret-storage", "REAL", [sys.executable, "tools/check_integration_credentials.py", "--scan-only"],
           "credenciales Broker/Agora disponibles en Credential Manager y ausentes de los archivos inspeccionados; no se imprime su valor")
    changed = ["Cargo.lock", "crates/larm-core/Cargo.toml", "crates/larm-domain/src/ports.rs", "crates/larm-domain/src/search.rs",
               "crates/larm-adapter-host/src/policy.rs", "crates/larm-adapter-host/src/worker.rs", "crates/larm-api/src/openapi.rs",
               "apps/daemon/src/main.rs", "apps/daemon/src/serve.rs", "apps/desktop/src/App.tsx", "apps/desktop/src/styles.css",
               "apps/desktop/src/sources.tsx", "apps/desktop/src/sources.test.tsx", "tests/fixtures/source/protocol.json",
               "docs/api/openapi.json", "docs/adapters/source-v1.md", "docs/implementation/STATE.md"]
    for package in ("larm-domain", "larm-db", "larm-core", "larm-api", "larm-adapter-sdk", "larm-adapter-host"):
        changed.append(f"crates/{package}/src/lib.rs")
        for candidate in ("src/sources.rs", "src/source.rs", "tests/sources.rs", "tests/source.rs", "tests/source_protocol.rs", "examples/fake_source.rs"):
            path = REPO / "crates" / package / candidate
            if path.is_file():
                changed.append(path.relative_to(REPO).as_posix())
    changed += [p.relative_to(REPO).as_posix() for p in (REPO / "crates/larm-adapter-sdk/src/source").glob("*.rs")]
    changed += [p.relative_to(REPO).as_posix() for p in (REPO / "tools").glob("p043_*.py")]
    changed.append("apps/daemon/src/sources.rs")
    return 1 if ev.finish(sorted(set(changed)), []) == "BLOCKED" else 0


if __name__ == "__main__":
    raise SystemExit(main())
