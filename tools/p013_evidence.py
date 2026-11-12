#!/usr/bin/env python3
"""P013 - HardwareFingerprint reproducible y hash de parámetros de lanzamiento: evidencia."""
import shutil
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from evidence import REPO, Evidence, cargo_ok  # noqa: E402

PY = sys.executable
NPM = shutil.which("npm.cmd") or "npm"


def main() -> int:
    ev = Evidence("P013")
    fx = [REPO / "tests/fixtures/launch/balanced.json", REPO / "tests/fixtures/hardware/profile-pcia.json"]
    lib = [
        ("P013-UNIT-jcs-numeros-rfc8785", "larm-domain", "canonical::tests::numeros_segun",
         "16 vectores del apéndice B de RFC 8785 (5e-324, 1.7976931348623157e+308, 1e+23, 0.000001…); NaN/inf rechazados"),
        ("P013-UNIT-jcs-documentos", "larm-domain", "canonical::tests::numeros_en_documentos",
         "1.0 -> 1, 0.7 -> 0.7; enteros > 2^53 rechazados en vez de alterarse"),
        ("P013-UNIT-jcs-claves", "larm-domain", "canonical::tests", "claves por unidades UTF-16; mismo contenido, mismo hash"),
        ("P013-UNIT-huella-oraculos", "larm-domain", "hardware::tests::p013_oraculos",
         "mismo input = mismo hash; orden distinto = mismo hash; driver distinto = otro hash con diff que lo nombra; RAM libre = mismo hash"),
        ("P013-UNIT-huella-volatiles", "larm-domain", "hardware::tests::la_huella_ignora", "LUID, evidencias, métricas y orden fuera"),
        ("P013-UNIT-huella-componentes", "larm-domain", "hardware::tests::la_huella_cambia", "driver, build, RAM, ISA o un null cambian la huella"),
        ("P013-CONTRACT-h03-orden", "larm-domain", "launch::tests::mismo_input",
         "LaunchParameters: mismo input mismo hash; propiedades reordenadas mismo hash"),
        ("P013-CONTRACT-h03-cambios", "larm-domain", "launch::tests::cambiar_solo",
         "solo argumento, orden de argumentos, opción, entorno, referencia, límite o temperatura -> hash distinto"),
        ("P013-CONTRACT-h03-sin-secretos", "larm-domain", "launch::tests::el_hash_no_contiene", "solo referencias larm/ en la forma hasheada"),
        ("P013-CONTRACT-h03-reglas", "larm-domain", "launch::tests::reglas_h03",
         "límite 0, nombres inválidos, duplicados sin mayúsculas, fuera de allowlist, referencia fuera de larm/, generación fuera de rango, binding doble"),
        ("P013-CONTRACT-h03-builtin", "larm-domain", "launch::tests::builtin_y_limite",
         "builtin = null/[]/{allowlist:[],variables:[]}/{}; límite efectivo = mínimo de los no null"),
    ]
    for test_id, crate, filt, expected in lib:
        kind = test_id.split("-")[1]
        ev.run(test_id, kind, ["cargo", "test", "-p", crate, "--lib", "--", filt], expected, ok=cargo_ok, fixtures=fx)
    ev.run("P013-INTEGRATION-historial", "INTEGRATION", ["cargo", "test", "-p", "larm-db", "--test", "hardware"],
           "fingerprint_version 1; configuración nueva guarda perfil anterior, huella anterior y diff; la misma configuración conserva el diff",
           ok=cargo_ok)
    ev.run("P013-INTEGRATION-p107-hash", "INTEGRATION", ["cargo", "test", "-p", "larm-runtime"],
           "la rebanada P107 calcula launch_profile_hash con RFC 8785 y sigue en verde", ok=cargo_ok)
    ev.run("P013-UNIT-cargo-fmt", "UNIT", ["cargo", "fmt", "--all", "--check"], "sin diferencias de formato")
    ev.run("P013-UNIT-cargo-clippy", "UNIT", ["cargo", "clippy", "--workspace", "--all-targets", "--", "-D", "warnings"], "clippy sin avisos")
    ev.run("P013-UNIT-cargo-test", "UNIT", ["cargo", "test", "--workspace"], "pruebas del workspace en verde", ok=cargo_ok)
    for script, expected in (("lint", "eslint sin avisos"), ("typecheck", "TypeScript sin errores"),
                             ("test", "vitest: versión de la huella y cambios respecto a la configuración anterior"),
                             ("build", "build de producción del frontend")):
        ev.run(f"P013-UNIT-desktop-{script}", "UNIT", [NPM, "run", script, "--silent"], expected)
    ev.run("P013-REAL-launch-oraculo", "REAL", [PY, "tools/p013_real.py", "launch"],
           "larm-daemon launch-profile-hash = JCS independiente en Python en el fixture y 9 variantes; reordenar no cambia; 5 rechazos",
           fixtures=fx, timeout=900)
    ev.run("P013-REAL-huella-hardware", "REAL", [PY, "tools/p013_real.py", "hardware"],
           "PC IA: misma huella en dos procesos con RAM libre distinta; recalculada en Python; driver cambia, volátiles no", timeout=900)
    ev.run("P013-REAL-historial-daemon", "REAL", [PY, "tools/p013_real.py", "serve"],
           "daemon real: versión 1 en el primer perfil; rearranque = mismo perfil con el diff conservado", timeout=900)
    changed = [
        "crates/larm-domain/src/canonical.rs", "crates/larm-domain/src/launch.rs", "crates/larm-domain/src/hardware.rs",
        "crates/larm-domain/src/lib.rs", "crates/larm-db/src/hardware.rs", "crates/larm-db/tests/hardware.rs",
        "crates/larm-runtime/src/external_adoption.rs", "apps/daemon/src/main.rs", "apps/desktop/src/hardware.tsx",
        "apps/desktop/src/hardware.test.tsx", "tests/fixtures/launch/balanced.json", "tools/p013_real.py", "tools/p013_evidence.py",
        "docs/operations/hardware.md", "docs/operations/launch-profiles.md",
    ]
    ev.finish(changed, [])
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
