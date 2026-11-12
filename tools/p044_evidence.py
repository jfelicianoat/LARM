#!/usr/bin/env python3
"""Evidencia reproducible de búsqueda federada (P044)."""

from __future__ import annotations

from evidence import Evidence, REPO, cargo_ok, exit_zero


def main() -> int:
    ev = Evidence("P044")
    ev.run(
        "P044-P041-ATOMIC-INGESTION", "INTEGRATION",
        ["cargo", "test", "-p", "larm-models", "--test", "acquisition", "-j1", "--", "--test-threads=1"],
        "El resolver P041 prepara identidad y anotaciones recíprocas sin escribir; el writer confirma grafo/snapshot/outbox juntos. Hashes requeridos incompletos no fusionan; certezas, replay, concurrencia y rollback se verifican con SQLite y guard CODE_PASS explícito.",
        ok=cargo_ok, fixtures=[REPO / "crates/larm-models/src/acquisition.rs", REPO / "crates/larm-models/tests/acquisition.rs", REPO / "crates/larm-models/src/registry.rs"],
    )
    ev.run(
        "P044-P041-QUALIFIED-NATIVE-INGESTION", "REAL",
        ["cargo", "test", "-p", "larm-daemon", "--test", "reference_adapters",
         "source_catalogo_cualificado_resuelve_p041_y_persiste_sin_bundle_fisico", "-j1", "--", "--exact", "--test-threads=1", "--nocapture"],
        "Fuente Core creada deshabilitada; su UUID se liga al perfil fixture antes de P026 completo. RPC cualificado original produce seis variantes para P041 y persistencia atómica sin bundle físico ni lectura de artefactos durante la ingesta. No prueba búsqueda HTTP, primera descarga Core ni GUI.",
        ok=cargo_ok, fixtures=[REPO / "apps/daemon/tests/reference_adapters.rs", REPO / "apps/daemon/tests/support/source_acquisition.rs", REPO / "crates/larm-models/src/acquisition.rs"],
    )
    ev.run(
        "P044-SOURCE-ACQUISITION-BINDING", "CONTRACT",
        ["cargo", "test", "-p", "larm-adapter-sdk", "--test", "source_acquisition", "-j1", "--", "--test-threads=1"],
        "Referencia pública y mapa completo de UUID centrales; selección opcional y eco sellado; cambios de revisión/manifiesto/selección fallan incluso al recalcular el SHA del plan.",
        ok=cargo_ok, fixtures=[REPO / "crates/larm-adapter-sdk/src/source/acquisition.rs", REPO / "tests/fixtures/source-acquisition-plan.json"],
    )
    ev.run(
        "P044-SOURCE-ACQUISITION-NATIVE", "REAL",
        ["cargo", "test", "-p", "larm-daemon", "--test", "reference_adapters",
         "source_acquisition::public_binding_native_selects_core_uuid_optionals_and_bounded_bytes", "-j1", "--", "--exact", "--test-threads=1", "--nocapture"],
        "Worker Python original por P022 nativo: seis variantes, UUID central distinto, selección con/sin opcional y SHA de transferencia HTTP acotada. Catálogo fixture explícito; no acredita ingesta Core, cualificación P026 completa ni GUI.",
        ok=cargo_ok, fixtures=[REPO / "apps/daemon/tests/support/source_acquisition.rs", REPO / "adapters/source/reference/adapter.py"],
    )
    ev.run(
        "P044-ACQUISITION-DOMAIN", "CONTRACT",
        ["cargo", "test", "-p", "larm-domain", "--lib", "acquisition::tests", "-j1", "--", "--test-threads=1"],
        "Snapshot remoto cerrado y sellado, certezas conservadas, identidades/rutas/tamaños incoherentes rechazados; sin bytes VERIFIED ficticios.",
        ok=cargo_ok, fixtures=[REPO / "crates/larm-domain/src/acquisition.rs", REPO / "docs/contracts/REVISION_3_12_0_P044_P052.md"],
    )
    ev.run(
        "P044-ACQUISITION-PERSISTENCE", "INTEGRATION",
        ["cargo", "test", "-p", "larm-db", "--test", "acquisition", "-j1", "--", "--test-threads=1"],
        "SQLite append-only y replay concurrente, reinicio y rollback de snapshot/evento/outbox, relaciones y cualificación comprobadas. Guard/recibos sintéticos: no cualifica workers.",
        ok=cargo_ok, fixtures=[REPO / "crates/larm-db/src/acquisition.rs", REPO / "crates/larm-db/tests/acquisition.rs", REPO / "migrations/0014_model_acquisition_manifest.sql"],
    )
    ev.run(
        "P044-ATOMIC-CATALOG-PERSISTENCE", "INTEGRATION",
        ["cargo", "test", "-p", "larm-db", "--test", "catalog_acquisition", "-j1", "--", "--test-threads=1"],
        "Grafo de identidades centrales ya resueltas y snapshot/eventos/outbox atómicos; cuatro certezas, replay, concurrencia, reinicio, revisión de resolución y rollback tardío. SQLite real y guard CODE_PASS sintético; no ejecuta el resolver P041 ni workers.",
        ok=cargo_ok, fixtures=[REPO / "crates/larm-db/src/catalog_acquisition.rs", REPO / "crates/larm-db/tests/catalog_acquisition.rs", REPO / "crates/larm-db/tests/support/acquisition_fixture.rs"],
    )
    ev.run(
        "P044-ACQUISITION-PLANNER", "UNIT",
        ["cargo", "test", "-p", "larm-download", "--lib", "acquisition", "-j1", "--", "--test-threads=1"],
        "Selección remota pura sin bundle físico, UNKNOWN requiere presupuesto, intervalo conserva punto desconocido y cota estimada; el binding se sella por separado del hash upstream.",
        ok=cargo_ok, fixtures=[REPO / "crates/larm-download/src/planner.rs", REPO / "crates/larm-download/src/activation_service.rs"],
    )
    domain = REPO / "crates/larm-domain/src/search.rs"
    core = REPO / "crates/larm-core/src/search.rs"
    engine = REPO / "crates/larm-models/src/search.rs"
    api = REPO / "crates/larm-api/src/search.rs"
    api_test = REPO / "crates/larm-api/tests/app.rs"
    daemon = REPO / "apps/daemon/src/sources.rs"
    serve = REPO / "apps/daemon/src/serve.rs"

    ev.run(
        "P044-FMT",
        "UNIT",
        ["cargo", "fmt", "--all", "--check"],
        expected="Todo el Rust del workspace está formateado.",
        ok=exit_zero,
        fixtures=[domain, core, engine, api, daemon, serve],
    )
    ev.run(
        "P044-CLIPPY",
        "CONTRACT",
        ["cargo", "clippy", "--workspace", "--all-targets", "--", "-D", "warnings"],
        expected="Clippy termina sin warnings en todos los targets del workspace.",
        ok=exit_zero,
        fixtures=[domain, core, engine, api, daemon, serve],
    )
    ev.run(
        "P044-DOMAIN-SEARCH-H05",
        "CONTRACT",
        [
            "cargo", "test", "-p", "larm-domain", "--lib", "search::tests",
            "-j", "1", "--", "--test-threads=1",
        ],
        expected=(
            "Search estricto valida los filtros, los rangos exactos, los límites y "
            "el filter_digest de H05."
        ),
        ok=cargo_ok,
        fixtures=[domain],
    )
    ev.run(
        "P044-FEDERATED-SEARCH-ENGINE",
        "CONTRACT",
        [
            "cargo", "test", "-p", "larm-models", "--lib", "search::tests",
            "-j", "1", "--", "--test-threads=1",
        ],
        expected=(
            "Dos fuentes se mezclan y deduplican; se recorren páginas upstream antes del "
            "filtrado global; timeout, cancelación, TTL, invalidación, filtros, cursores y "
            "advertencias conservan el resultado correcto."
        ),
        ok=cargo_ok,
        fixtures=[engine, core],
    )
    ev.run(
        "P044-API-SCOPE-IDEMPOTENCY",
        "CONTRACT",
        [
            "cargo", "test", "-p", "larm-api", "--test", "app",
            "search_respeta_scope_schema_e_idempotencia", "-j", "1", "--",
            "--exact", "--test-threads=1",
        ],
        expected=(
            "POST /models/search usa Search estricto, scope read, 201 e Idempotency-Key; "
            "replay no repite la búsqueda, la misma clave con otro cuerpo falla y read "
            "no autoriza /sources/search."
        ),
        ok=cargo_ok,
        fixtures=[api, api_test],
    )
    ev.run(
        "P044-DAEMON-NO-FALSE-EMPTY",
        "CONTRACT",
        [
            "cargo", "test", "-p", "larm-daemon", "--bin", "larm-daemon",
            "sources::tests::busqueda_no_cualificada_devuelve_error_y_no_catalogo_vacio",
            "-j", "1", "--", "--exact", "--test-threads=1",
        ],
        expected=(
            "Sin un host source cualificado, la búsqueda responde UPSTREAM_UNAVAILABLE "
            "y nunca simula un catálogo vacío."
        ),
        ok=cargo_ok,
        fixtures=[daemon, serve],
    )
    ev.blocked(
        "P044-REAL-WINDOWS-TWO-UPSTREAMS",
        "REAL",
        "Daemon Windows con dos fuentes autenticadas y adaptadores source instalados; "
        "buscar en ambas, forzar timeout de una y observar respuesta parcial, grupos y cursor real.",
        expected=(
            "Dos catálogos upstream reales producen modelos normalizados; una fuente lenta "
            "conserva los resultados de la otra y publica una advertencia; la paginación "
            "global no duplica ni omite modelos."
        ),
        reason=(
            "El daemon aún no tiene un ejecutor que lance source.search bajo P022 con "
            "credenciales por cuenta; su provider devuelve UPSTREAM_UNAVAILABLE. P041 y "
            "P043 siguen BLOCKED_REAL, por lo que esta aceptación no se puede declarar PASS."
        ),
    )

    ev.finish(
        changed_files=[
            "crates/larm-models/src/acquisition.rs",
            "crates/larm-models/tests/acquisition.rs",
            "crates/larm-models/src/registry.rs",
            "crates/larm-models/Cargo.toml",
            "crates/larm-db/src/catalog_acquisition.rs",
            "crates/larm-db/tests/catalog_acquisition.rs",
            "crates/larm-db/tests/support/acquisition_fixture.rs",
            "crates/larm-db/src/models.rs",
            "crates/larm-db/src/variants.rs",
            "crates/larm-adapter-sdk/src/source/acquisition.rs",
            "crates/larm-adapter-sdk/tests/source_acquisition.rs",
            "adapters/sdk/python/larm_adapter_sdk/source_acquisition.py",
            "apps/daemon/tests/support/source_acquisition.rs",
            "docs/contracts/REVISION_3_12_1_SOURCE_ACQUISITION.md",
            "crates/larm-domain/src/acquisition.rs",
            "crates/larm-domain/src/download.rs",
            "crates/larm-db/src/acquisition.rs",
            "crates/larm-db/tests/acquisition.rs",
            "migrations/0014_model_acquisition_manifest.sql",
            "docs/contracts/REVISION_3_12_0_P044_P052.md",
            "docs/reports/P044_revision_3_12_0_2026-10-27.md",
            "Cargo.lock",
            "apps/daemon/src/main.rs",
            "apps/daemon/src/model_search.rs",
            "apps/daemon/src/serve.rs",
            "apps/daemon/src/sources.rs",
            "crates/larm-api/src/lib.rs",
            "crates/larm-api/src/search.rs",
            "crates/larm-api/tests/app.rs",
            "crates/larm-core/Cargo.toml",
            "crates/larm-core/src/lib.rs",
            "crates/larm-core/src/search.rs",
            "crates/larm-domain/src/search.rs",
            "crates/larm-models/Cargo.toml",
            "crates/larm-models/src/lib.rs",
            "crates/larm-models/src/search.rs",
            "tools/p044_evidence.py",
        ],
        blockers=[
            "No se ejecutó el escenario G02 contra dos fuentes reales: falta el ejecutor productivo de ModelSourceAdapter con autorización y credenciales P022/P045.",
        ],
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
