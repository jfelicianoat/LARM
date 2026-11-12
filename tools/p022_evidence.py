#!/usr/bin/env python3
"""P022 - Permisos, confianza y aislamiento de adaptadores: pruebas y evidencia."""
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from evidence import Evidence, cargo_ok  # noqa: E402

PY = sys.executable


def main() -> int:
    ev = Evidence("P022")
    ev.run("P022-REAL-build-fake", "REAL", ["cargo", "build", "-p", "larm-adapter-host", "--example", "fake_adapter"],
           "adaptador falso compilado")
    fixtures = [Path("tests/fixtures/fake_adapter/manifest.json"), Path("tests/fixtures/adapters/demo-benchmark.manifest.json")]
    tests = [
        ("P022-REAL-permiso-denegado", "permiso_denegado_en_cada_operacion",
         "adaptador firmado VERIFIED: lectura/escritura/red/proceso/secreto fuera de permisos FORBIDDEN sin llegar al worker; manifiesto distinto del autorizado no arranca"),
        ("P022-REAL-sin-firma", "sin_firma_solo_con_concesion_trusted_only",
         "UNSIGNED y CUSTOM: APPROVAL_REQUIRED sin concesión; aprobaciones de otro publicador/permisos/hash/digest/actor o caducadas rechazadas; con concesión ejecuta sin secretos salvo concesión explícita; paquete manipulado package_hash_mismatch"),
        ("P022-REAL-revocado-elevado", "revocado_firma_invalida_y_elevado_bloquean",
         "clave o paquete revocados REVOKED y FORBIDDEN; firma falsificada signature_invalid; LARM elevado elevated_process"),
        ("P022-REAL-elevacion-permisos", "elevacion_de_permisos_requiere_aprobacion",
         "delta de permisos: APPROVAL_REQUIRED con delta exacto; aprobación del delta lo acepta; otro delta u otro actor no"),
        ("P022-REAL-integridad", "nivel_de_integridad_real_de_este_proceso",
         "whoami /groups real: integridad media (no elevado) en el PC IA"),
    ]
    for test_id, filt, expected in tests:
        ev.run(test_id, "REAL", ["cargo", "test", "-p", "larm-adapter-host", "--test", "policy", "--", filt, "--exact"],
               f"PC IA, procesos y ficheros reales: {expected}", ok=cargo_ok, fixtures=fixtures)
    ev.run("P022-UNIT-permisos-sdk", "UNIT", ["cargo", "test", "-p", "larm-adapter-sdk", "--lib", "permissions"],
           "firma Ed25519 y manipulación, delta, comprobación de rutas/orígenes/procesos/secretos", ok=cargo_ok)
    ev.run("P022-UNIT-cargo-fmt", "UNIT", ["cargo", "fmt", "--all", "--check"], "sin diferencias de formato")
    ev.run("P022-UNIT-cargo-clippy", "UNIT", ["cargo", "clippy", "--workspace", "--all-targets", "--", "-D", "warnings"], "clippy sin avisos")
    ev.run("P022-UNIT-cargo-test", "UNIT", ["cargo", "test", "--workspace"], "pruebas del workspace en verde", ok=cargo_ok)
    ev.run("P022-REAL-architecture", "REAL",
           ["powershell", "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", "tools/check_architecture.ps1"], "grafo de crates intacto")
    ev.run("P022-REAL-lock", "REAL", [PY, "tools/verify_p000.py", "lock-coherence"], "ed25519-dalek fijado en environment.lock.json")
    ev.run("P022-REAL-regresion-p021", "REAL", ["cargo", "test", "-p", "larm-adapter-host", "--test", "worker"],
           "host de workers (P021) sigue en verde", ok=cargo_ok)
    ev.blocked("P022-REAL-elevado-real", "REAL",
               "cargo test -p larm-adapter-host --test policy (consola elevada, LARM_EXPECT_ELEVATED=1)",
               "LARM ejecutado como administrador rechaza lanzar adaptadores (observado con whoami real)",
               "requiere que el operador ejecute las pruebas elevado con LARM_EXPECT_ELEVATED=1; aquí sólo hay integridad media")
    ev.blocked("P022-REAL-appcontainer", "REAL",
               "worker en AppContainer (P130)",
               "AppContainer con capability de red y ACL por SID de paquete como frontera del SO",
               "no implementado: P130; toda ejecución es TRUSTED_ONLY")
    changed = [
        "Cargo.toml", "environment.lock.json",
        "crates/larm-adapter-sdk/Cargo.toml", "crates/larm-adapter-sdk/src/lib.rs", "crates/larm-adapter-sdk/src/permissions.rs",
        "crates/larm-adapter-host/Cargo.toml", "crates/larm-adapter-host/src/lib.rs", "crates/larm-adapter-host/src/policy.rs",
        "crates/larm-adapter-host/tests/policy.rs", "docs/adapters/security.md", "tools/p022_evidence.py",
    ]
    ev.finish(changed, [])
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
