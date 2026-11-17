#!/usr/bin/env python3
"""P104: puerta del servidor MCP de administración — matriz H06 con casos de
uso Core reales, confirmation_required de destructivas, audit sin secretos,
negativos de args malformados y bucle stdio."""

import sys

from evidence import Evidence, REPO

TOOL_PYTHON = REPO / ".venv-tools" / "Scripts" / "python.exe"
PYTHON = str(TOOL_PYTHON) if TOOL_PYTHON.is_file() else sys.executable


def main() -> int:
    ev = Evidence("P104")

    ev.run(
        "P104-CONTRACT-mcp-admin-server",
        "CONTRACT",
        ["cargo", "test", "-p", "larm-protocol", "--lib", "mcp_admin"],
        "catálogo sellado de 12 herramientas con operaciones publicadas, matriz H06 (start/stop, install/update, admin nunca añade scopes API), confirmation_required sin ejecutar, bucle stdio con JSON inválido y audit sin secretos",
    )
    ev.run(
        "P104-REAL-mcp-server-with-core-use-cases",
        "REAL",
        ["cargo", "test", "-p", "larm-daemon", "--test", "mcp_admin", "--", "--nocapture"],
        "servidor MCP sobre la superficie API real del Core en loopback del PC IA: initialize con credencial real, tools/list 12, larm.get_hardware ejecuta el caso de uso real, stop sin permiso MCP FORBIDDEN, delete sin confirm confirmation_required y con confirm el rechazo real del Core",
        ok=lambda code, output: (
            code == 0
            and "test result: ok" in output
            and "REAL get_hardware" in output
            and "REAL delete_model confirmado" in output
        ),
        timeout=600,
    )

    changed = [
        "crates/larm-protocol/src/lib.rs",
        "crates/larm-protocol/src/mcp_admin/mod.rs",
        "crates/larm-protocol/src/mcp_admin/permissions.rs",
        "crates/larm-protocol/src/mcp_admin/server.rs",
        "crates/larm-protocol/src/mcp_admin/tests.rs",
        "crates/larm-protocol/Cargo.toml",
        "apps/daemon/Cargo.toml",
        "apps/daemon/tests/mcp_admin.rs",
        "docs/implementation/STATE.md",
        "docs/reports/P104_mcp-administration-server.md",
        "docs/reports/P104_mcp-administration-server.evidence.json",
        "tools/p104_evidence.py",
    ]
    blockers = [
        "REAL_PASS de P104 exige las herramientas MCP sobre lock cualificado y leases reales con runtime instalado (P031/P099 en BLOCKED_REAL); el paquete llm-mcp-admin distribuido como proceso del operador y el wizard de UI de la spec son fases de cierre.",
    ]
    status = ev.finish(changed, blockers)
    return 0 if "FAIL" not in status else 1


if __name__ == "__main__":
    raise SystemExit(main())
