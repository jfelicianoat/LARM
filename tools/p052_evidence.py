#!/usr/bin/env python3
"""Evidencia focal P052; preflight HTTP parcial y validación local de contrato."""

from evidence import Evidence, REPO, cargo_ok, exit_zero


def main() -> int:
    ev = Evidence("P052")
    ev.run(
        "P052-ACQUISITION-DOMAIN", "CONTRACT",
        ["cargo", "test", "-p", "larm-domain", "--lib", "acquisition::tests", "-j1", "--", "--test-threads=1"],
        "Snapshot remoto cerrado y sellado, certezas conservadas, identidades/rutas/tamaños incoherentes rechazados; sin bytes VERIFIED ficticios.",
        ok=cargo_ok, fixtures=[REPO / "crates/larm-domain/src/acquisition.rs", REPO / "docs/contracts/REVISION_3_12_0_P044_P052.md"],
    )
    ev.run(
        "P052-ACQUISITION-PERSISTENCE", "INTEGRATION",
        ["cargo", "test", "-p", "larm-db", "--test", "acquisition", "-j1", "--", "--test-threads=1"],
        "SQLite append-only y replay concurrente, reinicio y rollback de snapshot/evento/outbox, relaciones y cualificación comprobadas. Guard/recibos sintéticos: no cualifica workers.",
        ok=cargo_ok, fixtures=[REPO / "crates/larm-db/src/acquisition.rs", REPO / "crates/larm-db/tests/acquisition.rs", REPO / "migrations/0014_model_acquisition_manifest.sql"],
    )
    ev.run(
        "P052-ACQUISITION-PLANNER", "UNIT",
        ["cargo", "test", "-p", "larm-download", "--lib", "acquisition", "-j1", "--", "--test-threads=1"],
        "Selección remota pura sin bundle físico, UNKNOWN requiere presupuesto, intervalo conserva punto desconocido y cota estimada; el binding se sella por separado del hash upstream.",
        ok=cargo_ok, fixtures=[REPO / "crates/larm-download/src/planner.rs", REPO / "crates/larm-download/src/activation_service.rs"],
    )
    planner = REPO / "crates/larm-download/src/planner.rs"
    storage = REPO / "crates/larm-storage/src/estimate.rs"
    source_wire = REPO / "crates/larm-adapter-sdk/src/source/wire.rs"
    source_test = REPO / "crates/larm-adapter-sdk/tests/source_protocol.rs"
    download_manifest = REPO / "crates/larm-download/Cargo.toml"
    contract_schema = REPO / "docs/contracts/schemas/larm.schema.json"
    contract_revision = REPO / "docs/contracts/REVISION_3_12_0_P044_P052.md"
    package_verify = REPO.parent / "LARM_prompts_v3/tools/verify_pack.py"
    adapter_schema_generator = REPO / "tools/p026_schemas.py"
    adapter_method_schemas = REPO / "docs/adapters/method-schemas/source.get_manifest.response.schema.json"
    capability_schema_generator = REPO / "tools/p026_capability_schemas.py"
    capability_descriptor_generator = REPO / "tools/p026_reference_capabilities.py"
    domain_model = REPO / "crates/larm-domain/src/model.rs"
    db_variants = REPO / "crates/larm-db/src/variants.rs"
    db_migrations = REPO / "crates/larm-db/src/migrate.rs"
    db_migration = REPO / "migrations/0006_p052_artifact_size_estimate.sql"
    db_integration = REPO / "crates/larm-db/tests/integration.rs"
    model_registry = REPO / "crates/larm-models/src/registry.rs"
    api_models_test = REPO / "crates/larm-api/tests/models.rs"
    api_download_test = REPO / "crates/larm-api/tests/download_plans.rs"
    api_download_route = REPO / "crates/larm-api/src/download_plans.rs"
    preview_service = REPO / "crates/larm-download/src/preview_service.rs"
    consumer = REPO / "crates/larm-download/src/consumer.rs"
    download_jobs = REPO / "crates/larm-db/src/download_jobs.rs"
    openapi = REPO / "docs/api/openapi.json"
    test_temp = REPO / "target/test-tmp"
    test_temp.mkdir(parents=True, exist_ok=True)
    temp_env = {"TEMP": str(test_temp), "TMP": str(test_temp)}

    ev.run(
        "P052-FMT",
        "UNIT",
        ["cargo", "fmt", "--all", "--check"],
        "Todo el Rust del workspace está formateado.",
        fixtures=[planner, storage, download_manifest],
    )
    ev.run(
        "P052-CONTRACT-PACKAGE-3.12.0",
        "CONTRACT",
        [".venv-tools/Scripts/python.exe", "../LARM_prompts_v3/tools/verify_pack.py"],
        "El paquete vigente 3.12.0 separa la fuente principal de cero a dieciséis mirrors alternativos y conserva las reglas de hash, revalidación y fallback por artefacto.",
        fixtures=[contract_schema, contract_revision, package_verify, REPO / "docs/contracts/REVISION_3_1_0_P052.md", REPO / "docs/contracts/REVISION_3_7_0_P052.md", REPO / "docs/contracts/REVISION_3_10_0_P052.md", REPO / "docs/contracts/migrations/0006_p052_artifact_size_estimate.sql"],
    )
    ev.run(
        "P052-PACKAGE-COPIES",
        "CONTRACT",
        [".venv-tools/Scripts/python.exe", "tools/package_copy.py", "check"],
        "Las copias locales coinciden byte a byte con el paquete contractual vigente 3.12.0.",
        fixtures=[REPO / "docs/spec/manifest.json", REPO / "docs/spec/PACKAGE_COPY_HASHES.json", REPO / "tools/package_copy.py"],
    )
    ev.run(
        "P052-SOURCE-ARTIFACT-SIZE-SCHEMA",
        "CONTRACT",
        [".venv-tools/Scripts/python.exe", "tools/p026_schemas.py", "check"],
        "Las firmas de los adapters se regeneran desde el paquete vigente 3.12.0; source.get_manifest transporta la estimación por artefacto con sus límites y evidencia.",
        fixtures=[adapter_schema_generator, adapter_method_schemas, REPO / "docs/adapters/method-schemas/index.json"],
    )
    ev.run(
        "P052-SOURCE-SCHEMA-CORPUS",
        "CONTRACT",
        [".venv-tools/Scripts/python.exe", "tools/p026_schema_corpus.py", "check"],
        "El corpus de las 80 direcciones se regenera desde los schemas y mantiene el mismo veredicto entre Rust y Python.",
        fixtures=[REPO / "tools/p026_schema_corpus.py", REPO / "tests/fixtures/adapters/method-schema-corpus.json", REPO / "crates/larm-adapter-sdk/tests/contracts.rs", REPO / "tools/test_python_contracts.py"],
    )
    ev.run(
        "P052-CAPABILITY-SCHEMAS",
        "CONTRACT",
        [".venv-tools/Scripts/python.exe", "tools/p026_capability_schemas.py", "check"],
        "Las 18 capacidades y sus 36 schemas cerrados coinciden con el paquete contractual 3.12.0.",
        fixtures=[capability_schema_generator, REPO / "docs/adapters/capability-schemas/index.json", REPO / "crates/larm-adapter-sdk/src/capabilities/builtin.rs"],
    )
    ev.run(
        "P052-REFERENCE-CAPABILITY-DESCRIPTORS",
        "CONTRACT",
        [".venv-tools/Scripts/python.exe", "tools/p026_reference_capabilities.py", "check"],
        "Los descriptores source/runtime fijados usan hashes y fixtures válidos contra los schemas revisados.",
        fixtures=[capability_descriptor_generator, REPO / "adapters/source/reference/capabilities.json", REPO / "adapters/runtime/reference/capabilities.json"],
    )
    ev.run(
        "P052-ARTIFACT-SIZE-DOMAIN",
        "UNIT",
        [
            "cargo",
            "test",
            "-p",
            "larm-domain",
            "artifact_size_estimate",
            "--lib",
            "-j",
            "1",
            "--",
            "--test-threads=1",
        ],
        "EXACT/UNKNOWN serializan bytes decimales y se rechazan fuentes, límites o evidencias incoherentes.",
        fixtures=[domain_model],
        ok=cargo_ok,
    )
    ev.run(
        "P052-ARTIFACT-SIZE-MIGRATION",
        "INTEGRATION",
        [
            "cargo",
            "test",
            "-p",
            "larm-db",
            "--test",
            "integration",
            "p052_migra_tamanos_historicos_a_unknown_y_los_expone_conservadoramente",
            "-j",
            "1",
            "--",
            "--exact",
            "--test-threads=1",
        ],
        "0006 conserva expected_size, asigna UNKNOWN a las filas antiguas y el repositorio publica la estimación migrada.",
        fixtures=[db_migrations, db_migration, db_variants, db_integration],
        ok=cargo_ok,
    )
    ev.run(
        "P052-ARTIFACT-SIZE-API-DTO",
        "INTEGRATION",
        ["cargo", "test", "-p", "larm-api", "--test", "models", "-j", "1", "--", "--test-threads=1"],
        "Los endpoints de detalle y artefactos devuelven el DTO tipado size_estimate del contrato.",
        fixtures=[domain_model, db_variants, model_registry, api_models_test],
        ok=cargo_ok,
    )
    ev.run(
        "P052-API-PREFLIGHT-IDEMPOTENCY",
        "INTEGRATION",
        ["cargo", "test", "-p", "larm-api", "--test", "download_plans", "-j", "1", "--", "--test-threads=1"],
        "La ruta valida selección opcional, aplica jobs:write y reproduce previews por Idempotency-Key sin duplicar el cálculo.",
        fixtures=[api_download_route, api_download_test, REPO / "crates/larm-domain/src/download.rs"],
        ok=cargo_ok,
    )
    ev.run(
        "P052-DURABLE-PLAN-JOURNAL",
        "INTEGRATION",
        ["cargo", "test", "-p", "larm-db", "--lib", "download_plans::tests", "-j", "1", "--", "--test-threads=1"],
        "Un destino elegible persiste el plan en operation_journal con su evento outbox; selección inválida y fallo del outbox no dejan efectos parciales.",
        fixtures=[REPO / "crates/larm-db/src/download_plans.rs", REPO / "crates/larm-db/src/events.rs", REPO / "crates/larm-domain/src/download.rs"],
        ok=cargo_ok,
    )
    ev.run(
        "P052-ARTIFACT-SIZE-PREFLIGHT-MAPPING",
        "UNIT",
        ["cargo", "test", "-p", "larm-download", "--lib", "preview_service::tests", "-j", "1", "--", "--test-threads=1"],
        "La conexión del preflight conserva EXACT, intervalo ESTIMATED y UNKNOWN y falla cerrado ante evidencia incoherente.",
        fixtures=[preview_service, planner, domain_model],
        ok=cargo_ok,
    )
    ev.run(
        "P052-MIS-MODELOS-DESKTOP",
        "INTEGRATION",
        ["npm.cmd", "run", "test", "--prefix", "apps/desktop", "--", "src/models.test.tsx"],
        "Mis modelos separa biblioteca y comparación, presenta tamaños exactos/estimados/desconocidos y no inicia una descarga desde el preflight.",
        fixtures=[REPO / "apps/desktop/src/models.tsx", REPO / "apps/desktop/src/models.test.tsx"],
        ok=exit_zero,
    )
    ev.run(
        "P052-LOCAL-IMPORT-SOURCE-DB",
        "INTEGRATION",
        [
            "cargo", "test", "-p", "larm-db", "--lib",
            "model_import::tests::crea_una_fuente_por_carpeta_y_reutiliza_la_misma_en_reintentos",
            "-j", "1", "--", "--exact", "--test-threads=1",
        ],
        "La importación crea una fuente local automática por carpeta canónica y reutiliza esa fuente en reintentos.",
        fixtures=[REPO / "crates/larm-db/src/model_import.rs", REPO / "migrations/0007_p098_local_import_source.sql"],
        ok=cargo_ok,
    )
    ev.run(
        "P052-LOCAL-IMPORT-WORKER",
        "INTEGRATION",
        [
            "cargo", "test", "-p", "larm-models", "--test", "import_worker",
            "job_persistido_importa_una_carpeta_verificada_y_devuelve_su_resultado",
            "-j", "1", "--", "--exact", "--test-threads=1",
        ],
        "El worker procesa la carpeta importada desde el job durable con la fuente local registrada.",
        fixtures=[REPO / "crates/larm-models/src/import_worker.rs", REPO / "crates/larm-models/tests/import_worker.rs"],
        ok=cargo_ok,
    )
    ev.run(
        "P052-PLANNER-CASES",
        "CONTRACT",
        [
            "cargo",
            "test",
            "-p",
            "larm-download",
            "--lib",
            "planner::tests",
            "-j",
            "1",
            "--",
            "--test-threads=1",
        ],
        "Bundle exacto, unknown/rango, selección opcional, destinos, presupuesto duro y lista vacía, orden y límite de mirrors pasan sus oráculos.",
        fixtures=[planner, storage],
        ok=cargo_ok,
    )
    ev.run(
        "P052-STORAGE-ESTIMATOR",
        "CONTRACT",
        [
            "cargo",
            "test",
            "-p",
            "larm-storage",
            "--lib",
            "estimate::tests",
            "-j",
            "1",
            "--",
            "--test-threads=1",
        ],
        "El planificador consume el estimador P015 para cotas, presupuesto y admisión por volumen.",
        fixtures=[storage],
        ok=cargo_ok,
    )
    ev.run(
        "P052-SOURCE-PLAN-PRIORITY",
        "CONTRACT",
        [
            "cargo",
            "test",
            "-p",
            "larm-adapter-sdk",
            "--test",
            "source_protocol",
            "presupuesto_y_prioridad_no_desaparecen_y_unknown_no_es_cero",
            "-j",
            "1",
            "--",
            "--exact",
            "--test-threads=1",
        ],
        "La prioridad queda fijada en el plan sellado; cambiarla invalida el plan y no convierte UNKNOWN en cero.",
        fixtures=[source_wire, source_test, planner],
        ok=cargo_ok,
    )
    ev.run(
        "P052-CLIPPY-WORKSPACE",
        "CONTRACT",
        [
            "cargo",
            "clippy",
            "--workspace",
            "--all-targets",
            "-j",
            "1",
            "--",
            "-D",
            "warnings",
        ],
        "Todo el workspace pasa Clippy sin warnings.",
        fixtures=[domain_model, db_variants, db_migrations, model_registry, planner, storage, download_manifest],
    )
    ev.run(
        "P052-TESTS-DOWNLOAD-DB",
        "INTEGRATION",
        ["cargo", "test", "-p", "larm-download", "-p", "larm-db", "--lib", "-j", "1", "--", "--test-threads=1"],
        "Las suites unitarias de descarga y persistencia pasan, incluidas selección, fallback, checkpoint, evento/outbox y procedencia.",
        fixtures=[consumer, download_jobs, planner, db_migrations, db_variants],
        ok=cargo_ok,
        env=temp_env,
    )
    ev.run(
        "P052-OPENAPI-3.10.1",
        "CONTRACT",
        ["cargo", "run", "-p", "larm-daemon", "--", "openapi", "--out", "docs/api/openapi.json", "--check"],
        "OpenAPI refleja el schema vigente de DownloadPlanResponse, incluidos source_candidates y la lista vacía.",
        fixtures=[REPO / "crates/larm-api/src/openapi.rs", openapi, contract_schema],
        env=temp_env,
    )
    ev.run(
        "P052-ARCHITECTURE",
        "CONTRACT",
        ["powershell", "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", "tools/check_architecture.ps1"],
        "El grafo de crates del workspace y Tauri conserva sus límites publicados.",
        fixtures=[planner, download_manifest, REPO / "docs/contracts/crate-graph.json"],
    )
    ev.run(
        "P052-CONSUMER-MIRROR-FAILOVER",
        "INTEGRATION",
        ["cargo", "test", "-p", "larm-download", "--lib", "consumer::tests::consumidor_reintenta_en_mirror_y_guarda_procedencia_tras_verificar", "-j", "1", "--", "--exact", "--test-threads=1"],
        "Tras escribir un bloque parcial y vencer la fuente principal, el consumidor reinicia desde offset cero y completa desde el mirror aprobado con procedencia efectiva.",
        fixtures=[consumer, download_jobs, REPO / "crates/larm-download/src/transfer.rs"],
        ok=cargo_ok,
        env=temp_env,
    )
    ev.run(
        "P052-CONSUMER-RECOVERY",
        "INTEGRATION",
        ["cargo", "test", "-p", "larm-download", "--lib", "consumer::tests::consumidor_recupera_checkpoint_y_reanuda_misma_fuente_tras_reapertura_sqlite", "-j", "1", "--", "--exact", "--test-threads=1"],
        "Tras cuatro bytes sincronizados y una cola de staging no reconocida, el test cierra y reabre SQLite, verifica el checkpoint persistido, trunca la cola y reanuda la misma fuente/revisión desde offset cuatro con el validador conservado.",
        fixtures=[consumer, download_jobs, REPO / "crates/larm-download/src/transfer.rs"],
        ok=cargo_ok,
        env=temp_env,
    )
    ev.run(
        "P052-CONSUMER-PROCESS-CRASH-RECOVERY",
        "INTEGRATION",
        ["cargo", "test", "-p", "larm-download", "--lib", "consumer::tests::consumidor_recupera_descarga_despues_de_caida_del_proceso", "-j", "1", "--", "--exact", "--test-threads=1"],
        "Un proceso hijo termina abruptamente durante la transferencia tras persistir cuatro bytes; al reabrir SQLite y staging, recover_interrupted conserva el checkpoint, descarta la cola no reconocida, reanuda con el mismo ETag y completa el CAS.",
        fixtures=[consumer, download_jobs, REPO / "crates/larm-download/src/transfer.rs", REPO / "crates/larm-storage/src/cas.rs"],
        ok=cargo_ok,
        env=temp_env,
    )
    ev.run(
        "P052-MIRROR-FALLBACK-OUTBOX",
        "INTEGRATION",
        ["cargo", "test", "-p", "larm-db", "--lib", "download_jobs::tests::failover_reinicia_checkpoint_y_guarda_evento_con_outbox_atomicos", "-j", "1", "--", "--exact", "--test-threads=1"],
        "El failover rechaza fuentes no aprobadas y guarda el checkpoint reiniciado, evento y outbox atómicamente.",
        fixtures=[download_jobs],
        ok=cargo_ok,
        env=temp_env,
    )
    ev.run(
        "P052-PRIMARY-WITH-MIRRORS",
        "INTEGRATION",
        ["cargo", "test", "-p", "larm-db", "--lib", "download_jobs::tests::completion_acepta_la_fuente_principal_ademas_de_mirrors_sellados", "-j", "1", "--", "--exact", "--test-threads=1"],
        "La finalización admite la fuente principal aunque el plan contenga mirrors alternativos.",
        fixtures=[download_jobs],
        ok=cargo_ok,
        env=temp_env,
    )
    ev.run(
        "P052-FALLBACK-CLASSIFIER",
        "UNIT",
        ["cargo", "test", "-p", "larm-download", "--lib", "consumer::tests::fallback_solo_admite_fallos_transitorios_de_transporte", "-j", "1", "--", "--exact", "--test-threads=1"],
        "Solo timeout, I/O y HTTP 408/429/5xx pueden activar fallback; autorización, protocolo e integridad son terminales.",
        fixtures=[consumer],
        ok=cargo_ok,
        env=temp_env,
    )

    ev.blocked(
        "P052-REAL-PREFLIGHT-AND-DESTINATIONS",
        "REAL",
        "Preflight del daemon en Windows con ubicaciones locales y volúmenes de prueba",
        "Mostrar exact/estimado/rango/desconocido, temporales y alternativas; no admitir UNKNOWN sin presupuesto duro.",
        "El endpoint combina bundles SQL, sondeo nativo de volúmenes y reservas de P015; aún faltan REAL_PASS de P015/P041 y una ejecución Windows con discos local y de prueba. Las pruebas de código no sustituyen esa observación física.",
    )
    ev.blocked(
        "P052-API-OPTIONAL-SELECTION-AND-CONFIRMATION",
        "REAL",
        "POST /api/v1/download-plans y ejecución con confirmación explícita del operador",
        "La selección opcional llega al plan sellado; crear el plan no inicia transferencia y cada descarga requiere confirmación explícita.",
        "La selección de destino elegible persiste el plan y el evento en una transacción SQLite. `POST /downloads` exige confirmación, revalida, reserva y crea el job durable; el consumidor P051 está conectado, pero la puerta sigue cerrada hasta tener fuentes y dependencias cualificadas.",
    )

    blockers = [
        "P015, P043, P051 y P041 siguen sin REAL_PASS; las puertas del consumidor permanecen BLOCKED_REAL según contrato 00.",
        "El catálogo, la activación y el worker ya integran mirrors exactos; falta seleccionar y probar fuentes cualificadas bajo P043/P045 y completar la aceptación real de transferencias P051.",
        "Falta la prueba Windows con variantes, destinos y reservas físicas y la transferencia end-to-end desde fuentes cualificadas; los tests actuales verifican selección, autorización simulada, fallback, persistencia y recorrido con fixtures, mientras la admisión real permanece cerrada.",
    ]
    ev.finish(
        changed_files=[
            "crates/larm-domain/src/acquisition.rs",
            "crates/larm-domain/src/download.rs",
            "crates/larm-db/src/acquisition.rs",
            "crates/larm-db/tests/acquisition.rs",
            "migrations/0014_model_acquisition_manifest.sql",
            "docs/contracts/REVISION_3_12_0_P044_P052.md",
            "docs/reports/P044_revision_3_12_0_2026-10-27.md",
            "Cargo.lock",
            "crates/larm-download/Cargo.toml",
            "crates/larm-download/src/lib.rs",
            "crates/larm-download/src/preview_service.rs",
            "crates/larm-download/src/planner.rs",
            "crates/larm-download/src/activation_service.rs",
            "crates/larm-download/src/consumer.rs",
            "crates/larm-download/src/transfer.rs",
            "crates/larm-domain/src/download.rs",
            "crates/larm-domain/src/ports.rs",
            "crates/larm-db/src/download_jobs.rs",
            "crates/larm-db/src/download_progress.rs",
            "crates/larm-domain/src/auth.rs",
            "crates/larm-api/src/download_plans.rs",
            "crates/larm-api/src/lib.rs",
            "crates/larm-api/src/openapi.rs",
            "docs/api/openapi.json",
            "crates/larm-api/tests/download_plans.rs",
            "apps/daemon/Cargo.toml",
            "apps/daemon/src/serve.rs",
            "apps/daemon/tests/reference_adapters.rs",
            "apps/desktop/src/models.tsx",
            "apps/desktop/src/models.test.tsx",
            "crates/larm-db/src/model_import.rs",
            "crates/larm-models/src/import_worker.rs",
            "crates/larm-models/tests/import_worker.rs",
            "crates/larm-domain/src/model.rs",
            "crates/larm-db/src/variants.rs",
            "crates/larm-db/src/download_plans.rs",
            "crates/larm-db/src/migrate.rs",
            "crates/larm-models/src/registry.rs",
            "crates/larm-api/tests/models.rs",
            "crates/larm-db/tests/integration.rs",
            "crates/larm-db/tests/adapters.rs",
            "crates/larm-db/tests/sources.rs",
            "migrations/0006_p052_artifact_size_estimate.sql",
            "docs/contracts/00_PACKAGE.md",
            "docs/contracts/02_STORAGE_DOWNLOAD.md",
            "docs/contracts/07_API_EVENTS_MCP.md",
            "docs/contracts/REVISION_3_1_0_P052.md",
            "docs/contracts/REVISION_3_10_0_P052.md",
            "docs/contracts/REVISION_3_10_1_P052.md",
            "docs/contracts/api-operations.json",
            "docs/contracts/adapter-methods.json",
            "docs/contracts/persistence.sql",
            "docs/contracts/schemas/domain.schema.json",
            "docs/contracts/schemas/larm.schema.json",
            "docs/contracts/migrations/0006_p052_artifact_size_estimate.sql",
            "docs/adapters/method-schemas/source.download_plan.request.schema.json",
            "docs/adapters/method-schemas/source.get_manifest.response.schema.json",
            "docs/adapters/method-schemas/index.json",
            "crates/larm-adapter-sdk/src/contracts/generated.rs",
            "crates/larm-adapter-sdk/tests/contracts.rs",
            "tools/test_python_contracts.py",
            "tools/p026_schema_corpus.py",
            "tools/p026_capability_schemas.py",
            "tools/p026_reference_capabilities.py",
            "docs/adapters/capability-schemas/index.json",
            "crates/larm-adapter-sdk/src/capabilities/builtin.rs",
            "adapters/source/reference/capabilities.json",
            "adapters/runtime/reference/capabilities.json",
            "crates/larm-adapter-host/src/package.rs",
            "crates/larm-adapter-host/src/updates.rs",
            "tests/fixtures/adapters/method-schema-corpus.json",
            "docs/spec/manifest.json",
            "docs/spec/PACKAGE_COPY_HASHES.json",
            "tools/package_copy.py",
            "tools/p052_contract_revision.py",
            "tools/p052_evidence.py",
            "docs/implementation/STATE.md",
            "docs/reports/P052_planificador-de-tamano-y-descarga-multi-fuente.md",
            "tools/p052_evidence.py",
        ],
        blockers=blockers,
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
