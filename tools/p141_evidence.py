#!/usr/bin/env python3
"""Evidencia de la resolución de cuenta para probes Basic dentro de P141."""

import sys

from evidence import Evidence, REPO, cargo_ok, exit_zero


def main() -> int:
    ev = Evidence("P141")
    python = REPO / ".venv-tools/Scripts/python.exe"
    ev.run(
        "P141-CONTRACT-SCHEMAS",
        "CONTRACT",
        [str(python), "tools/p026_schemas.py", "check"],
        "AuthProbe incluye account_id nullable obligatorio y sus schemas cerrados se generan desde los contratos canónicos.",
        ok=exit_zero,
        fixtures=[REPO / "docs/contracts/schemas/larm.schema.json", REPO / "docs/adapters/method-schemas/source.auth_probe.request.schema.json"],
    )
    ev.run(
        "P141-OPENAPI-SYNC",
        "CONTRACT",
        ["cargo", "run", "-p", "larm-daemon", "--", "openapi", "--out", "docs/api/openapi.json", "--check"],
        "El OpenAPI generado conserva account_id nullable y obligatorio en AuthProbe y coincide con el artefacto publicado.",
        ok=exit_zero,
        fixtures=[REPO / "crates/larm-api/src/openapi.rs", REPO / "docs/contracts/schemas/larm.schema.json", REPO / "docs/api/openapi.json"],
    )
    ev.run(
        "P141-SDK-SOURCE-PROTOCOL",
        "CONTRACT",
        ["cargo", "test", "-p", "larm-adapter-sdk", "--test", "source_protocol", "-j", "1", "--", "--test-threads=1"],
        "El SDK valida AuthProbe cerrado, UUIDv7, account_id/ref y roundtrip contractual de sus peticiones.",
        ok=cargo_ok,
        fixtures=[REPO / "crates/larm-adapter-sdk/src/source/wire.rs", REPO / "crates/larm-adapter-sdk/tests/source_protocol.rs"],
    )
    ev.run(
        "P141-HOST-CREDENTIAL-BINDING",
        "INTEGRATION",
        ["cargo", "test", "-p", "larm-daemon", "source_probes", "-j", "1", "--", "--test-threads=1"],
        "El host comprueba propietario, cuenta, referencia, tipo y permiso exacto antes de iniciar el worker; los casos inválidos fallan cerrados.",
        ok=cargo_ok,
        fixtures=[REPO / "apps/daemon/src/source_probes.rs"],
    )
    ev.run(
        "P141-P048-BASIC-PROBE",
        "UNIT",
        ["cargo", "test", "-p", "larm-source-p048", "-j", "1", "--", "--test-threads=1"],
        "El worker recupera Basic desde el almacén mediante service/account fijados, exige HTTPS salvo loopback de fixture, no sigue redirects y clasifica 401/403 sin revelar secretos.",
        ok=cargo_ok,
        fixtures=[REPO / "adapters/source/p048/src/lib.rs", REPO / "adapters/source/p048/Cargo.toml"],
    )
    ev.run(
        "P141-SOURCE-ACCOUNT-DB",
        "FAULT",
        ["cargo", "test", "-p", "larm-db", "--test", "sources", "-j", "1", "--", "--test-threads=1"],
        "La persistencia mantiene las referencias ligadas a su cuenta/fuente y revierte de forma atómica los fallos de outbox.",
        ok=cargo_ok,
        fixtures=[REPO / "crates/larm-db/tests/sources.rs"],
    )
    ev.run(
        "P141-DESKTOP-BASIC-ACCESS",
        "INTEGRATION",
        ["node", "node_modules/vitest/vitest.mjs", "run", "--pool=threads", "--maxWorkers=1", "--root", "apps/desktop", "src/sources.test.tsx"],
        "El escritorio crea y vincula cuentas con If-Match, guarda Basic por la ruta protegida y limpia usuario/contraseña al terminar.",
        ok=exit_zero,
        fixtures=[REPO / "apps/desktop/src/sources.tsx", REPO / "apps/desktop/src/sources.test.tsx"],
    )
    ev.run(
        "P141-CAPABILITY-INDEX",
        "CONTRACT",
        [str(python), "tools/p026_capability_schemas.py", "check"],
        "El índice de capacidades refleja los hashes fuente del schema actualizado y el código Rust derivado coincide.",
        ok=exit_zero,
        fixtures=[REPO / "docs/adapters/capability-schemas/index.json", REPO / "crates/larm-adapter-sdk/src/capabilities/builtin.rs"],
    )
    ev.run(
        "P141-PACKAGE-COPY",
        "CONTRACT",
        [str(python), "tools/package_copy.py", "check"],
        "Las copias normativas locales coinciden byte a byte con el paquete contractual 3.8.0.",
        ok=exit_zero,
        fixtures=[REPO / "docs/spec/PACKAGE_COPY_HASHES.json", REPO / "docs/contracts/REVISION_3_8_0_P141.md"],
    )
    ev.blocked(
        "P141-OAUTH-TOKEN-SSH-COOKIE-GIT-LFS",
        "INTEGRATION",
        "tests de API_TOKEN, OAuth device/PKCE, refresh/revoke, SSH agent/host key, cookies y Git LFS",
        "Cada mecanismo anunciado tiene perfil cualificado, flujo real, fallos cerrados, redacción y pruebas contractuales propias.",
        "El vínculo de cuenta, el probe Basic de P048 y su recorrido de escritorio están implementados; API_TOKEN, OAuth, SSH, cookies y Git LFS siguen sin implementación cualificada.",
    )
    ev.blocked(
        "P141-PRIVATE-SOURCE-REAL",
        "REAL",
        "tools/check.ps1 -Prompt P141 -Real con una fuente privada autorizada",
        "Una fuente privada autorizada completa el probe y verifica renovación/revocación sin filtrar credenciales.",
        "No hay una fuente privada ni una cuenta de prueba autorizadas disponibles en este entorno; no se reutilizan tokens de otras integraciones.",
    )
    status = ev.finish(
        changed_files=[
            "apps/daemon/src/source_probes.rs",
            "apps/desktop/src/sources.tsx",
            "apps/desktop/src/sources.test.tsx",
            "tools/p141_evidence.py",
            "docs/reports/P141_revision-contractual-oauth-propuesta.md",
            "crates/larm-adapter-sdk/src/source/wire.rs",
            "crates/larm-adapter-sdk/tests/source_protocol.rs",
            "crates/larm-adapter-host/tests/source.rs",
            "crates/larm-adapter-sdk/src/capabilities/builtin.rs",
            "adapters/source/p048/Cargo.toml",
            "adapters/source/p048/src/lib.rs",
            "docs/contracts/00_PACKAGE.md",
            "docs/contracts/03_ADAPTERS_SECURITY.md",
            "docs/contracts/REVISION_3_8_0_P141.md",
            "docs/api/openapi.json",
            "docs/contracts/crate-graph.json",
            "docs/contracts/schemas/larm.schema.json",
            "docs/adapters/method-schemas/source.auth_probe.request.schema.json",
            "docs/adapters/capability-schemas/index.json",
            "../LARM_prompts_v3/contracts/00_PACKAGE.md",
            "../LARM_prompts_v3/contracts/03_ADAPTERS_SECURITY.md",
            "../LARM_prompts_v3/contracts/REVISION_3_8_0_P141.md",
            "../LARM_prompts_v3/contracts/crate-graph.json",
            "../LARM_prompts_v3/contracts/schemas/larm.schema.json",
        ],
        blockers=[
            "P141 aún no implementa API_TOKEN, OAuth device/PKCE con refresh/revoke, SSH agent/host key, cookies ni Git LFS.",
            "Falta una prueba con fuente privada y cuenta autorizadas, incluido el ciclo de renovación/revocación.",
        ],
    )
    return 0 if status in ("CODE_PASS", "REAL_PASS", "BLOCKED_REAL") else 1


if __name__ == "__main__":
    sys.exit(main())
