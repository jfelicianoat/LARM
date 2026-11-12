#!/usr/bin/env python3
"""P011 - Hardware Profiler: GPU, NPU y backends; pruebas y evidencia."""
import shutil
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from evidence import REPO, Evidence, cargo_ok  # noqa: E402

PY = sys.executable
NPM = shutil.which("npm.cmd") or "npm"


def main() -> int:
    ev = Evidence("P011")
    fx = [REPO / "tests/fixtures/hardware/native-pcia.json", REPO / "tests/fixtures/hardware/vulkaninfo-summary-pcia.txt",
          REPO / "tests/fixtures/hardware/clinfo-pcia.txt", REPO / "tests/fixtures/hardware/profile-pcia.json"]
    lib = [
        ("P011-UNIT-matriz-pc-ia", "larm-hardware", "accel::tests::pc_ia_real",
         "fixtures reales del PC IA: 1 GPU (WARP y el adaptador duplicado por la pantalla remota fuera), 1 NPU, 5 APIs verificadas y 7 ABSENT"),
        ("P011-UNIT-detectado-no-es-operativo", "larm-hardware", "accel::tests::componente_presente",
         "DLL presente sin prueba = UNKNOWN not_verified; vulkaninfo ausente = UNKNOWN tool_missing; nunca entra en backends"),
        ("P011-FAULT-hijo-nativo-caido", "larm-hardware", "accel::tests::hijo_nativo_caido",
         "hijo nativo caído: sin dispositivos inventados y APIs nativas UNKNOWN con el motivo"),
        ("P011-FAULT-sin-dll", "larm-hardware", "accel::tests::sin_dll",
         "sin DLL ninguna API se afirma: ABSENT; herramienta colgada = UNKNOWN tool_timeout"),
        ("P011-FAULT-directml-hip", "larm-hardware", "accel::tests::directml_fallido",
         "DMLCreateDevice fallido = UNKNOWN probe_failed con el HRESULT; HIP con 2 dispositivos y 1 GPU AMD = sin ligar"),
        ("P011-UNIT-gpu-con-problema", "larm-hardware", "accel::tests::controladora_pci",
         "controladora PCI con problema 43 que DXGI no ve: detectada, sin backends, driver desconocido declarado"),
        ("P011-UNIT-prueba-manda", "larm-hardware", "accel::tests::la_prueba_nativa_manda",
         "la prueba nativa manda sobre la búsqueda de DLL; sin hijo y sin DLL, ABSENT"),
        ("P011-UNIT-version-hip", "larm-hardware", "accel::tests::version_hip", "HIP_VERSION 60450101 -> 6.4.50101"),
        ("P011-UNIT-parsers", "larm-hardware", "tools::tests",
         "vulkaninfo y clinfo reales del PC IA; varios dispositivos, bloques sin ids, sin plataformas y basura"),
        ("P011-UNIT-busqueda-dll", "larm-hardware", "libs::tests",
         "patrones exactos y por prefijo, orden de carpetas, carpetas inexistentes, sha256 y raíces de SDK sin duplicados"),
        ("P011-FAULT-hijo-real", "larm-hardware", "devices::tests::hijo_inexistente",
         "procesos reales: inexistente, colgado 60 s (timeout 1,5 s), exit 3, basura, informe incompleto y de otra versión"),
        ("P011-FAULT-herramienta-real", "larm-hardware", "devices::tests::herramienta",
         "herramienta ausente = NotInstalled; colgada = tool_timeout"),
        ("P011-CONTRACT-informe-nativo", "larm-hardware", "native_report::tests",
         "informe del hijo con schema cerrado: campos extra rechazados; LUID estable"),
        ("P011-UNIT-servicio-refresco", "larm-core", "hardware::tests",
         "un solo sondeo a la vez; fallo = FAILED con la correlación de la petición; job visible solo para su autor o admin"),
    ]
    for test_id, crate, filt, expected in lib:
        kind = test_id.split("-")[1]
        ev.run(test_id, kind, ["cargo", "test", "-p", crate, "--lib", "--", filt], expected, ok=cargo_ok, fixtures=fx)
    ev.run("P011-INTEGRATION-sqlite-jobs", "INTEGRATION", ["cargo", "test", "-p", "larm-db", "--test", "hardware"],
           "10 encolados simultáneos = 1 job; transiciones inválidas = INVALID_STATE_TRANSITION; recuperación = FAILED "
           "job_interrupted reintentable; perfiles por huella", ok=cargo_ok)
    ev.run("P011-INTEGRATION-api", "INTEGRATION", ["cargo", "test", "-p", "larm-api", "--test", "hardware"],
           "POST /hardware/refresh con guard real: 202 JobReceipt, job en curso reutilizado, replay idempotente, 409 con otro cuerpo, "
           "Probe obligatorio y cerrado, profile_id no aplicable, resource_id inválido/inexistente, 403 sin admin, job ajeno 404, "
           "COMPLETED y FAILED por /jobs", ok=cargo_ok)
    ev.run("P011-CONTRACT-openapi", "CONTRACT",
           [str(REPO / "target/debug/larm-daemon.exe"), "openapi", "--out", "docs/api/openapi.json", "--check"],
           "OpenAPI al día con POST /api/v1/hardware/refresh implementada")
    ev.run("P011-CONTRACT-unsafe-adr-0010", "CONTRACT", [PY, "tools/verify_p001.py", "policies"],
           "unsafe solo en winsec.rs (ADR-0009) y native.rs (ADR-0010)")
    ev.run("P011-CONTRACT-adrs", "CONTRACT", [PY, "tools/verify_p002.py", "adrs"], "ADR-0010 aceptado y con sus decisiones")
    ev.run("P011-UNIT-cargo-fmt", "UNIT", ["cargo", "fmt", "--all", "--check"], "sin diferencias de formato")
    ev.run("P011-UNIT-cargo-clippy", "UNIT", ["cargo", "clippy", "--workspace", "--all-targets", "--", "-D", "warnings"],
           "clippy sin avisos (incluido el módulo nativo)")
    ev.run("P011-UNIT-cargo-test", "UNIT", ["cargo", "test", "--workspace"], "pruebas del workspace en verde", ok=cargo_ok)
    for script, expected in (("lint", "eslint sin avisos"), ("typecheck", "TypeScript sin errores"),
                             ("test", "vitest: GPU/NPU reales, matriz, sin sondeo de dispositivos, refresco con job, 403 y FAILED"),
                             ("build", "build de producción del frontend")):
        ev.run(f"P011-UNIT-desktop-{script}", "UNIT", [NPM, "run", script, "--silent"], expected)
    ev.run("P011-REAL-architecture", "REAL",
           ["powershell", "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", "tools/check_architecture.ps1"],
           "larm-hardware sigue dependiendo solo de larm-domain; la API usa puertos y larm-core")
    ev.run("P011-REAL-lock-coherence", "REAL", [PY, "tools/verify_p000.py", "lock-coherence"],
           "dependencias nuevas fijadas en environment.lock.json y coherentes con Cargo.lock")
    ev.run("P011-REAL-frente-a-oraculos", "REAL", [PY, "tools/p011_real.py", "windows"],
           "PC IA: dispositivos y drivers = Get-PnpDevice; memoria dedicada = registro; Vulkan/OpenCL/HIP = ctypes propios; "
           "ausencias = búsqueda propia; DLL: sha256 y versión = hashlib y VersionInfo; huella recalculada en Python",
           timeout=900)
    ev.run("P011-REAL-sin-dll", "REAL", [PY, "tools/p011_real.py", "sin-dll"],
           "SystemRoot y PATH vacíos: lo buscado ABSENT, DirectML/WinML/HIP siguen VERIFIED, huella distinta", timeout=600)
    ev.run("P011-REAL-refresco-job", "REAL", [PY, "tools/p011_real.py", "serve"],
           "daemon real: 202, un solo sondeo, idempotencia y 409, COMPLETED con perfil de 2 dispositivos y 12 APIs, "
           "permisos, y FAILED job_interrupted tras matar el daemon", timeout=900)
    ev.run("P011-REAL-p010-sin-regresion", "REAL", [PY, "tools/p010_real.py", "serve"],
           "el arranque sigue registrando y actualizando el perfil (P010) con el sondeo como job", timeout=600)
    changed = [
        "docs/adr/0010-excepcion-unsafe-sondeo-nativo.md", "crates/larm-hardware/src/native.rs",
        "crates/larm-hardware/src/native_report.rs", "crates/larm-hardware/src/accel.rs", "crates/larm-hardware/src/devices.rs",
        "crates/larm-hardware/src/libs.rs", "crates/larm-hardware/src/tools.rs", "crates/larm-hardware/src/process.rs",
        "crates/larm-hardware/src/cim.rs", "crates/larm-hardware/src/profiler.rs", "crates/larm-hardware/src/lib.rs",
        "crates/larm-hardware/Cargo.toml", "crates/larm-domain/src/hardware.rs", "crates/larm-domain/src/ports.rs",
        "crates/larm-core/src/hardware.rs", "crates/larm-core/src/lib.rs", "crates/larm-db/src/hardware.rs",
        "crates/larm-db/tests/hardware.rs", "crates/larm-api/src/hardware.rs", "crates/larm-api/src/security.rs",
        "crates/larm-api/src/openapi.rs", "crates/larm-api/tests/hardware.rs", "crates/larm-api/Cargo.toml",
        "apps/daemon/src/hardware.rs", "apps/daemon/src/main.rs", "apps/daemon/src/serve.rs", "apps/daemon/build.rs",
        "apps/daemon/larm-daemon.manifest", "apps/daemon/Cargo.toml", "Cargo.toml", "Cargo.lock", "environment.lock.json",
        "docs/api/openapi.json", "docs/operations/hardware.md", "apps/desktop/src/hardware.tsx",
        "apps/desktop/src/hardware.test.tsx", "apps/desktop/src/styles.css", "apps/desktop/tsconfig.app.json",
        "tests/fixtures/hardware/native-pcia.json", "tests/fixtures/hardware/vulkaninfo-summary-pcia.txt",
        "tests/fixtures/hardware/clinfo-pcia.txt", "tests/fixtures/hardware/profile-pcia.json",
        "tools/verify_p001.py", "tools/verify_p002.py", "tools/p011_real.py", "tools/p011_evidence.py",
    ]
    ev.finish(changed, [])
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
