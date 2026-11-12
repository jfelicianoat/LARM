#!/usr/bin/env python3
"""P024: paquete, registro, selección y actualización durable, incluidos procesos Windows."""
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from evidence import Evidence, cargo_ok  # noqa: E402


def main() -> int:
    ev = Evidence("P024")
    ev.run("P024-REAL-build", "REAL",
           ["cargo", "build", "-p", "larm-daemon", "-p", "larm-adapter-sdk", "--bins"],
           "daemon y worker genérico reales compilados para las pruebas nativas")
    ev.run("P024-CONTRACT-packages", "CONTRACT",
           ["cargo", "test", "-p", "larm-adapter-host", "--test", "packages"],
           "9 pruebas: side-by-side sin sobrescribir, zip-slip/ADS/dispositivos/colisiones/enlaces/bombas rechazados, protocolo/schema/hashes, snapshot cambiado, límites del índice antes de asignarlo, manifiesto acotado, cancel/timeout/IO; registro local determinista y sin versiones ambiguas",
           ok=cargo_ok)
    ev.run("P024-CONTRACT-catalog", "CONTRACT",
           ["cargo", "test", "-p", "larm-api", "--test", "adapters"],
           "4 pruebas del catálogo y alta existente: autorización real, listado/detalle, filtros/cursor, DTO e idempotencia",
           ok=cargo_ok)
    ev.run("P024-CONTRACT-update-api", "CONTRACT",
           ["cargo", "test", "-p", "larm-api", "--test", "adapter_updates"],
           "4 pruebas con guard real: adapters:write, UUIDv7, DTO cerrado con approval_id obligatorio nullable, Idempotency-Key y hash del cuerpo exacto, actor/correlación autenticados, JobReceipt 202 y privacidad propietario/admin sin exponer rutas",
           ok=cargo_ok)
    ev.run("P024-UNIT-update-dto", "UNIT",
           ["cargo", "test", "-p", "larm-domain", "adapter_updates"],
           "AdapterImport exacto y serialización de estados; campos extra o faltantes, hash y aprobación inválidos rechazados",
           ok=cargo_ok)
    ev.run("P024-INTEGRATION-database", "INTEGRATION",
           ["cargo", "test", "-p", "larm-db"],
           "persistencia SQLite completa: selección CAS/rollback y migración conservadora, 10 casos de journal durable con aceptación concurrente, replay 30 días aislado por actor/cuerpo/destino, aprobación exacta, reservas/locks/fencing, reapertura, journal alterado y fallos de outbox en transacciones; regresiones P015/P019",
           ok=cargo_ok)
    ev.run("P024-INTEGRATION-coordinator", "INTEGRATION",
           ["cargo", "test", "-p", "larm-daemon", "--test", "adapter_coordinator"],
           "18 pruebas de IO y SQLite reales, espacio controlado para negativos: reserva antes de copiar, side-by-side sin promoción, firma/revocación/aprobación, cancel/timeout/fences, commit/ack inciertos, recuperación por fase, lock físico, revisión cambiada, aceptación que sobrevive a desconexión, un worker con cola durable y shutdown que espera el IO",
           ok=cargo_ok)
    ev.run("P024-UNIT-space-trust", "UNIT",
           ["cargo", "test", "-p", "larm-daemon", "--bin", "larm-daemon", "adapters::tests"],
           "4 pruebas: identidad y espacio deben estar verificados y online/escribibles; mínimo de política; claves configuradas requieren binding explícito correcto y las retiradas no se reactivan desde el fichero de metadata",
           ok=cargo_ok)
    native = [
        ("catalog", "tools/p024_real.py",
         "9 pasos Windows: ZIP/registro local y hashes/presupuesto independientes, zip-slip/protocolo incompatible sin efectos, listado paginado/detalle contra SQLite, autorización y filtros/cursor"),
        ("selection", "tools/p024_selection_real.py",
         "13 pasos Windows: selección y rollback desde procesos independientes, CAS concurrente con un solo ganador, outbox fallido revierte todo, reapertura, revocación y deshabilitación; journal/outbox/bytes comprobados con Python; VALIDATED es fixture, no conformidad P026"),
        ("update", "tools/p024_update_real.py",
         "8 pasos Windows con profiler real/NTFS/SQLite: actualización desde alta P023, hash/pico y GUID contrastados independientemente, replay sin duplicados, DTO/protocolo rechazados; muerte del proceso propio durante EXTRACTING conserva recursos y otro proceso limpia sólo su staging y registra FAILED"),
        ("http", "tools/p024_api_real.py",
         "14 pasos HTTP real: guard/DTO sin efectos, aceptación concurrente 202, trabajo y paquete con profiler/SQLite/NTFS, propiedad/admin, replay original y separación de actores, elevación con digest independiente y aprobación no transferible consumida; CTRL_BREAK espera IO; muerte real en PUBLISHING conserva journal/bytes/recursos y arranque termina el mismo job sin promover ni duplicar"),
    ]
    for name, script, expected in native:
        ev.run(f"P024-REAL-{name}", "REAL", [sys.executable, script], expected,
               fixtures=[Path(script)])
    ev.run("P024-REAL-migration", "REAL", [sys.executable, "tools/p004_real.py", "dbinfo"],
           "CLI real aplica 0001+0002, 45 tablas y reaplicación vacía; checksum de 0001 conservado")
    ev.run("P024-CONTRACT-openapi", "CONTRACT",
           ["target/debug/larm-daemon.exe", "openapi", "--out", "docs/api/openapi.json", "--check"],
           "OpenAPI coincide con contrato; POST /adapters/{id}/update figura implementado")
    ev.run("P024-REAL-architecture", "REAL",
           ["powershell", "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", "tools/check_architecture.ps1"],
           "grafo publicado intacto; composición host+DB en daemon y puerto de dominio para la API")
    ev.run("P024-REAL-lock", "REAL", [sys.executable, "tools/verify_p000.py", "lock-coherence"],
           "dependencias fijadas y extensión ZIP coherente con environment.lock.json")
    ev.run("P024-REAL-secret-storage", "REAL",
           [sys.executable, "tools/check_integration_credentials.py", "--scan-only"],
           "ambas credenciales autorizadas disponibles en Credential Manager y ausentes de los archivos inspeccionados; no se imprime su valor")
    ev.blocked("P024-REAL-conformance-before-promotion", "REAL",
               "suite de conformidad P026 sobre una candidata nueva antes de promoverla",
               "sólo una candidata que complete la suite puede llegar a VALIDATED y seleccionarse",
               "P026 aún no está implementado; P024 instala sólo INSTALLED y rechaza su promoción. Las fixtures VALIDATED de selección prueban el mecanismo, no acreditan la suite ni una cualificación real")
    changed = [
        "Cargo.lock", "environment.lock.json", "migrations/0002_adapter_selection.sql",
        "crates/larm-adapter-sdk/src/permissions.rs",
        "crates/larm-domain/src/adapters.rs", "crates/larm-domain/src/adapter_updates.rs",
        "crates/larm-domain/src/lib.rs", "crates/larm-domain/src/ports.rs",
        "crates/larm-db/src/adapter_selection.rs", "crates/larm-db/src/adapter_updates.rs",
        "crates/larm-db/src/adapters.rs", "crates/larm-db/src/guard.rs", "crates/larm-db/src/reservations.rs",
        "crates/larm-db/src/lib.rs", "crates/larm-db/src/migrate.rs",
        "crates/larm-db/tests/adapters.rs", "crates/larm-db/tests/adapter_selection.rs",
        "crates/larm-db/tests/adapter_updates.rs", "crates/larm-db/tests/integration.rs",
        "crates/larm-adapter-host/Cargo.toml", "crates/larm-adapter-host/src/lib.rs",
        "crates/larm-adapter-host/src/package.rs", "crates/larm-adapter-host/src/registry.rs",
        "crates/larm-adapter-host/src/updates.rs", "crates/larm-adapter-host/src/update_service.rs",
        "crates/larm-adapter-host/tests/packages.rs",
        "crates/larm-api/Cargo.toml", "crates/larm-api/src/adapters.rs", "crates/larm-api/src/openapi.rs",
        "crates/larm-api/tests/adapters.rs", "crates/larm-api/tests/adapter_updates.rs",
        "apps/daemon/Cargo.toml", "apps/daemon/src/main.rs", "apps/daemon/src/adapters.rs",
        "apps/daemon/src/serve.rs", "apps/daemon/tests/adapter_coordinator.rs", "docs/api/openapi.json",
        "docs/adapters/registry.md", "docs/implementation/STATE.md",
        "tools/p024_evidence.py", "tools/p024_prepare_evidence.py", "tools/p024_real.py",
        "tools/p024_selection_real.py", "tools/p024_update_real.py", "tools/p024_api_real.py",
        "tools/p004_real.py", "tools/p004_evidence.py",
    ]
    status = ev.finish(changed, [])
    return 1 if status == "BLOCKED" else 0


if __name__ == "__main__":
    raise SystemExit(main())
