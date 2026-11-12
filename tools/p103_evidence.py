#!/usr/bin/env python3
"""P103: evidencia del framework sidecar y sus límites actuales."""

from evidence import Evidence, REPO, cargo_ok


def main() -> int:
    ev = Evidence("P103")
    env = {"CARGO_TARGET_DIR": str(REPO / "target/verification-final")}
    ev.run(
        "P103-CONTRACT-sidecar-http-and-cli-fixture",
        "CONTRACT",
        [
            "cargo",
            "test",
            "-p",
            "larm-protocol",
            "--all-targets",
            "-j",
            "1",
            "--",
            "--test-threads=1",
        ],
        "suite de unidad e integración: CLI fixture ejecutado por el binario sidecar separado, HTTP loopback, autenticación efímera, cancelación, límites y rechazo de features/input inválidos",
        ok=cargo_ok,
        timeout=900,
        env=env,
        fixtures=[
            REPO / "crates/larm-protocol/src/sidecar.rs",
            REPO / "crates/larm-protocol/tests/sidecar_http.rs",
            REPO / "adapters/runtime/reference-sidecar/main.rs",
            REPO / "adapters/runtime/reference-sidecar/echo_cli.rs",
        ],
    )

    ev.blocked(
        "P103-REAL-runtime-instance-and-consumer",
        "REAL",
        "integrar el sidecar con una RuntimeInstance cualificada y un consumidor real",
        "endpoint de instancia con auth efímera, correlation/cancel/timeout, lifecycle y subsets declarados verificados contra un runtime real",
        "el proceso de prueba usa únicamente el CLI fixture echo; P036 no tiene evidencia, y P021/P026 siguen BLOCKED_REAL, así que no existe todavía una integración real que pueda cualificarse",
    )

    changed = [
        "adapters/runtime/reference-sidecar/README.md",
        "adapters/runtime/reference-sidecar/echo_cli.rs",
        "adapters/runtime/reference-sidecar/main.rs",
        "crates/larm-protocol/Cargo.toml",
        "crates/larm-protocol/src/lib.rs",
        "crates/larm-protocol/src/sidecar.rs",
        "crates/larm-protocol/tests/sidecar_http.rs",
        "docs/implementation/STATE.md",
        "docs/reports/P103_protocol-sidecar-framework.md",
        "tools/p103_evidence.py",
    ]
    blockers = [
        "P103 solo prepara un sidecar REST personalizado de texto; no anuncia compatibilidad OpenAI ni está integrado aún al lifecycle RuntimeInstance/lease.",
        "El echo CLI es un fixture, no un modelo. Faltan cualificación de runtimes/prompt templates, contención de árbol de procesos y aceptación real del consumidor.",
    ]
    return int(ev.finish(changed, blockers) == "BLOCKED")


if __name__ == "__main__":
    raise SystemExit(main())
