#!/usr/bin/env python3
"""P009 - autenticación, secretos y autorizaciones: pruebas y evidencia."""
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from evidence import REPO, Evidence, cargo_ok  # noqa: E402

PY = sys.executable


def main() -> int:
    ev = Evidence("P009", extra_inputs=["contracts/mcp-permissions.json"])
    fx = [REPO / "docs/contracts/api-operations.json", REPO / "docs/contracts/mcp-permissions.json"]
    lib = [
        ("P009-CONTRACT-catalog", "larm-domain", "auth::tests::el_catalogo", "142 operaciones y 12 herramientas MCP del contrato; rutas literales y con parámetro"),
        ("P009-CONTRACT-h06-start-stop-install-update", "larm-domain", "auth::tests::h06_start", "start sin stop, install sin update"),
        ("P009-CONTRACT-h06-read-search-move-delete", "larm-domain", "auth::tests::h06_read", "read/search y move/delete separados"),
        ("P009-CONTRACT-h06-scope-api", "larm-domain", "auth::tests::h06_sin_scope", "sin scope API se niega aunque haya permiso MCP"),
        ("P009-CONTRACT-h06-admin", "larm-domain", "auth::tests::h06_admin", "admin MCP no añade scopes API; admin API no añade herramientas"),
        ("P009-CONTRACT-h06-direct-api", "larm-domain", "auth::tests::h06_mcp_por_api", "MCP por API directa a operación sin herramienta: FORBIDDEN"),
        ("P009-CONTRACT-broker-scopes", "larm-domain", "auth::tests::broker", "Broker solo read+leases:manage; credentials:read distinto de read"),
        ("P009-FAULT-contract-contradiction", "larm-domain", "auth::tests::contrato_contradictorio", "contratos contradictorios o con scopes desconocidos se rechazan"),
        ("P009-UNIT-approval", "larm-domain", "approval::tests", "concesión con TTL, uso exacto; caducada/consumida/actor/digest/recursos/código distintos rechazados"),
        ("P009-UNIT-canonical", "larm-domain", "canonical::tests", "JSON canónico RFC 8785 (orden UTF-16), hash estable"),
        ("P009-UNIT-credential-write", "larm-domain", "credential::tests", "espacio larm/, reservados, secreto oculto en Debug, campos extra rechazados"),
        ("P009-UNIT-principals", "larm-core", "security::tests::alta", "alta y autenticación; token fuera de la referencia"),
        ("P009-FAULT-rotation", "larm-core", "security::tests::rotar", "rotar revoca el token anterior inmediatamente"),
        ("P009-FAULT-rotation-external", "larm-core", "security::tests::rotacion_desde_otro", "rotación desde otro proceso revoca al caducar la caché (5 s)"),
        ("P009-FAULT-backend-down", "larm-core", "security::tests::almacen_caido", "Credential Manager caído: 503 reintentable, falla cerrado, sin alta a medias"),
        ("P009-FAULT-credential-compensation", "larm-core", "security::tests::credencial_guarda", "secreto aparte; duplicado no sobrescribe; fallo de base retira el secreto"),
        ("P009-UNIT-revoke", "larm-core", "security::tests::revocar", "revocar borra token y referencia"),
        ("P009-UNIT-cursor", "larm-core", "security::tests::paginacion", "paginación sin huecos; cursor alterado o de otro arranque rechazado"),
        ("P009-UNIT-hmac", "larm-core", "security::tests::hmac", "HMAC-SHA256 = RFC 4231 caso 2"),
    ]
    for test_id, crate, filt, expected in lib:
        kind = test_id.split("-")[1]
        ev.run(test_id, kind, ["cargo", "test", "-p", crate, "--lib", "--", filt], expected, ok=cargo_ok, fixtures=fx)
    ev.run("P009-INTEGRATION-api", "INTEGRATION", ["cargo", "test", "-p", "larm-api", "--test", "security"],
           "router real: 401/403/503, autenticar antes del cuerpo, idempotencia, If-Match, paginación, correlación, sin secretos",
           ok=cargo_ok, fixtures=fx)
    ev.run("P009-INTEGRATION-sqlite", "INTEGRATION", ["cargo", "test", "-p", "larm-db", "--test", "security"],
           "evento+outbox en la misma transacción; duplicado, stale; aprobación consumida una vez con 8 usos concurrentes; idempotencia caduca",
           ok=cargo_ok)
    ev.run("P009-UNIT-pipe-acl", "UNIT", ["cargo", "test", "-p", "larm-daemon", "--bin", "larm-daemon", "--", "winsec", "bootstrap", "bridge"],
           "SDDL solo para SIDs válidos; nombre del pipe por SID; config del puente sin principales", ok=cargo_ok)
    ev.run("P009-CONTRACT-unsafe-exception", "CONTRACT", [PY, "tools/verify_p001.py", "policies"],
           "unsafe solo en apps/daemon/src/winsec.rs, documentado en ADR-0009")
    ev.run("P009-UNIT-cargo-fmt", "UNIT", ["cargo", "fmt", "--all", "--check"], "sin diferencias de formato")
    ev.run("P009-UNIT-cargo-clippy", "UNIT", ["cargo", "clippy", "--workspace", "--all-targets", "--", "-D", "warnings"], "clippy sin avisos")
    ev.run("P009-UNIT-cargo-test", "UNIT", ["cargo", "test", "--workspace"], "pruebas del workspace en verde", ok=cargo_ok)
    ev.run("P009-REAL-architecture", "REAL", ["powershell", "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", "tools/check_architecture.ps1"],
           "el grafo real sigue cumpliendo crate-graph.json tras P009")
    ev.run("P009-REAL-daemon-build", "REAL", ["cargo", "build", "-p", "larm-daemon"], "binario del daemon compilado")
    ev.run("P009-REAL-credential-manager-acl", "REAL", [PY, "tools/p009_real.py", "serve"],
           "daemon real: principales por CLI, 401/403, secreto solo en Credential Manager (CredReadW) y no en SQLite/WAL/logs, "
           "If-Match, rotación externa, bootstrap de un uso, DACL D:P con SID+SY, limpieza sin restos", timeout=600)
    ev.blocked("P009-REAL-second-windows-user", "REAL",
               "conectar al pipe y leer la entrada de Credential Manager desde otra cuenta de Windows",
               "otra cuenta no puede abrir el pipe (ERROR_ACCESS_DENIED) ni leer los secretos del usuario",
               "no hay una segunda cuenta de Windows con credenciales para la prueba; crearla modifica el sistema y requiere "
               "autorización del operador. La DACL efectiva sí se verificó (solo SID del usuario y SYSTEM)")
    changed = [
        "crates/larm-domain/src/auth.rs", "crates/larm-domain/src/approval.rs", "crates/larm-domain/src/canonical.rs",
        "crates/larm-domain/src/credential.rs", "crates/larm-domain/src/ports.rs", "crates/larm-domain/src/lib.rs",
        "crates/larm-domain/Cargo.toml", "crates/larm-core/src/security/mod.rs", "crates/larm-core/src/security/memory.rs",
        "crates/larm-core/Cargo.toml", "crates/larm-db/src/events.rs", "crates/larm-db/src/security.rs",
        "crates/larm-db/tests/security.rs", "crates/larm-db/Cargo.toml", "crates/larm-api/src/http.rs",
        "crates/larm-api/src/security.rs", "crates/larm-api/src/broker.rs", "crates/larm-api/tests/security.rs",
        "crates/larm-api/Cargo.toml", "crates/larm-runtime/src/credentials_pipe.rs",
        "crates/larm-runtime/src/external_adoption.rs", "crates/larm-runtime/src/external_adoption/tests.rs",
        "apps/daemon/src/main.rs", "apps/daemon/src/admin.rs", "apps/daemon/src/serve.rs", "apps/daemon/src/bootstrap.rs",
        "apps/daemon/src/bridge.rs", "apps/daemon/src/winsec.rs", "apps/daemon/Cargo.toml", "Cargo.toml",
        "docs/adr/0009-excepcion-unsafe-dacl-pipes.md", "docs/operations/security.md", "tools/p009_real.py",
        "tools/p009_evidence.py", "tools/verify_p001.py", "tools/verify_p002.py",
    ]
    ev.finish(changed, ["prueba con un segundo usuario de Windows pendiente de autorización del operador"])
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
