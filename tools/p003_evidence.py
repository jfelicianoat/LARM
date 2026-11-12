#!/usr/bin/env python3
"""P003 - configuración, errores y logging estructurado: pruebas y evidencia."""
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from evidence import REPO, Evidence, cargo_ok  # noqa: E402

PY = sys.executable


def tests(crate: str, filt: str) -> list[str]:
    return ["cargo", "test", "-p", crate, "--lib", "--", filt]


def main() -> int:
    ev = Evidence("P003")
    defaults = REPO / "docs/contracts/defaults.json"
    schema = REPO / "docs/contracts/schemas/larm.schema.json"
    unit = [
        ("P003-UNIT-error-shape", "larm-domain", "error::tests::serializa",
         "LarmError serializa exactamente {code,message,retryable,correlation_id,details} sin el detalle técnico"),
        ("P003-CONTRACT-error-catalog", "larm-domain", "error::tests::catalogo",
         "34 códigos del contrato 07 y §154 con su estado HTTP"),
        ("P003-UNIT-error-extensible", "larm-domain", "error::tests::codigo_desconocido",
         "código desconocido se conserva y responde 500"),
        ("P003-FAULT-error-strict", "larm-domain", "error::tests::campos_adicionales",
         "campo adicional (detail) o código vacío rechazados"),
        ("P003-UNIT-error-retryable", "larm-domain", "error::tests::retryable",
         "retryable nunca en licencia/incompatibles"),
        ("P003-UNIT-original-error", "larm-domain", "error::tests::original_error",
         "original_error redactado y truncado a 4096"),
        ("P003-UNIT-correlation", "larm-domain", "correlation::tests",
         "correlation id UUIDv7: roundtrip, generación, conservación y rechazo de v4/mayúsculas"),
        ("P003-UNIT-redaction", "larm-domain", "redact::tests",
         "9 patrones compilan; secretos redactados; texto normal y sha256 intactos"),
        ("P003-UNIT-decimal-bytes", "larm-domain", "units::tests",
         "bytes como string decimal; number, signo, ceros y overflow rechazados"),
        ("P003-CONTRACT-defaults", "larm-core", "config::tests::defaults_serializan",
         "Settings::defaults() serializa exactamente defaults.json y valida"),
        ("P003-UNIT-precedence", "larm-core", "config::tests::precedencia", "defaults < file < env < cli"),
        ("P003-UNIT-env-sections", "larm-core", "config::tests::env_solo",
         "solo LARM_<SECCIÓN>__<CLAVE> es config; bytes de env como string; typo rechazado"),
        ("P003-CONTRACT-h08", "larm-core", "config::tests::h08", "límite null o positivo como string; cero y number rechazados"),
        ("P003-CONTRACT-h09", "larm-core", "config::tests::h09",
         "PERCENT y BYTES con critical<warning; igualdad, cruce, rango, mezcla y modo desconocido rechazados"),
        ("P003-FAULT-schema-negatives", "larm-core", "config::tests::negativos",
         "11 configuraciones fuera de schema -> VALIDATION_FAILED 400 sin detalle técnico serializado"),
        ("P003-UNIT-logging-options", "larm-core", "config::tests::logging_options",
         "nivel/formato con precedencia env<cli; valores inválidos rechazados, sin fallback"),
    ]
    for test_id, crate, filt, expected in unit:
        kind = "FAULT" if "-FAULT-" in test_id else ("CONTRACT" if "-CONTRACT-" in test_id else "UNIT")
        ev.run(test_id, kind, tests(crate, filt), expected, ok=cargo_ok, fixtures=[defaults, schema])

    ev.run("P003-UNIT-cargo-fmt", "UNIT", ["cargo", "fmt", "--all", "--check"], "sin diferencias de formato")
    ev.run("P003-UNIT-cargo-clippy", "UNIT", ["cargo", "clippy", "--workspace", "--all-targets", "--", "-D", "warnings"],
           "clippy sin avisos")
    ev.run("P003-UNIT-cargo-test", "UNIT", ["cargo", "test", "--workspace"], "pruebas del workspace en verde", ok=cargo_ok)

    ev.run("P003-REAL-daemon-build", "REAL", ["cargo", "build", "-p", "larm-daemon"], "binario del daemon compilado")
    ev.run("P003-UNIT-settings-api", "UNIT", tests("larm-api", "settings::tests::"),
           "Settings API usa la precedencia del arranque: persistido < fichero < entorno < CLI; CAS, límite y umbrales efectivos coherentes",
           ok=cargo_ok, fixtures=[REPO / "crates/larm-api/src/settings.rs", REPO / "apps/daemon/src/serve.rs"])
    ev.run("P003-REAL-settings-restart", "REAL",
           ["cargo", "test", "-p", "larm-daemon", "--test", "reference_adapters", "settings_restart::p003_settings_tcp_respeta_fichero_y_recupera_valor_persistido", "-j1", "--", "--exact", "--test-threads=1"],
           "Cinco arranques del daemon TCP verifican fichero, null explícito, retirada del override, valor persistido, ETag/CAS y outbox; principal temporal revocado",
           ok=cargo_ok, fixtures=[REPO / "apps/daemon/tests/support/settings_restart.rs", REPO / "apps/daemon/tests/support/download_restart.rs", REPO / "apps/daemon/tests/support/daemon.rs"], timeout=300)
    for sub, expected in [
        ("precedence", "larm-daemon print-settings: cli > env > fichero > defaults, salida válida contra Settings"),
        ("rejects", "6 configuraciones inválidas -> exit 2 sin escribir logs"),
        ("logging", "log JSON diario con timestamps ms+Z, correlation UUIDv7 y secreto inyectado redactado; exit 0"),
        ("retention", "20 logs antiguos podados a diagnostics.log_retention_days"),
        ("stdout-bloqueado", "stdout en un pipe sin leer no congela el daemon: 500 peticiones, registro completo en fichero"),
    ]:
        ev.run(f"P003-REAL-{sub}", "REAL", [PY, "tools/p003_real.py", sub], expected, fixtures=[defaults, schema])

    changed = [
        "crates/larm-domain/src/error.rs", "crates/larm-domain/src/redact.rs", "crates/larm-domain/src/units.rs",
        "crates/larm-domain/src/correlation.rs", "crates/larm-domain/src/bridge.rs", "crates/larm-domain/src/lib.rs",
        "crates/larm-domain/Cargo.toml", "crates/larm-core/src/config/mod.rs", "crates/larm-core/src/logging.rs",
        "crates/larm-core/src/redact.rs", "crates/larm-runtime/src/external_adoption/tests.rs", "apps/daemon/src/main.rs",
        "docs/operations/configuration.md", "tools/p003_real.py", "tools/p003_evidence.py",
        "crates/larm-api/src/settings.rs", "apps/daemon/src/serve.rs",
        "apps/daemon/tests/reference_adapters.rs", "apps/daemon/tests/support/settings_restart.rs",
        "apps/daemon/tests/support/download_restart.rs", "apps/daemon/tests/support/p048_download.rs",
        "docs/reports/P003_settings_file_2026-10-27.md",
    ]
    ev.finish(changed, [])
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
