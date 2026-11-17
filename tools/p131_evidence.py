#!/usr/bin/env python3
"""P131: puerta de secretos, auth local y permisos API — rate limit de
fallos de autenticación, matriz H06 completa con concesiones distintas y
admin nunca implícito, rotación/revocación con eventos y Credential Manager.
Ejecución real sobre el PC IA (TCP loopback con IP de origen y SQLite
migrado)."""

import sys

from evidence import Evidence, REPO

TOOL_PYTHON = REPO / ".venv-tools" / "Scripts" / "python.exe"
PYTHON = str(TOOL_PYTHON) if TOOL_PYTHON.is_file() else sys.executable


def main() -> int:
    ev = Evidence("P131")

    ev.run(
        "P131-CONTRACT-rate-limiter-y-guard",
        "CONTRACT",
        ["cargo", "test", "-p", "larm-api", "--lib", "auth"],
        "rate limiter de fallos de autenticación determinista (ventana deslizante, bloqueo tras 10 fallos, liberación por ventana y reset por éxito, fuentes independientes)",
    )
    ev.run(
        "P131-CONTRACT-guard-router",
        "CONTRACT",
        ["cargo", "test", "-p", "larm-api", "--test", "security"],
        "guard real sobre el router: un Bearer ausente no consume el límite, diez credenciales inválidas bloquean la fuente (auth_rate_limited con Retry-After, AUTH_REQUIRED del catálogo sellado) y el reset por éxito devuelve el servicio; unauthorized (401/403) ya cubierto por la suite",
    )
    ev.run(
        "P131-REAL-matriz-h06-pc-ia",
        "REAL",
        ["cargo", "test", "-p", "larm-daemon", "--test", "h06_matrix", "--", "--nocapture"],
        "matriz H06 completa REAL sobre el servidor MCP de administración con principales reales y despacho observado hacia el Core real en loopback: start despacha y stop sin concesión es FORBIDDEN sin despachar; install despacha y update sin concesión FORBIDDEN; read y search distintas en ambas direcciones; move/delete destructivas con confirmación y concesiones distintas; admin comodín MCP nunca añade scopes API (scope_missing sin despachar); audit con cada herramienta y sin tokens",
        ok=lambda code, output: (
            code == 0
            and "test result: ok. 1 passed" in output
            and "REAL H06: start despacha y stop sin concesion es FORBIDDEN" in output
            and "REAL H06: install despacha y update sin concesion es FORBIDDEN" in output
            and "REAL H06: read y search son concesiones distintas en ambas direcciones" in output
            and "REAL H06: move y delete son concesiones distintas y destructivas con confirmacion" in output
            and "REAL H06: admin comodin MCP nunca anade scopes API" in output
        ),
        timeout=600,
    )
    ev.run(
        "P131-REAL-credenciales-rotacion-pc-ia",
        "REAL",
        ["cargo", "test", "-p", "larm-daemon", "--test", "auth_security", "--", "--nocapture"],
        "rate limit sobre TCP real con IP de origen (bloqueo tras 10 fallos con token válido incluido, reset por éxito) y rotación/revocación reales contra SQLite migrado con el backend seguro del sistema operativo (Windows Credential Manager): token fuera de la referencia y de la base, eventos credential.created/rotated/deleted, revocación borra el secreto del SO",
        ok=lambda code, output: (
            code == 0
            and "test result: ok. 2 passed" in output
            and "REAL P131: rate limit bloquea la fuente real tras 10 fallos" in output
            and "REAL P131: credencial con secreto en el Credential Manager y sin token en SQLite" in output
            and "REAL P131: rotacion con eventos credential.created/rotated en SQLite" in output
            and "REAL P131: revocacion borra el secreto del SO y emite credential.deleted" in output
        ),
        timeout=600,
    )
    ev.blocked(
        "P131-REAL-start-stop-e2e",
        "REAL",
        "arranque y parada reales de una instancia de runtime vía MCP start/stop con lease ACTIVE",
        "un principal MCP con start arranca una instancia real (lease ACTIVE con runtime instalado) y stop sin concesión queda FORBIDDEN sobre el daemon completo",
        "la autorización H06 está verificada REAL (el despacho de start llega al Core y stop es rechazado sin despachar); el caso de uso completo exige leases reales con runtime instalado mediante lock cualificado (P100/P031/P099 en BLOCKED_REAL)",
    )
    ev.blocked(
        "P131-REAL-remote-bind-fuerte",
        "REAL",
        "bind remoto no-loopback con opt-in, auth fuerte y TLS del sistema",
        "remote_enabled=true con bind no loopback exige autenticación fuerte y TLS del sistema (sin TLS casero) con un cliente remoto real",
        "el opt-in está aplicado y probado (remote_enabled=false por defecto, arranque abortado con bind no loopback sin opt-in); el cliente remoto real con TLS del sistema queda para la aceptación de acceso remoto completo",
    )

    changed = [
        "crates/larm-api/src/auth.rs",
        "crates/larm-api/src/security.rs",
        "crates/larm-api/src/app.rs",
        "crates/larm-api/src/lib.rs",
        "crates/larm-api/tests/security.rs",
        "crates/larm-api/tests/app.rs",
        "apps/daemon/src/serve.rs",
        "apps/daemon/tests/h06_matrix.rs",
        "apps/daemon/tests/auth_security.rs",
        "docs/implementation/STATE.md",
        "docs/reports/P131_secretos-auth-local-y-permisos-api.md",
        "docs/reports/P131_secretos-auth-local-y-permisos-api.evidence.json",
        "tools/p131_evidence.py",
    ]
    blockers = [
        "arranque/parada e2e con lease ACTIVE: exige leases reales con runtime instalado mediante lock cualificado (P100/P031/P099 en BLOCKED_REAL)",
        "bind remoto con cliente remoto real y TLS del sistema: opt-in aplicado y probado en loopback; la aceptación remota completa queda pendiente",
    ]
    status = ev.finish(changed, blockers)
    return 0 if "FAIL" not in status else 1


if __name__ == "__main__":
    raise SystemExit(main())
