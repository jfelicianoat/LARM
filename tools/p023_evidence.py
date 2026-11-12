#!/usr/bin/env python3
"""P023 - Adaptador declarativo configurable desde GUI: pruebas y evidencia."""
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from evidence import Evidence, cargo_ok  # noqa: E402

PY = sys.executable


def main() -> int:
    ev = Evidence("P023")
    ev.run("P023-REAL-build", "REAL",
           ["cargo", "build", "-p", "larm-daemon", "-p", "larm-adapter-sdk", "--bins", "--examples"],
           "daemon, worker genérico larm-generic-adapter y runtime falso compilados")
    fixtures = [Path("adapters/runtime/generic/wizard.schema.json")]
    host = [
        ("P023-REAL-sondeo-ejecutable-falso", "sondeo_de_version_con_ejecutable_falso",
         "Probe fake executable: versión EXACT_TEXT y JSON_FIELD reales, canal custom (no stable), artifact_hash = sha256 del exe, sondeo colgado Timeout a 10 s, método no aplicable UNSUPPORTED_CAPABILITY sin éxito vacío"),
        ("P023-REAL-arranque-entorno-health", "arranque_entorno_health_y_parada_reales",
         "H04 + Health fake server: plan de un solo uso con hash, LARM_TEST llega al proceso y nada del worker se hereda, args expandidos, health READY, parada SIGNAL con el puerto cerrado"),
        ("P023-REAL-health-degradado-http", "health_degradado_caido_y_parada_http",
         "health 503 DEGRADED, parada HTTP dentro de la gracia, runtime muerto FAILED process_exited"),
        ("P023-REAL-negativos-entorno-inyeccion", "entorno_no_autorizado_inyeccion_y_credencial",
         "variable fuera de las dos allowlists rechazada antes de IO; inyección por model_path rechazada por worker y host; credencial sin resolver bloquea sin lanzar; límite de memoria UNSUPPORTED_CAPABILITY"),
        ("P023-REAL-paquete-manipulado", "paquete_manipulado_no_se_autoriza",
         "cambiar declarative.json rompe el hash y la política P022 lo rechaza (package_hash_mismatch)"),
    ]
    for test_id, filt, expected in host:
        ev.run(test_id, "REAL", ["cargo", "test", "-p", "larm-adapter-host", "--test", "declarative", "--", filt, "--exact"],
               f"PC IA, procesos reales (host P021 + política P022 + worker genérico + runtime falso): {expected}",
               ok=cargo_ok, fixtures=fixtures)
    ev.run("P023-REAL-daemon-extremo-a-extremo", "REAL", [PY, "tools/p023_real.py", "serve"],
           "daemon real: alta por la API con Bearer real, paquete con hash recalculado por un oráculo independiente, fila y evento en SQLite, idempotencia, 403/400 sin efectos, exportar sin secretos e importar UNTRUSTED, worker del paquete manejado por un cliente JSON-RPC independiente: la variable no secreta llega al proceso; credencial sin resolver bloquea; runtime real (Ollama aislado) sondeado, arrancado y parado",
           fixtures=fixtures)
    ev.run("P023-UNIT-sdk-declarativo", "UNIT", ["cargo", "test", "-p", "larm-adapter-sdk", "--lib", "declarative"],
           "valid/invalid manifest y documento, placeholders tipados y desconocidos, inyección, entorno H04 (casos H04-055..058), simulación determinista, materialización con hash real, schema del asistente coherente",
           ok=cargo_ok, fixtures=fixtures)
    ev.run("P023-UNIT-worker-generico", "UNIT", ["cargo", "test", "-p", "larm-adapter-sdk", "--lib", "generic"],
           "HTTP chunked, puerto de la URL y bloqueo de credenciales", ok=cargo_ok)
    ev.run("P023-CONTRACT-api", "CONTRACT", ["cargo", "test", "-p", "larm-api", "--test", "adapters"],
           "POST /adapters/declarative con el guard real: 201 adapter_installation exacto, idempotencia, IDEMPOTENCY_CONFLICT, 409 de versión, 403/401, 400 antes de IO sin filas ni carpetas, exportar → importar sin secretos ni confianza",
           ok=cargo_ok)
    ev.run("P023-INTEGRATION-repositorio", "INTEGRATION", ["cargo", "test", "-p", "larm-db", "--test", "adapters"],
           "adapter_installation sobre SQLite con la migración del contrato: alta, lectura, búsqueda, evento adapter.updated en la misma transacción y CHECK que anula también el evento",
           ok=cargo_ok)
    ev.run("P023-FAULT-mutaciones", "FAULT", [PY, "tools/p023_mutations.py"],
           "24 defectos introducidos a propósito (placeholders, .bat, metacaracteres, loopback, allowlists, secretos, raíces, EXACT_TEXT, plan, health, hash, entorno heredado, credencial, confianza, conflicto): todos hacen fallar alguna prueba; el worker se recompila con cada mutación",
           timeout=5400)
    ev.run("P023-UNIT-cargo-fmt", "UNIT", ["cargo", "fmt", "--all", "--check"], "sin diferencias de formato")
    ev.run("P023-UNIT-cargo-clippy", "UNIT", ["cargo", "clippy", "--workspace", "--all-targets", "--", "-D", "warnings"], "clippy sin avisos")
    ev.run("P023-UNIT-cargo-test", "UNIT", ["cargo", "test", "--workspace"], "pruebas del workspace en verde", ok=cargo_ok)
    ev.run("P023-REAL-architecture", "REAL",
           ["powershell", "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", "tools/check_architecture.ps1"],
           "grafo de crates intacto (larm-core → larm-adapter-sdk, permitido)")
    ev.run("P023-REAL-lock", "REAL", [PY, "tools/verify_p000.py", "lock-coherence"],
           "dependencias nuevas (chrono en el SDK; sha2/tempfile de prueba) ya fijadas en environment.lock.json")
    ev.run("P023-REAL-regresion-p021-p022", "REAL", ["cargo", "test", "-p", "larm-adapter-host"],
           "host de workers (P021) y política (P022) siguen en verde", ok=cargo_ok)
    ev.blocked("P023-REAL-gui", "REAL", "pantalla Adaptadores → asistente (P037)",
               "configurar desde la GUI una variable no secreta, guardar/exportar/importar y comprobar que llega al proceso",
               "la GUI final del asistente es de P037 (H04 también es de P037/P140); aquí se probó la misma operación por la API que usa la GUI y el schema del asistente queda publicado")
    ev.blocked("P023-REAL-credencial-en-worker", "REAL", "credential_reference resuelta en el worker (P045)",
               "una variable con credential_reference llega al proceso con el secreto de Credential Manager",
               "la resolución de secretos dentro del worker es de P045; hoy una referencia bloquea sondeo y arranque (credential_not_enrolled), comprobado en real")
    ev.blocked("P023-REAL-api-export-import", "REAL", "POST /adapters/export y /adapters/import (P028)",
               "exportación e importación por sus endpoints",
               "los endpoints son de P028; aquí se exporta el documento del paquete y se importa con POST /adapters/declarative")
    changed = [
        "Cargo.lock",
        "adapters/runtime/README.md", "adapters/runtime/generic/README.md", "adapters/runtime/generic/main.rs",
        "adapters/runtime/generic/wizard.schema.json",
        "apps/daemon/src/serve.rs",
        "crates/larm-adapter-sdk/Cargo.toml", "crates/larm-adapter-sdk/src/lib.rs",
        "crates/larm-adapter-sdk/src/declarative.rs", "crates/larm-adapter-sdk/src/generic.rs",
        "crates/larm-adapter-sdk/examples/fake_runtime.rs",
        "crates/larm-adapter-host/Cargo.toml", "crates/larm-adapter-host/tests/declarative.rs",
        "crates/larm-domain/src/lib.rs", "crates/larm-domain/src/adapters.rs", "crates/larm-domain/src/ports.rs",
        "crates/larm-db/src/lib.rs", "crates/larm-db/src/adapters.rs", "crates/larm-db/tests/adapters.rs",
        "crates/larm-core/Cargo.toml", "crates/larm-core/src/lib.rs", "crates/larm-core/src/adapters.rs",
        "crates/larm-api/Cargo.toml", "crates/larm-api/src/lib.rs", "crates/larm-api/src/adapters.rs",
        "crates/larm-api/src/openapi.rs", "crates/larm-api/tests/adapters.rs",
        "docs/adapters/declarative.md",
        "tools/p023_evidence.py", "tools/p023_real.py", "tools/p023_mutations.py",
    ]
    ev.finish(changed, [])
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
