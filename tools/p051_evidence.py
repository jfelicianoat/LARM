#!/usr/bin/env python3
"""Evidencia focal de P051; las aceptaciones no ejecutadas conservan BLOCKED."""

from pathlib import Path

from evidence import Evidence, REPO, cargo_ok, exit_zero


def main() -> int:
    ev = Evidence("P051")
    domain = REPO / "crates/larm-domain/src/download.rs"
    sdk = REPO / "crates/larm-adapter-sdk/src/source/wire.rs"
    policy = REPO / "crates/larm-download/src/policy.rs"
    activation = REPO / "crates/larm-db/src/download_plans.rs"
    activation_service = REPO / "crates/larm-download/src/activation_service.rs"
    api = REPO / "crates/larm-api/src/download_plans.rs"
    api_tests = REPO / "crates/larm-api/tests/download_plans.rs"
    source_wire = REPO / "crates/larm-adapter-sdk/src/source/wire.rs"
    source_tests = REPO / "crates/larm-adapter-sdk/tests/source_protocol.rs"
    test_temp = REPO / "target/test-tmp"
    test_temp.mkdir(parents=True, exist_ok=True)
    temp_env = {"TEMP": str(test_temp), "TMP": str(test_temp)}

    ev.run(
        "P051-CONTRACT-PACKAGE",
        "CONTRACT",
        ["python", "tools/package_copy.py", "check"],
        "Las copias locales coinciden con el paquete contractual vigente; P052 3.1.0 define IDs opcionales e intervalos por artefacto.",
        fixtures=[
            REPO / "docs/contracts/00_PACKAGE.md",
            REPO / "docs/contracts/REVISION_3_1_0_P052.md",
            REPO / "docs/contracts/02_STORAGE_DOWNLOAD.md",
            REPO / "docs/contracts/schemas/larm.schema.json",
            REPO / "docs/reports/P051_alineacion_paquete_contractual.md",
        ],
    )
    ev.run(
        "P051-FMT",
        "UNIT",
        ["cargo", "fmt", "--all", "--check"],
        "Todo el Rust del workspace está formateado.",
        fixtures=[domain, sdk, policy, activation_service, api, source_wire],
    )
    ev.run(
        "P051-DOMAIN-STATE",
        "CONTRACT",
        [
            "cargo",
            "test",
            "-p",
            "larm-domain",
            "--lib",
            "download::tests",
            "-j",
            "1",
            "--",
            "--test-threads=1",
        ],
        "Prioridades, DTO estricto, transiciones y reintento con preflight coinciden con el contrato.",
        fixtures=[domain],
        ok=cargo_ok,
    )
    ev.run(
        "P051-TRANSFER-POLICY",
        "CONTRACT",
        [
            "cargo",
            "test",
            "-p",
            "larm-download",
            "--lib",
            "policy::tests",
            "-j",
            "1",
            "--",
            "--test-threads=1",
        ],
        "La cola, Range/ETag, 200/206/416, retry, checksum y límite agregado obedecen sus reglas.",
        fixtures=[policy],
        ok=cargo_ok,
    )
    ev.run(
        "P051-DOWNLOAD-LIB-ALL",
        "CONTRACT",
        [
            "cargo",
            "test",
            "-p",
            "larm-download",
            "--lib",
            "-j",
            "1",
            "--",
            "--test-threads=1",
        ],
        "El conjunto completo del gestor valida planner, política, preflight y activación.",
        fixtures=[
            REPO / "crates/larm-download/src/activation_service.rs",
            REPO / "crates/larm-download/src/planner.rs",
            policy,
            REPO / "crates/larm-download/src/preview_service.rs",
        ],
        ok=cargo_ok,
        env=temp_env,
    )
    ev.run(
        "P051-TRANSFER-STAGING",
        "INTEGRATION",
        ["cargo", "test", "-p", "larm-download", "--lib", "transfer::tests", "-j", "1", "--", "--test-threads=1"],
        "Staging verifica bytes/SHA, pausa/cancelación, reanudación SQLite, ETag, límites, checkpoint y StorageFull. Un EOF vacío en un checkpoint completo sigue verificando tamaño y hash del fichero y rechaza el parcial alterado.",
        fixtures=[REPO / "crates/larm-download/src/transfer.rs"],
        ok=cargo_ok,
        env=temp_env,
    )
    ev.run(
        "P051-CONSUMER-END-TO-END",
        "INTEGRATION",
        ["cargo", "test", "-p", "larm-download", "--lib", "consumer::tests::consumidor_reintenta_en_mirror_y_guarda_procedencia_tras_verificar", "-j", "1", "--", "--exact", "--test-threads=1"],
        "El consumidor en SQLite prueba el failover de mirror desde offset cero, cierra ambas sesiones y verifica staging, promoción CAS, procedencia, catálogo y liberación de reserva.",
        fixtures=[REPO / "crates/larm-download/src/consumer.rs", REPO / "crates/larm-download/src/transfer.rs", REPO / "crates/larm-db/src/download_jobs.rs"],
        ok=cargo_ok,
        env=temp_env,
    )
    ev.run(
        "P051-STORAGE-FULL-ERROR-MAPPING",
        "UNIT",
        ["cargo", "test", "-p", "larm-download", "--lib", "consumer::tests::disco_lleno_se_informa_como_espacio_insuficiente", "-j", "1", "--", "--exact", "--test-threads=1"],
        "El consumidor convierte StorageFull a STORAGE_INSUFFICIENT con un mensaje seguro y detalle técnico solo para logs.",
        fixtures=[REPO / "crates/larm-download/src/consumer.rs"],
        ok=cargo_ok,
    )
    ev.run(
        "P051-STORAGE-FULL-CONSUMER",
        "INTEGRATION",
        ["cargo", "test", "-p", "larm-download", "--lib", "consumer::tests::consumidor_persiste_storage_full_y_libera_reserva_sin_fallback", "-j", "1", "--", "--exact", "--test-threads=1"],
        "El fallo de staging recorre el consumidor y SQLite: el job guarda STORAGE_INSUFFICIENT, cierra la sesión, conserva el checkpoint, libera la reserva y no prueba mirrors por un error local.",
        fixtures=[REPO / "crates/larm-download/src/consumer.rs", REPO / "crates/larm-download/src/transfer.rs", REPO / "crates/larm-db/src/download_jobs.rs"],
        ok=cargo_ok,
        env=temp_env,
    )
    ev.run(
        "P051-CONSUMER-PROCESS-CRASH-RECOVERY",
        "INTEGRATION",
        ["cargo", "test", "-p", "larm-download", "--lib", "consumer::tests::consumidor_recupera_descarga_despues_de_caida_del_proceso", "-j", "1", "--", "--exact", "--test-threads=1"],
        "Un proceso hijo termina abruptamente tras un checkpoint durable; el consumer reabre SQLite/staging, descarta la cola no reconocida, reanuda en offset cuatro con el mismo ETag y completa el CAS.",
        fixtures=[REPO / "crates/larm-download/src/consumer.rs", REPO / "crates/larm-download/src/transfer.rs", REPO / "crates/larm-db/src/download_jobs.rs", REPO / "crates/larm-storage/src/cas.rs"],
        ok=cargo_ok,
        env=temp_env,
    )
    ev.run(
        "P051-DAEMON-PROCESS-RESTART-RECOVERY",
        "INTEGRATION",
        ["cargo", "test", "-p", "larm-daemon", "--test", "lifecycle", "proceso_real_reinicia_api_y_recupera_descarga_running_con_checkpoint", "-j", "1", "--", "--exact", "--test-threads=1"],
        "El daemon real arranca, sirve health/live y se reinicia con un job RUNNING en SQLite; verifica recuperación durable, checkpoint conservado, evento único y reclamación segura sin un plan sellado.",
        fixtures=[REPO / "apps/daemon/tests/lifecycle.rs", REPO / "apps/daemon/tests/support/daemon.rs", REPO / "apps/daemon/src/serve.rs", REPO / "crates/larm-db/src/download_jobs.rs"],
        ok=cargo_ok,
        env=temp_env,
    )
    ev.run(
        "P051-NATIVE-WORKER-FIXTURE-BUILD",
        "UNIT",
        ["cargo", "build", "-p", "larm-adapter-host", "--example", "fake_adapter", "-j1"],
        "El adaptador de pruebas incluye el caso de descendiente sin canales heredados; no depende de un ejecutable anterior en target.",
        fixtures=[REPO / "crates/larm-adapter-host/examples/fake_adapter.rs"],
        ok=exit_zero,
    )
    ev.run(
        "P051-NATIVE-WORKER-TREE-CLOSE",
        "INTEGRATION",
        ["cargo", "test", "-p", "larm-adapter-host", "--test", "worker", "-j1", "--", "--test-threads=1"],
        "El cierre confirma la salida del proceso y todos los miembros del Job Object, incluso descendientes sin canales heredados; un fallo de espera o drenaje no acredita cierre.",
        fixtures=[REPO / "crates/larm-adapter-host/src/worker.rs", REPO / "crates/larm-adapter-host/tests/worker.rs", REPO / "crates/larm-adapter-host/examples/fake_adapter.rs"],
        ok=cargo_ok,
        env=temp_env,
    )
    ev.run(
        "P051-HTTP-IF-RANGE-AND-REVISION",
        "INTEGRATION",
        ["cargo", "test", "-p", "larm-source-p048", "--lib", "-j1", "--", "--test-threads=1"],
        "El adaptador envía If-Range, distingue 200/206 y sólo convierte 416 en EOF candidato con total válido, offset coincidente y hash fijado; rechaza metadatos ausentes, malformados o desbordados.",
        fixtures=[REPO / "adapters/source/p048/src/lib.rs", REPO / "crates/larm-download/src/transfer.rs"],
        ok=cargo_ok,
        env=temp_env,
    )
    ev.run(
        "P051-DAEMON-VALID-TRANSFER-RESTART",
        "INTEGRATION",
        ["cargo", "test", "-p", "larm-daemon", "--test", "reference_adapters", "source_suite_nativa_completa_descarga_recupera_y_confirma_core_sqlite", "-j", "1", "--", "--exact", "--test-threads=1"],
        "La medición de admisión no ejecuta RPC y preserva la exclusión de sesiones. La API exige cualificación y confirmación, y acepta sin duplicados bajo 30 s. ETag cambiado fuerza checkpoint cero; hash corrupto falla sin publicación/CAS y con journal/reserva cerrados. Conserva otra descarga pausada, cancela trabajos activos/en cola y recupera Range/If-Range, SHA, CAS, procedencia y journal/outbox tras caer; revoca el principal efímero.",
        fixtures=[
            REPO / "apps/daemon/tests/reference_adapters.rs",
            REPO / "apps/daemon/tests/support/download_restart.rs",
            REPO / "apps/daemon/tests/support/download_integrity.rs",
            REPO / "apps/daemon/tests/support/download_ranges.rs",
            REPO / "apps/daemon/tests/support/daemon.rs",
            REPO / "adapters/source/reference/adapter.py",
            REPO / "adapters/source/reference/transfer.py",
            REPO / "tools/p025_host_fixture.py",
            REPO / "adapters/source/p048/src/lib.rs",
            REPO / "apps/daemon/src/download_execution.rs",
            REPO / "apps/daemon/src/serve.rs",
            REPO / "crates/larm-download/src/activation_service.rs",
            REPO / "crates/larm-download/src/consumer.rs",
            REPO / "crates/larm-download/src/transfer.rs",
            REPO / "crates/larm-db/src/download_jobs.rs",
            REPO / "crates/larm-db/src/download_controls.rs",
            REPO / "crates/larm-api/src/downloads.rs",
            REPO / "crates/larm-adapter-host/src/conformance.rs",
            REPO / "crates/larm-adapter-host/src/conformance/qualification.rs",
            REPO / "crates/larm-adapter-host/src/worker.rs",
        ],
        ok=cargo_ok,
        env=temp_env,
    )
    ev.run(
        "P051-DAEMON-HTTP416",
        "INTEGRATION",
        ["cargo", "test", "-p", "larm-daemon", "--test", "reference_adapters", "source_suite_nativa_http416_verifica_parciales_y_reinicia_rango", "-j1", "--", "--exact", "--test-threads=1"],
        "Un daemon y una cualificación independientes comprueban 416: completa el parcial íntegro tras verificar tamaño/hash, rechaza el completo alterado y reinicia un rango prematuro. Conserva otra descarga pausada, limpia ésta al terminar, deja cero reservas y revoca el principal.",
        fixtures=[REPO / "apps/daemon/tests/reference_adapters.rs", REPO / "apps/daemon/tests/support/download_restart.rs", REPO / "apps/daemon/tests/support/download_integrity.rs", REPO / "apps/daemon/tests/support/download_ranges.rs", REPO / "adapters/source/reference/adapter.py", REPO / "adapters/source/reference/transfer.py", REPO / "tools/p025_host_fixture.py"],
        ok=cargo_ok,
        env=temp_env,
    )
    ev.run(
        "P051-H08-SETTINGS-CODE",
        "INTEGRATION",
        ["cargo", "test", "-p", "larm-api", "-p", "larm-db", "--lib", "settings::tests", "-j", "1", "--", "--test-threads=1"],
        "Settings GET/PATCH publican ETag, exigen If-Match, validan el límite, actualizan el limitador activo y guardan Settings con CAS y outbox atómico.",
        fixtures=[
            REPO / "crates/larm-api/src/settings.rs",
            REPO / "crates/larm-db/src/settings.rs",
            REPO / "crates/larm-domain/src/ports.rs",
        ],
        ok=cargo_ok,
    )
    ev.run(
        "P051-DAEMON-DOWNLOAD-GUARD",
        "UNIT",
        ["cargo", "test", "-p", "larm-daemon", "download_execution::tests", "-j", "1", "--", "--test-threads=1"],
        "La composición daemon limita los orígenes al manifiesto y falla cerrada para auth, workers sin chunks y bindings no ejecutables.",
        fixtures=[REPO / "apps/daemon/src/download_execution.rs"],
        ok=cargo_ok,
    )
    ev.run(
        "P051-DAEMON-DOWNLOAD-SUPERVISOR",
        "INTEGRATION",
        ["cargo", "test", "-p", "larm-daemon", "--bin", "larm-daemon", "serve::download_consumer_supervision_tests::supervisor_continua_despues_de_fallo_de_espacio_y_cierra_con_shutdown", "-j", "1", "--", "--exact", "--test-threads=1"],
        "El supervisor registra un job fallido por espacio, sigue atendiendo la cola y termina ordenadamente al recibir shutdown.",
        fixtures=[REPO / "apps/daemon/src/serve.rs", REPO / "crates/larm-download/src/consumer.rs"],
        ok=cargo_ok,
    )
    ev.run(
        "P051-SOURCE-PRIORITY-WIRE",
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
        "DownloadPriority conserva su serialización y no convierte tamaños desconocidos en cero.",
        fixtures=[sdk, policy],
        ok=cargo_ok,
    )
    ev.run(
        "P051-DURABLE-ACTIVATION",
        "CONTRACT",
        [
            "cargo",
            "test",
            "-p",
            "larm-db",
            "--lib",
            "download_plans::tests",
            "-j",
            "1",
            "--",
            "--test-threads=1",
        ],
        "La confirmación explícita crea job, reserva, journal, outbox e idempotencia en una única transacción SQLite.",
        fixtures=[activation, domain],
        ok=cargo_ok,
    )
    ev.run(
        "P051-DURABLE-TERMINAL-RESULT",
        "CONTRACT",
        [
            "cargo",
            "test",
            "-p",
            "larm-db",
            "--lib",
            "download_jobs::tests",
            "-j",
            "1",
            "--",
            "--test-threads=1",
        ],
        "Los estados VERIFYING/FAILED y la publicación final de catálogo, procedencia, journal, outbox y reserva se confirman o revierten atómicamente.",
        fixtures=[
            REPO / "crates/larm-db/src/download_jobs.rs",
            REPO / "crates/larm-db/src/artifacts.rs",
            REPO / "crates/larm-db/src/models.rs",
            REPO / "crates/larm-db/src/download_plans.rs",
        ],
        ok=cargo_ok,
    )
    ev.run(
        "P051-PREVIEW-HASH",
        "CONTRACT",
        [
            "cargo",
            "test",
            "-p",
            "larm-download",
            "--lib",
            "preview_service::tests",
            "-j",
            "1",
            "--",
            "--test-threads=1",
        ],
        "El preflight conserva tamaños, destinos y el hash sellado después de compartir su cálculo con la validación de ejecución.",
        fixtures=[REPO / "crates/larm-download/src/preview_service.rs"],
        ok=cargo_ok,
    )
    ev.run(
        "P051-ACTIVATION-SEAL",
        "CONTRACT",
        [
            "cargo",
            "test",
            "-p",
            "larm-download",
            "--lib",
            "activation_service::tests",
            "-j",
            "1",
            "--",
            "--test-threads=1",
        ],
        "El servicio exige confirmación expresa y el hash exacto, y falla cerrado si falta un worker cualificado.",
        fixtures=[activation_service, REPO / "crates/larm-download/src/preview_service.rs"],
        ok=cargo_ok,
    )
    ev.run(
        "P051-API-CONFIRMATION",
        "CONTRACT",
        [
            "cargo",
            "test",
            "-p",
            "larm-api",
            "--test",
            "download_plans",
            "-j",
            "1",
            "--",
            "--test-threads=1",
        ],
        "La API rechaza confirmación ausente y campos ajenos, y reproduce el receipt de forma idempotente.",
        fixtures=[api, api_tests, domain],
        ok=cargo_ok,
    )
    ev.run(
        "P051-PRIORITY-DB",
        "CONTRACT",
        ["cargo", "test", "-p", "larm-db", "--lib", "download_jobs::tests", "-j", "1", "--", "--test-threads=1"],
        "La prioridad usa CAS; los controles son idempotentes y limitados al actor y al estado. Journal, evento y recibo se confirman juntos; la recuperación conserva el checkpoint y completa cancelaciones sin reabrir IO.",
        fixtures=[REPO / "crates/larm-db/src/download_jobs.rs", REPO / "crates/larm-db/src/download_controls.rs", REPO / "crates/larm-domain/src/ports.rs"],
        ok=cargo_ok,
    )
    ev.run(
        "P051-PRIORITY-API",
        "CONTRACT",
        ["cargo", "test", "-p", "larm-api", "--test", "downloads", "-j", "1", "--", "--test-threads=1"],
        "La prioridad exige If-Match; pause/resume/cancel exigen jobs:write, UUIDv7, Stop cerrado e Idempotency-Key y devuelven JobReceipt 202.",
        fixtures=[REPO / "crates/larm-api/src/downloads.rs", REPO / "crates/larm-api/tests/downloads.rs"],
        ok=cargo_ok,
    )
    ev.run(
        "P051-PRIORITY-DESKTOP",
        "UNIT",
        ["node", "../../node_modules/vitest/vitest.mjs", "run", "--pool=threads", "--maxWorkers=1"],
        "La interfaz confirma prioridad con la row_version observada, permite pausa/reanudación, pide confirmar la cancelación y recupera una respuesta perdida con la misma clave y orden.",
        fixtures=[REPO / "apps/desktop/src/downloads.tsx", REPO / "apps/desktop/src/downloads.test.tsx", REPO / "apps/desktop/src/download-controls.tsx", REPO / "apps/desktop/src/download-controls.test.tsx"],
        ok=exit_zero,
        cwd=REPO / "apps/desktop",
    )
    ev.run(
        "P051-DESKTOP-LINT",
        "UNIT",
        ["node", "../../node_modules/eslint/bin/eslint.js", "."],
        "El frontend mantiene las reglas de lint.",
        fixtures=[REPO / "apps/desktop/src/downloads.tsx", REPO / "apps/desktop/src/downloads.test.tsx"],
        ok=exit_zero,
        cwd=REPO / "apps/desktop",
    )
    ev.run(
        "P051-DESKTOP-TYPECHECK",
        "UNIT",
        ["node", "../../node_modules/typescript/bin/tsc", "-b"],
        "Los tipos del control de prioridad coinciden con el cliente API.",
        fixtures=[REPO / "apps/desktop/src/downloads.tsx"],
        ok=exit_zero,
        cwd=REPO / "apps/desktop",
    )
    ev.run(
        "P051-DESKTOP-BUILD",
        "UNIT",
        ["node", "../../node_modules/vite/bin/vite.js", "build"],
        "El bundle de escritorio se compila con el control de prioridad.",
        fixtures=[REPO / "apps/desktop/src/downloads.tsx", REPO / "apps/desktop/src/styles.css"],
        ok=exit_zero,
        cwd=REPO / "apps/desktop",
    )
    ev.run(
        "P051-OPENAPI",
        "CONTRACT",
        ["cargo", "run", "-p", "larm-daemon", "--", "openapi", "--out", "docs/api/openapi.json", "--check"],
        "OpenAPI generado conserva la operación de prioridad declarada por el contrato.",
        fixtures=[REPO / "crates/larm-api/src/downloads.rs", REPO / "docs/api/openapi.json"],
        ok=exit_zero,
    )
    ev.run(
        "P051-SOURCE-ARTIFACT-ESTIMATE",
        "CONTRACT",
        [
            "cargo",
            "test",
            "-p",
            "larm-adapter-sdk",
            "--test",
            "source_protocol",
            "-j",
            "1",
            "--",
            "--test-threads=1",
        ],
        "La suite completa de protocolo de fuentes conserva intervalos en manifiestos, rechaza contradicciones exactas y mantiene strictness de schemas.",
        fixtures=[source_wire, source_tests, REPO / "docs/contracts/schemas/larm.schema.json"],
        ok=cargo_ok,
    )
    ev.run(
        "P051-CLIPPY-FOCAL",
        "CONTRACT",
        [
            "cargo",
            "clippy",
            "-p",
            "larm-domain",
            "-p",
            "larm-adapter-sdk",
            "-p",
            "larm-adapter-host",
            "-p",
            "larm-download",
            "-p",
            "larm-db",
            "-p",
            "larm-api",
            "-p",
            "larm-daemon",
            "--all-targets",
            "-j",
            "1",
            "--",
            "-D",
            "warnings",
        ],
        "Los crates afectados y todos sus targets pasan Clippy sin warnings.",
        fixtures=[domain, sdk, policy, activation],
    )

    ev.run(
        "P051-H08-CORE-TCP",
        "REAL",
        ["cargo", "test", "-p", "larm-download", "--lib", "consumer::tests::h08::", "-j1", "--", "--test-threads=1", "--nocapture"],
        "Dos consumidores del Core transfieren bytes TCP propios con contraste sin límite; presupuesto agregado y ráfaga por prefijos/ventanas, cambio persistido, pausa y reapertura SQLite hasta SHA/CAS/procedencia. Fuente/planes de test, sin cualificación P022/P048, API Settings del daemon, kill de proceso ni GUI. Ocho pares adicionales publican contenido idéntico.",
        fixtures=[REPO / "crates/larm-download/src/consumer.rs", REPO / "crates/larm-download/src/consumer_h08.rs", policy, REPO / "tools/p051_h08_http_fixture.py", REPO / "crates/larm-storage/src/cas.rs"],
        ok=cargo_ok,
        timeout=300,
    )
    ev.run(
        "P051-CAS-CONCURRENT-PUBLICATION",
        "FAULT",
        ["cargo", "test", "-p", "larm-storage", "--lib", "cas::tests::", "-j1", "--", "--test-threads=1"],
        "En Windows, dos promociones que observaron destino ausente reutilizan el blob concurrente tras verificar hash/tamaño; una colisión corrupta falla y conserva el parcial y recibo.",
        fixtures=[REPO / "crates/larm-storage/src/cas.rs"],
        ok=cargo_ok,
    )
    ev.blocked(
        "P051-REAL-TRANSFER-AND-FAULTS",
        "REAL",
        "Daemon + DownloadManager + servidor HTTP local controlado en Windows",
        "La ejecución Windows del daemon/worker con servidor local controlado cubre pausa/reanudación, ETag cambiado, hash incorrecto, cancelación, disco lleno y reinicio completo tras caída.",
        "La prueba controlada cualifica por HTTP y verifica pausa/reanudación, cancelación activa con salida de su árbol nativo y conservación de otra descarga pausada, cancelación en cola con el único worker ocupado y recuperación Range/If-Range tras una caída, con SHA-256, CAS, procedencia, journal/outbox y reservas. StorageFull está probado en consumer/SQLite con fixture. El ensayo final observa HTTP 200 por ETag cambiado, checkpoint cero y SHA correcto, y rechaza un cuerpo corrupto sin publicación ni CAS con journal FAILED y reserva liberada. Una fase independiente verifica 416 con parcial completo íntegro, completo alterado y rango prematuro. Faltan StorageFull y los restantes casos HTTP en el recorrido completo, además de upstream real; no se concede REAL_PASS a esas aceptaciones.",
    )
    ev.blocked(
        "P051-REAL-H08-BANDWIDTH",
        "REAL",
        "Cambiar Settings por API y medir dos transferencias concurrentes contra servidor local",
        "El caudal agregado cambia mediante PATCH /settings, afecta el siguiente bloque, respeta la ráfaga de un segundo y persiste tras reiniciar configuración.",
        "El Core mide tráfico TCP real de dos consumidores con contraste sin límite, presupuesto agregado/ráfaga, cambio persistido, pausa y reapertura SQLite. La prueba usa fuente/planes de test, sin host cualificado. Settings API y reinicio del daemon se verifican por separado con P048 original. Falta reunir dos transferencias cualificadas, PATCH y kill/reinicio en una única aceptación del daemon.",
    )

    blockers = [
        "Faltan REAL_PASS de P015, P043, P019 y P029, dependencias obligatorias de P051.",
        "El consumidor supervisado y la ruta de bloques hacia staging/CAS están conectados; auth/licencia P045, mirrors P043 y cualificación real siguen cerrando la admisión.",
        "El daemon demuestra pausa/reanudación, cancelación activa con cierre del árbol y aislamiento de otra descarga pausada, cancelación en cola con un slot ocupado y recuperación tras caída con paquete controlado cualificado. También prueba ETag cambiado, hash corrupto y tres escenarios 416 en fases independientes. Faltan StorageFull y la matriz HTTP restante integrados, upstream real y la aceptación H08 conjunta del daemon; el Core ya mide caudal TCP agregado con fuente de test.",
    ]
    ev.finish(
        changed_files=[
            "crates/larm-adapter-host/src/worker.rs",
            "crates/larm-adapter-host/tests/worker.rs",
            "crates/larm-adapter-host/examples/fake_adapter.rs",
            "crates/larm-adapter-host/examples/fake_source.rs",
            "crates/larm-adapter-host/src/conformance.rs",
            "crates/larm-adapter-host/src/conformance/qualification.rs",
            "crates/larm-adapter-sdk/tests/source_protocol.rs",
            "crates/larm-adapter-sdk/src/source/wire.rs",
            "crates/larm-domain/src/download.rs",
            "crates/larm-domain/src/lib.rs",
            "crates/larm-domain/src/ports.rs",
            "crates/larm-core/src/config/mod.rs",
            "crates/larm-download/Cargo.toml",
            "crates/larm-download/src/lib.rs",
            "crates/larm-download/src/policy.rs",
            "crates/larm-download/src/preview_service.rs",
            "crates/larm-download/src/transfer.rs",
            "adapters/source/p048/src/lib.rs",
            "tools/p025_host_fixture.py",
            "crates/larm-download/src/consumer.rs",
            "crates/larm-download/src/consumer_h08.rs",
            "crates/larm-storage/src/cas.rs",
            "tools/p051_h08_http_fixture.py",
            "docs/reports/P051_h08_core_cas_2026-10-27.md",
            "crates/larm-db/src/download_controls.rs",
            "crates/larm-download/src/activation_service.rs",
            "crates/larm-api/src/download_plans.rs",
            "crates/larm-api/tests/download_plans.rs",
            "crates/larm-api/src/downloads.rs",
            "crates/larm-api/src/settings.rs",
            "crates/larm-api/src/openapi.rs",
            "crates/larm-api/tests/downloads.rs",
            "apps/desktop/src/downloads.tsx",
            "apps/desktop/src/downloads.test.tsx",
            "apps/desktop/src/download-controls.tsx",
            "apps/desktop/src/download-controls.test.tsx",
            "apps/desktop/src/models.tsx",
            "apps/desktop/src/models.test.tsx",
            "apps/desktop/src/api/settings.ts",
            "apps/desktop/src/settings.tsx",
            "apps/desktop/src/settings.test.tsx",
            "apps/desktop/src/styles.css",
            "apps/daemon/src/download_execution.rs",
            "apps/daemon/tests/lifecycle.rs",
            "apps/daemon/tests/reference_adapters.rs",
            "apps/daemon/tests/support/daemon.rs",
            "apps/daemon/tests/support/download_restart.rs",
            "apps/daemon/tests/support/download_integrity.rs",
            "apps/daemon/tests/support/download_ranges.rs",
            "adapters/source/reference/adapter.py",
            "adapters/source/reference/transfer.py",
            "apps/daemon/src/main.rs",
            "apps/daemon/src/serve.rs",
            "docs/api/openapi.json",
            "docs/reports/P051_alineacion_paquete_contractual.md",
            "crates/larm-db/src/download_plans.rs",
            "crates/larm-db/src/download_jobs.rs",
            "crates/larm-db/src/settings.rs",
            "crates/larm-db/src/lib.rs",
            "crates/larm-db/src/artifacts.rs",
            "crates/larm-db/src/models.rs",
            "docs/implementation/STATE.md",
            "docs/reports/P051_download-manager-cola-pausa-reanudacion-y-checksum.md",
            "docs/reports/P051_controls_2026-10-26.md",
            "docs/reports/P051_active_cancel_2026-10-26.md",
            "docs/reports/P051_integrity_2026-10-26.md",
            "docs/reports/P051_http416_2026-10-26.md",
            "docs/reports/P003_configuracion-errores-y-logging-estructurado.md",
            "docs/reports/P005_api-administrativa-axum-v1-y-openapi.md",
            "tools/p051_evidence.py",
        ],
        blockers=blockers,
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
