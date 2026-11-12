#!/usr/bin/env python3
"""P012 - Storage Profiler y Volume Identity en Windows: pruebas y evidencia."""
import shutil
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from evidence import REPO, Evidence, cargo_ok  # noqa: E402

PY = sys.executable
NPM = shutil.which("npm.cmd") or "npm"


def main() -> int:
    ev = Evidence("P012")
    fx = [REPO / "tests/fixtures/hardware/storage-pcia.json", REPO / "tests/fixtures/hardware/profile-pcia.json"]
    lib = [
        ("P012-UNIT-clasificacion", "larm-domain", "storage::tests::clasifica",
         "NVME/SSD/HDD/REMOVABLE/NETWORK solo con bus o penalización observados; sin evidencia o VHD = UNKNOWN"),
        ("P012-UNIT-guid", "larm-domain", "storage::tests::guid",
         "\\\\?\\Volume{GUID}\\ -> GUID en minúsculas sin llaves; formatos inválidos rechazados"),
        ("P012-UNIT-unc-parser", "larm-domain", "storage::tests::rutas_unc",
         "UNC con \\\\?\\UNC\\ y /, clave casefold; dispositivo, sin recurso, '..' y caracteres inválidos rechazados"),
        ("P012-CONTRACT-enums", "larm-domain", "storage::tests::serializa", "NVME, REMOTE, FILE_BACKED_VIRTUAL con los nombres del contrato"),
        ("P012-UNIT-estado-observado", "larm-domain", "storage::tests::estado", "ONLINE/READ_ONLY/UNKNOWN/OFFLINE según lo observado"),
        ("P012-UNIT-pc-ia", "larm-hardware", "storage::tests::pc_ia_real",
         "fixture real: 3 volúmenes NVMe (2 sin letra) y \\\\localhost\\Temp; servidor inexistente sin datos inventados"),
        ("P012-UNIT-montajes", "larm-hardware", "storage::tests::varios_montajes",
         "letra y montaje en carpeta en el mismo volumen; cambio de letra conserva la volume_key"),
        ("P012-UNIT-clase-sin-evidencia", "larm-hardware", "storage::tests::sin_evidencia", "UNKNOWN declarado; USB = REMOVABLE"),
        ("P012-FAULT-guid-ilegible", "larm-hardware", "storage::tests::guid_ilegible", "sin GUID verificable no hay volume_key"),
        ("P012-UNIT-unidad-mapeada", "larm-hardware", "storage::tests::unidad_mapeada",
         "Z: mapeada y la misma raíz UNC son un volumen; ruta no UNC declarada"),
        ("P012-FAULT-enumeracion", "larm-hardware", "storage::tests::enumeracion_fallida", "FindFirstVolumeW fallido = volumes_probe_failed"),
        ("P012-FAULT-hijo", "larm-hardware", "storage::tests::hijo_inexistente", "hijo inexistente o de otra versión rechazados"),
        ("P012-UNIT-refresco", "larm-core", "hardware::tests::el_refresco_sondea",
         "el refresco pide las raíces UNC, guarda la observación en el perfil y la aplica"),
        ("P012-FAULT-refresco", "larm-core", "hardware::tests::un_fallo_de_almacenamiento",
         "fallo de almacenamiento declarado en metadata.storage; perfil registrado; storage_location intacto"),
    ]
    for test_id, crate, filt, expected in lib:
        kind = test_id.split("-")[1]
        ev.run(test_id, kind, ["cargo", "test", "-p", crate, "--lib", "--", filt], expected, ok=cargo_ok, fixtures=fx)
    ev.run("P012-INTEGRATION-sqlite", "INTEGRATION", ["cargo", "test", "-p", "larm-db", "--test", "storage"],
           "UNKNOWN->ONLINE con versión y evento; solo espacio libre sin versión; cambio de letra; OFFLINE conserva datos y "
           "last_seen_at; READ_ONLY; UNC caído; 8 observaciones simultáneas = 1 cambio", ok=cargo_ok)
    ev.run("P012-CONTRACT-unsafe-adr-0010", "CONTRACT", [PY, "tools/verify_p001.py", "policies"],
           "unsafe solo en winsec.rs y native.rs (ADR-0010 ampliado)")
    ev.run("P012-CONTRACT-adrs", "CONTRACT", [PY, "tools/verify_p002.py", "adrs"], "ADR aceptados")
    ev.run("P012-UNIT-cargo-fmt", "UNIT", ["cargo", "fmt", "--all", "--check"], "sin diferencias de formato")
    ev.run("P012-UNIT-cargo-clippy", "UNIT", ["cargo", "clippy", "--workspace", "--all-targets", "--", "-D", "warnings"], "clippy sin avisos")
    ev.run("P012-UNIT-cargo-test", "UNIT", ["cargo", "test", "--workspace"], "pruebas del workspace en verde", ok=cargo_ok)
    for script, expected in (("lint", "eslint sin avisos"), ("typecheck", "TypeScript sin errores"),
                             ("test", "vitest: sección Almacenamiento con volúmenes reales, sin letra, red caída y error"),
                             ("build", "build de producción del frontend")):
        ev.run(f"P012-UNIT-desktop-{script}", "UNIT", [NPM, "run", script, "--silent"], expected)
    ev.run("P012-REAL-architecture", "REAL",
           ["powershell", "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", "tools/check_architecture.ps1"],
           "grafo de crates intacto")
    ev.run("P012-REAL-enumeracion", "REAL", [PY, "tools/p012_real.py", "windows"],
           "PC IA: GUID y montajes = mountvol; capacidad exacta = GetDiskFreeSpaceExW; NTFS = Get-Volume; NVME = Get-PhysicalDisk; "
           "\\\\localhost\\Temp disponible sin GUID; UNC inexistente sin datos; identidad estable", timeout=900)
    ev.run("P012-REAL-ultimo-estado", "REAL", [PY, "tools/p012_real.py", "serve"],
           "daemon real: filas de storage_location ONLINE/OFFLINE/red con capacidad real, 4 eventos en outbox, segundo "
           "sondeo sin versión ni eventos, observación en el perfil", timeout=900)
    ev.run("P012-REAL-p011-sin-regresion", "REAL", [PY, "tools/p011_real.py", "serve"],
           "el refresco de hardware sigue funcionando con el almacenamiento integrado", timeout=900)
    changed = [
        "crates/larm-domain/src/storage.rs", "crates/larm-domain/src/ports.rs", "crates/larm-domain/src/lib.rs",
        "crates/larm-hardware/src/storage.rs", "crates/larm-hardware/src/native.rs", "crates/larm-hardware/src/native_report.rs",
        "crates/larm-hardware/src/lib.rs", "crates/larm-core/src/hardware.rs", "crates/larm-db/src/storage.rs",
        "crates/larm-db/src/lib.rs", "crates/larm-db/tests/storage.rs", "crates/larm-api/tests/hardware.rs",
        "apps/daemon/src/hardware.rs", "apps/daemon/src/main.rs", "Cargo.toml", "Cargo.lock",
        "apps/desktop/src/hardware.tsx", "apps/desktop/src/hardware.test.tsx",
        "tests/fixtures/hardware/storage-pcia.json", "tests/fixtures/hardware/profile-pcia.json",
        "docs/adr/0010-excepcion-unsafe-sondeo-nativo.md", "docs/operations/storage.md",
        "tools/p012_real.py", "tools/p012_evidence.py",
    ]
    ev.finish(changed, [])
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
