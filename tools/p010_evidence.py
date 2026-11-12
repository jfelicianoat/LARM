#!/usr/bin/env python3
"""P010 - Hardware Profiler (Windows, CPU y memoria): pruebas y evidencia."""
import shutil
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from evidence import REPO, Evidence, cargo_ok  # noqa: E402

PY = sys.executable
NPM = shutil.which("npm.cmd") or "npm"


def main() -> int:
    ev = Evidence("P010")
    schema = [REPO / "docs/contracts/schemas/domain.schema.json", REPO / "docs/contracts/schemas/larm.schema.json"]
    lib = [
        ("P010-UNIT-fingerprint-estable", "larm-domain", "hardware::tests::la_huella_ignora",
         "la huella ignora observed_metrics, LUID, evidence_refs y el orden de enumeración (contrato 04)"),
        ("P010-UNIT-fingerprint-cambia", "larm-domain", "hardware::tests::la_huella_cambia",
         "driver, build, RAM, ISA o un componente que pasa a desconocido cambian la huella"),
        ("P010-CONTRACT-snapshot-schema", "larm-domain", "hardware::tests::serializa",
         "bytes como string decimal, enums GPU/NPU, campos extra rechazados (schema cerrado)"),
        ("P010-UNIT-invariantes", "larm-domain", "hardware::tests::validate",
         "disponible ≤ visible ≤ instalada, físicos ≤ lógicos, uso 0..100; desconocido no es incoherente"),
        ("P010-UNIT-mapping-cpu", "larm-hardware", "cpu::tests",
         "texto vacío o cero núcleos = desconocido; ISA ordenada y con jerarquías coherentes (CPUID real)"),
        ("P010-UNIT-mapping-os", "larm-hardware", "os::tests",
         "build = CurrentBuild.UBR; sin CurrentBuild o no numérico = desconocido; registro real legible"),
        ("P010-UNIT-mapping-memoria", "larm-hardware", "memory::tests",
         "carve-out de VRAM no sumado como RAM; instalada < visible = incoherente; disponible imposible = desconocido"),
        ("P010-FAULT-wmi-parse", "larm-hardware", "cim::tests::interpreta",
         "salida CIM vacía, cero, negativa, no numérica o desbordada = wmi_unparseable"),
        ("P010-FAULT-wmi-ausente", "larm-hardware", "cim::tests::programa_inexistente",
         "programa CIM inexistente = wmi_unavailable"),
        ("P010-FAULT-wmi-colgado", "larm-hardware", "cim::tests::consulta_colgada",
         "PowerShell real colgado 60 s: vence el timeout (1,5 s), se mata el proceso y no bloquea"),
        ("P010-FAULT-wmi-error", "larm-hardware", "cim::tests::clase_inexistente",
         "clase CIM inexistente con PowerShell real = wmi_failed"),
        ("P010-FAULT-wmi-sondeo-degradado", "larm-hardware", "profiler::tests",
         "con WMI colgado, ausente o con basura el sondeo sigue: RAM instalada null con motivo y el resto observado"),
    ]
    for test_id, crate, filt, expected in lib:
        kind = test_id.split("-")[1]
        ev.run(test_id, kind, ["cargo", "test", "-p", crate, "--lib", "--", filt], expected, ok=cargo_ok, fixtures=schema)
    ev.run("P010-INTEGRATION-sqlite", "INTEGRATION", ["cargo", "test", "-p", "larm-db", "--test", "hardware"],
           "misma huella actualiza (row_version+1), huella nueva crea perfil; 10 sondeos simultáneos = 1 perfil; "
           "snapshot incoherente rechazado sin escribir; fila fuera del schema = INTERNAL", ok=cargo_ok)
    ev.run("P010-INTEGRATION-api", "INTEGRATION", ["cargo", "test", "-p", "larm-api", "--test", "hardware"],
           "GET /hardware con guard real: forma {items,next_cursor,snapshot_at}, cursor firmado, cursor alterado 400, "
           "filtro/limit inválidos 400, sin token 401, sin scope read 403, fallo del repositorio 500", ok=cargo_ok)
    ev.run("P010-UNIT-cursor-credenciales", "UNIT", ["cargo", "test", "-p", "larm-api", "--test", "security"],
           "la paginación genérica no cambia la de /credentials", ok=cargo_ok)
    ev.run("P010-CONTRACT-openapi", "CONTRACT",
           [str(REPO / "target/debug/larm-daemon.exe"), "openapi", "--out", "docs/api/openapi.json", "--check"],
           "OpenAPI al día con GET /api/v1/hardware implementada")
    ev.run("P010-UNIT-cargo-fmt", "UNIT", ["cargo", "fmt", "--all", "--check"], "sin diferencias de formato")
    ev.run("P010-UNIT-cargo-clippy", "UNIT", ["cargo", "clippy", "--workspace", "--all-targets", "--", "-D", "warnings"],
           "clippy sin avisos")
    ev.run("P010-UNIT-cargo-test", "UNIT", ["cargo", "test", "--workspace"], "pruebas del workspace en verde", ok=cargo_ok)
    for script, expected in (("lint", "eslint sin avisos"), ("typecheck", "TypeScript sin errores"),
                             ("test", "vitest: pantalla Hardware con datos, desconocidos, cursor, vacío y error"),
                             ("build", "build de producción del frontend")):
        ev.run(f"P010-UNIT-desktop-{script}", "UNIT", [NPM, "run", script, "--silent"], expected)
    ev.run("P010-REAL-architecture", "REAL",
           ["powershell", "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", "tools/check_architecture.ps1"],
           "larm-hardware solo depende de larm-domain; la API llega por puertos del dominio")
    ev.run("P010-REAL-lock-coherence", "REAL", [PY, "tools/verify_p000.py", "lock-coherence"],
           "sysinfo 0.39.6 y windows-registry 0.100.0 coinciden con environment.lock.json")
    ev.run("P010-REAL-frente-a-windows", "REAL", [PY, "tools/p010_real.py", "windows"],
           "sondeo real del PC IA = CIM, reg.exe e IsProcessorFeaturePresent; schema válido; huella reproducible",
           timeout=600)
    ev.run("P010-REAL-api-persistencia", "REAL", [PY, "tools/p010_real.py", "serve"],
           "daemon real: perfil registrado al arrancar, GET /hardware conforme a hardware_profileList, "
           "rearranque con el mismo hardware = mismo perfil row_version 2; 401/403/400", timeout=600)
    changed = [
        "crates/larm-domain/src/hardware.rs", "crates/larm-domain/src/ports.rs", "crates/larm-domain/src/lib.rs",
        "crates/larm-hardware/src/lib.rs", "crates/larm-hardware/src/cim.rs", "crates/larm-hardware/src/cpu.rs",
        "crates/larm-hardware/src/os.rs", "crates/larm-hardware/src/memory.rs", "crates/larm-hardware/src/profiler.rs",
        "crates/larm-hardware/Cargo.toml", "crates/larm-db/src/hardware.rs", "crates/larm-db/src/lib.rs",
        "crates/larm-db/tests/hardware.rs", "crates/larm-core/src/security/mod.rs", "crates/larm-api/src/hardware.rs",
        "crates/larm-api/src/http.rs", "crates/larm-api/src/security.rs", "crates/larm-api/src/openapi.rs",
        "crates/larm-api/src/lib.rs", "crates/larm-api/tests/hardware.rs", "apps/daemon/src/hardware.rs",
        "apps/daemon/src/main.rs", "apps/daemon/src/serve.rs", "apps/daemon/Cargo.toml", "Cargo.toml", "Cargo.lock",
        "environment.lock.json", "docs/api/openapi.json", "apps/desktop/src/hardware.tsx",
        "apps/desktop/src/hardware.test.tsx", "apps/desktop/src/pages.tsx", "apps/desktop/src/App.tsx",
        "apps/desktop/src/styles.css", "tools/p010_real.py", "tools/p010_evidence.py", "docs/operations/hardware.md",
    ]
    ev.finish(changed, [])
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
