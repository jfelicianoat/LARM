#!/usr/bin/env python3
"""P119: puerta de la admisión autorizada de descargas automáticas —
evaluador determinista del dominio, repositorio SQL con cuota diaria y
revocación, API de política/admisiones y el flujo REAL sobre SQLite migrado
(notificar sin descarga → cuota pequeña → admisión/bloqueo → revocación)."""

import sys

from evidence import Evidence, REPO

TOOL_PYTHON = REPO / ".venv-tools" / "Scripts" / "python.exe"
PYTHON = str(TOOL_PYTHON) if TOOL_PYTHON.is_file() else sys.executable


def main() -> int:
    ev = Evidence("P119")

    ev.run(
        "P119-CONTRACT-domain-evaluator",
        "CONTRACT",
        ["cargo", "test", "-p", "larm-domain", "--lib", "download_admission"],
        "evaluador determinista: opt-in apagado sólo notifica, allowlists de fuente/capability/license separadas, trust por debajo del mínimo bloquea, cuota diaria agregada bloquea el excedente, política inválida falla cerrado",
    )
    ev.run(
        "P104-CARRIED-contractual-gates",
        "CONTRACT",
        ["cargo", "test", "-p", "larm-db", "--test", "download_admissions"],
        "política singleton versionada con journal/evento/outbox, cuota diaria agregada por day_key (800 admitido, excedente bloqueado), revocación inmediata con triggers de inmutabilidad y listado",
    )
    ev.run(
        "P119-CONTRACT-api-routes",
        "CONTRACT",
        ["cargo", "test", "-p", "larm-api", "--lib"],
        "PUT/GET de política con If-Match (stale ante versión antigua), GET de admisiones paginado y revocación POST sobre el repositorio real",
    )

    def real_ok(code: int, output: str) -> bool:
        # El test imprime "REAL P119: ..." con la política versionada y la
        # revocación; el bloqueo por cuota se verifica con su aserto interno.
        return (
            code == 0
            and "test result: ok" in output
            and "REAL P119" in output
            and "habilitada" in output and "ADMITTED" in output
        )

    ev.run(
        "P119-REAL-quota-admission-and-revocation",
        "REAL",
        ["cargo", "test", "-p", "larm-daemon", "--test", "download_admission", "--", "--nocapture"],
        "flujo contractual REAL en el PC IA sobre SQLite migrado (0016): sin política sólo notifica (policy_disabled), cuota pequeña autorizada (1000 bytes) admite dos descargas de 400 y bloquea la tercera (admission_quota_exhausted), revocación inmediata REVOKED y política persistida sobrevive re-open",
        ok=real_ok,
        timeout=600,
    )
    ev.blocked(
        "P119-REAL-watcher-auto-job-end-to-end",
        "REAL",
        "auto-job BACKGROUND end-to-end desde la sincronización del watcher con un job descargado por el consumer",
        "detección compatible en fuente controlada → admisión con cuota → plan+activación BACKGROUND con job BACKGROUND en el consumer",
        "el activador automático de plan/activación desde el watcher requiere el plan preview real de la fuente (P044 en BLOCKED_REAL) y los locks cualificados de P099/P031 para runtime; la evaluación, cuota, revocación y persistencia sí están verificadas sobre SQLite migrado",
    )

    changed = [
        "tools/p119_contract_revision.py",
        "crates/larm-domain/src/lib.rs",
        "crates/larm-domain/src/download_admission.rs",
        "crates/larm-db/src/lib.rs",
        "crates/larm-db/src/migrate.rs",
        "crates/larm-db/src/download_admissions.rs",
        "crates/larm-db/tests/download_admissions.rs",
        "crates/larm-db/tests/adapter_selection.rs",
        "crates/larm-db/tests/integration.rs",
        "crates/larm-db/src/source_watches.rs",
        "crates/larm-api/src/lib.rs",
        "crates/larm-api/src/download_admissions.rs",
        "crates/larm-api/src/openapi.rs",
        "crates/larm-domain/src/auth.rs",
        "apps/daemon/src/serve.rs",
        "apps/daemon/Cargo.toml",
        "apps/daemon/tests/download_admission.rs",
        "migrations/0016_p119_download_admission.sql",
        "docs/implementation/STATE.md",
        "docs/reports/P119_admision-autorizada-de-descargas-automaticas.md",
        "docs/reports/P119_admision-autorizada-de-descargas-automaticas.evidence.json",
        "tools/p119_evidence.py",
    ]
    blockers = [
        "auto-job BACKGROUND end-to-end desde watcher: requiere el plan preview real de la fuente (P044 BLOCKED_REAL) y locks cualificados (P099/P031); la política, cuota, revocación y persistencia están verificadas",
        "UI de historial/revocación: la pantalla de descargas consume las mismas rutas contratadas; el panel dedicado queda para el cierre de UI de P129/P139",
    ]
    status = ev.finish(changed, blockers)
    return 0 if "FAIL" not in status else 1


if __name__ == "__main__":
    raise SystemExit(main())
