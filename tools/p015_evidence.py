#!/usr/bin/env python3
"""P015 - StorageEstimate y preflight de espacio: pruebas y evidencia."""
import shutil
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from evidence import REPO, Evidence, cargo_ok  # noqa: E402

PY = sys.executable
NPM = shutil.which("npm.cmd") or "npm"


def main() -> int:
    ev = Evidence("P015")
    lib = [
        ("P015-UNIT-ejemplo-contrato", "larm-domain", "estimate::tests::ejemplo_del_contrato",
         "fuente existente 30 + temporal 18 + salida 9: total 57 / extra 27; fuente descargada: extra 57"),
        ("P015-UNIT-exact-y-margen", "larm-domain", "estimate::tests::margen", "margen max(1 GiB, ceil 5 %); requerido = pico + margen"),
        ("P015-UNIT-insuficiente-y-reserva", "larm-domain", "estimate::tests::espacio_insuficiente",
         "mínimo de política y reservas de otros respetados; utilizable nunca negativo"),
        ("P015-UNIT-rango-estimado", "larm-domain", "estimate::tests::estimado_y_rango", "se reserva la cota superior; manda la peor confianza"),
        ("P015-UNIT-unknown", "larm-domain", "estimate::tests::desconocido", "UNKNOWN sin cota bloquea; presupuesto duro explícito lo acota"),
        ("P015-FAULT-overflow", "larm-domain", "estimate::tests::copia_a_otro", "copia a otro volumen reserva el destino íntegro; desbordamiento y volumen ausente son error"),
        ("P015-CONTRACT-schema", "larm-domain", "estimate::tests::serializa", "StorageEstimate con la forma exacta del schema"),
        ("P015-UNIT-planificador", "larm-storage", "estimate::tests::explica",
         "evalúa todas las ubicaciones: fits, insufficient_space, state_offline, state_low_space, volume_identity_unknown, fat32_file_too_large; determinista"),
        ("P015-UNIT-minimo-de-politica", "larm-storage", "estimate::tests::minimo", "mínimo del volumen = mayor reserva, no la suma"),
        ("P015-UNIT-planificador-unknown", "larm-storage", "estimate::tests::tamano_desconocido", "size_unknown salvo presupuesto duro"),
    ]
    for test_id, crate, filt, expected in lib:
        kind = test_id.split("-")[1]
        ev.run(test_id, kind, ["cargo", "test", "-p", crate, "--lib", "--", filt], expected, ok=cargo_ok)
    ev.run("P015-INTEGRATION-reservas", "INTEGRATION", ["cargo", "test", "-p", "larm-db", "--test", "reservations"],
           "admisión atómica; propia reserva no se cuenta dos veces; consumo; readmisión; instantánea > 5 s rechazada; "
           "caducadas fuera; todo o nada entre volúmenes; 10 admisiones simultáneas = 1", ok=cargo_ok)
    ev.run("P015-UNIT-cargo-fmt", "UNIT", ["cargo", "fmt", "--all", "--check"], "sin diferencias de formato")
    ev.run("P015-UNIT-cargo-clippy", "UNIT", ["cargo", "clippy", "--workspace", "--all-targets", "--", "-D", "warnings"], "clippy sin avisos")
    ev.run("P015-UNIT-cargo-test", "UNIT", ["cargo", "test", "--workspace"], "pruebas del workspace en verde", ok=cargo_ok)
    ev.run("P015-REAL-architecture", "REAL",
           ["powershell", "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", "tools/check_architecture.ps1"], "grafo de crates intacto")
    ev.run("P015-REAL-preflight", "REAL", [PY, "tools/p015_real.py", "preflight"],
           "PC IA con ubicaciones reales en C:: utilizable = libre (ctypes) - mayor reserva; insuficiente; rango por cota; "
           "UNKNOWN y presupuesto; 30/18/9; desbordamiento; admisión real en volume_reservation; 4 procesos concurrentes",
           timeout=1200)
    ev.run("P015-REAL-p014-sin-regresion", "REAL", [PY, "tools/p014_real.py", "serve"], "las ubicaciones de P014 siguen funcionando", timeout=1200)
    changed = [
        "crates/larm-domain/src/estimate.rs", "crates/larm-domain/src/ports.rs", "crates/larm-domain/src/lib.rs",
        "crates/larm-storage/src/estimate.rs", "crates/larm-storage/src/lib.rs", "crates/larm-storage/Cargo.toml",
        "crates/larm-db/src/reservations.rs", "crates/larm-db/src/lib.rs", "crates/larm-db/tests/reservations.rs",
        "apps/daemon/src/preflight.rs", "apps/daemon/src/main.rs", "docs/operations/storage.md",
        "tools/p015_real.py", "tools/p015_evidence.py",
    ]
    ev.finish(changed, [])
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
