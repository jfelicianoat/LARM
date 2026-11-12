#!/usr/bin/env python3
"""P025: SDKs y referencias externas reales, conformance común y host protegido."""
import sys

from evidence import Evidence, REPO, cargo_ok


def main():
    ev = Evidence("P025")
    ev.run("P025-REAL-build", "REAL", ["cargo", "build", "-p", "larm-adapter-sdk", "-p", "larm-daemon", "--bins"],
           "worker genérico, servidor echo y daemon compilados sin instalar herramientas globales")
    ev.run("P025-REAL-python-sdk", "REAL", [sys.executable, "tools/test_python_adapter_sdk.py"],
           "9 pruebas: ocho procesos propios con frames reales, límites/recuperación, callbacks/cola/cancel/EOF y 24 ejemplos compartidos", fixtures=[REPO / "tools/test_python_adapter_sdk.py", REPO / "tests/fixtures/adapters/python_sdk_worker.py"])
    ev.run("P025-CONTRACT-rust-sdk", "CONTRACT", ["cargo", "test", "-p", "larm-adapter-sdk"],
           "SDK completo: decoder de producción, 24 ejemplos compartidos, sesión/EOF/IDs duplicados, permisos, genérico y Source Protocol con recibo canónico de worker sin job ficticio de Core", ok=cargo_ok)
    ev.run("P025-REAL-source-host-regression", "REAL", ["cargo", "test", "-p", "larm-adapter-host", "--test", "source"],
           "6 procesos Windows: permisos, negociación, paginación, auth/429, respuestas inválidas, crash/reinicio, timeout/cancel e ID duplicado", ok=cargo_ok)
    ev.run("P025-REAL-protected-references", "REAL", ["cargo", "test", "-p", "larm-daemon", "--test", "reference_adapters"],
           "ambas referencias por GuardedWorker/WhoAmI real y concesión exacta TRUSTED_ONLY sin secretos; echo con parada, source actualizada por CLI/coordinador P024 real con profiler/SQLite/reservas/publicación, candidata descubierta INSTALLED/UNTRUSTED y usada por host tipado, SHA y ledger contrastados; origen inicial fixture no acredita P028 ni P026", ok=cargo_ok,
           fixtures=[REPO / "apps/daemon/tests/reference_adapters.rs", REPO / "tools/p025_host_fixture.py"])
    ev.run("P025-REAL-runtime", "REAL", [sys.executable, "tools/p025_runtime_real.py"],
           "25 pasos: conformance común, versión/hash y plan independientes, cancel/estado/generación/límites, echo HTTP real, muerte del hijo propio y recuperación, parada/EOF, registro API y uso de paquete instalado sin cambios Core, hash inmutable y principal revocado", fixtures=[REPO / "tools/p025_runtime_real.py"])
    ev.run("P025-REAL-source", "REAL", [sys.executable, "tools/p025_source_real.py"],
           "30 pasos: Python autónomo, catálogo/paginación/metadata, nueve firmas, snapshot/cursor/plan/expiry/auth/presupuesto, SHA y tamaños, ejecución durable privada, reanudación/cancel, fallo real de outbox en aceptación/finalización, proceso muerto/recuperación sin timeout, perfil fijado, conformance común, ZIP descubierto por Core y paquete inmutable", fixtures=[REPO / "tools/p025_source_real.py"])
    ev.run("P025-REAL-source-transfer", "REAL", [sys.executable, "tools/test_reference_transfer.py"],
           "9 pruebas HTTP/NTFS: bytes/hash, Range/If-Range, cambio ETag/Range/longitud, checksum, cancel/retry, 401/429/redirect, presupuesto y raíces/orígenes, junction real rechazada", fixtures=[REPO / "tools/test_reference_transfer.py"])
    ev.run("P025-CONTRACT-source-schemas", "CONTRACT", [sys.executable, "tools/p043_schemas.py"],
           "18 válidos y 102 negativos contra schemas canónicos; las referencias no amplían DTO ni sustituyen unknown por soporte")
    ev.run("P025-CONTRACT-package-regression", "CONTRACT", ["cargo", "test", "-p", "larm-adapter-host", "--test", "packages"],
           "9 oráculos P024 de paquetes: snapshot/hash, paths/colisiones/enlaces/bombas, límites antes de asignar, cancel/timeout y publicación sin sobrescribir", ok=cargo_ok)
    ev.run("P025-CONTRACT-openapi", "CONTRACT", ["target/debug/larm-daemon.exe", "openapi", "--out", "docs/api/openapi.json", "--check"],
           "OpenAPI canónico intacto; el estado privado del worker no se anuncia como ruta Core implementada")
    ev.run("P025-REAL-architecture", "REAL", ["powershell", "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", "tools/check_architecture.ps1"],
           "grafo publicado conservado: adaptadores no dependen de Core/DB/API; pruebas cruzadas sólo en composition root daemon")
    ev.run("P025-REAL-lock", "REAL", [sys.executable, "tools/verify_p000.py", "lock-coherence"],
           "46 dependencias fijadas; Python autónomo usa las versiones existentes y sus licencias, sin nueva dependencia")
    ev.run("P025-REAL-secret-storage", "REAL", [sys.executable, "tools/check_integration_credentials.py", "--scan-only"],
           "ambos tokens autorizados permanecen en Credential Manager y ausentes de los archivos inspeccionados; valores nunca impresos")
    changed = ["environment.lock.json", "crates/larm-adapter-sdk/Cargo.toml", "crates/larm-adapter-sdk/src/server.rs", "crates/larm-adapter-sdk/src/protocol.rs", "crates/larm-adapter-sdk/src/generic.rs", "crates/larm-adapter-sdk/src/source/wire.rs", "crates/larm-adapter-sdk/tests/session.rs", "crates/larm-adapter-sdk/tests/examples.rs", "crates/larm-adapter-sdk/tests/source_protocol.rs", "apps/daemon/tests/reference_adapters.rs", "tests/fixtures/adapters/python_sdk_worker.py", "tools/test_python_adapter_sdk.py", "tools/test_reference_transfer.py", "docs/adapters/authoring.md", "docs/adapters/source-v1.md", "docs/implementation/STATE.md"]
    for folder in ("adapters/sdk/python", "adapters/runtime/reference", "adapters/source/reference", "tools/adapter-conformance"):
        changed += [p.relative_to(REPO).as_posix() for p in (REPO / folder).rglob("*") if p.suffix in (".py", ".rs", ".md") and "__pycache__" not in p.parts]
    changed += [p.relative_to(REPO).as_posix() for p in (REPO / "tools").glob("p025_*.py")]
    return 1 if ev.finish(sorted(set(changed)), []) == "BLOCKED" else 0


if __name__ == "__main__":
    raise SystemExit(main())
