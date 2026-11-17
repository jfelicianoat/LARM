#!/usr/bin/env python3
"""P081: hosts de ejecución observados read-only (WSL2/contenedores); la
cualificación por runtime y la primera instalación con estrategia WSL/CONTAINER
siguen bloqueadas por locks sin cualificación real (P099/P031)."""

import sys

from evidence import Evidence, REPO

TOOL_PYTHON = REPO / ".venv-tools" / "Scripts" / "python.exe"
PYTHON = str(TOOL_PYTHON) if TOOL_PYTHON.is_file() else sys.executable

REAL_TEST = "windows_sondeo_real_wsl2_y_docker_registro_y_lectura_publica"


def main() -> int:
    ev = Evidence("P081")

    ev.run(
        "P081-CONTRACT-domain-and-hardware",
        "CONTRACT",
        ["cargo", "test", "-p", "larm-domain", "--lib"],
        "enum ExecutionHostKind con los seis modos de §5.1 y regla allows_strategy que impide anunciar un runtime WSL como nativo",
    )
    ev.run(
        "P081-CONTRACT-virtualization-probe",
        "CONTRACT",
        ["cargo", "test", "-p", "larm-hardware", "--lib"],
        "sondeo read-only con timeout/kill, entorno mínimo, decodificación UTF-8/UTF-16LE, parsers tolerantes al locale y ausencia de binarios como observación válida",
    )
    ev.run(
        "P081-CONTRACT-execution-host-abstraction",
        "CONTRACT",
        ["cargo", "test", "-p", "larm-runtime", "--lib"],
        "mapeo Windows↔WSL ida/vuelta sin traversal, política de puertos sólo loopback, comandos que mutan el host rechazados (execution_host_plan_mutates_host) y coherencia instalación↔host",
    )
    ev.run(
        "P081-CONTRACT-installation-roundtrip",
        "CONTRACT",
        ["cargo", "test", "-p", "larm-runtime", "--test", "install"],
        "las quince estrategias quedan en el plan con execution_host_id exigido para WSL/CONTAINER",
    )
    ev.run(
        "P081-CONTRACT-db-repository",
        "CONTRACT",
        ["cargo", "test", "-p", "larm-db", "--test", "execution_hosts"],
        "upsert idempotente con row_version+1, journal/evento/outbox en una transacción, filtros, DTO inválido sin escrituras, sin columna de promoción y FK exigida",
    )
    ev.run(
        "P081-CONTRACT-api-route",
        "CONTRACT",
        ["cargo", "test", "-p", "larm-api", "--lib"],
        "GET /api/v1/execution-hosts publicado como implementado con 155 operaciones en el catálogo",
    )

    def wsl_ok(code: int, output: str) -> bool:
        return (
            code == 0
            and "test result: ok" in output
            and "REAL wsl" in output
            and "docker=" in output
        )

    ev.run(
        "P081-REAL-wsl2-docker-probe-and-public-read",
        "REAL",
        ["cargo", "test", "-p", "larm-daemon", "--test", "execution_hosts", REAL_TEST, "--", "--nocapture"],
        "sondeo real de wsl.exe/docker.exe del PC IA; registro con journal/evento/outbox; GET público con scopes (200/400/401/403) y POST rechazado (registro read-only)",
        ok=wsl_ok,
        timeout=600,
    )

    ev.blocked(
        "P081-REAL-per-runtime-qualification",
        "REAL",
        "cualificación real de un runtime concreto instalado en WSL2/contenedor",
        "lock de perfil con versión upstream, documentación de esa versión y fixtures/probes de cada runtime adoptado bajo WSL2/contenedor",
        "los locks de P099 permanecen UNQUALIFIED sin cualificación real; el registro P081 observa motores y nunca acredita compatibilidad de un runtime concreto",
    )
    ev.blocked(
        "P081-REAL-installation-with-wsl-strategy",
        "REAL",
        "primera instalación real con estrategia WSL/CONTAINER referenciando un execution host",
        "instalación MANAGED con execution_host_id sobre un lock cualificado y su runtime observado en el huésped",
        "la admisión P031 exige lock cualificado; sin él la referencia queda probada sólo estructuralmente",
    )

    changed = [
        "crates/larm-domain/src/runtime.rs",
        "crates/larm-domain/src/runtime_install.rs",
        "crates/larm-domain/src/runtime_ports.rs",
        "crates/larm-domain/src/auth.rs",
        "crates/larm-hardware/src/lib.rs",
        "crates/larm-hardware/src/virtualization.rs",
        "crates/larm-runtime/src/lib.rs",
        "crates/larm-runtime/src/execution_host.rs",
        "crates/larm-runtime/src/install_service.rs",
        "crates/larm-db/src/lib.rs",
        "crates/larm-db/src/migrate.rs",
        "crates/larm-db/src/execution_hosts.rs",
        "crates/larm-db/src/runtimes.rs",
        "crates/larm-api/src/runtimes.rs",
        "crates/larm-api/src/openapi.rs",
        "migrations/0015_p081_execution_host.sql",
        "crates/larm-runtime/tests/install.rs",
        "crates/larm-adapter-host/tests/runtime.rs",
        "crates/larm-db/tests/runtime_installation.rs",
        "crates/larm-db/tests/execution_hosts.rs",
        "crates/larm-adapter-sdk/tests/contracts.rs",
        "apps/daemon/tests/execution_hosts.rs",
        "docs/adapters/method-schemas/runtime.install_plan.request.schema.json",
        "docs/adapters/llama-cpp.md",
        "tests/fixtures/adapters/method-schema-corpus.json",
        "apps/desktop/src/runtimes.tsx",
        "apps/desktop/src/runtimes.test.tsx",
        "apps/desktop/src/api/runtimes.ts",
        "docs/implementation/STATE.md",
        "docs/reports/P081_wsl2-y-container-execution-host-abstraction.md",
        "docs/reports/P081_wsl2-y-container-execution-host-abstraction.evidence.json",
        "tools/p081_contract_revision.py",
    ]
    status = ev.finish(changed, [
        "cualificación real por versión upstream (P099): los locks vigentes permanecen UNQUALIFIED",
        "primera instalación real con estrategia WSL/CONTAINER exige lock cualificado (P031)",
    ])
    # BLOCKED_REAL con todos los CONTRACT/REAL en PASS es el mejor estado
    # alcanzable sin locks cualificados: el runner sale 0 salvo FAIL.
    return 0 if "FAIL" not in status else 1


if __name__ == "__main__":
    raise SystemExit(main())
