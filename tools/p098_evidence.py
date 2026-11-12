#!/usr/bin/env python3
"""Pruebas y evidencia de preview e importación local P098."""

from evidence import Evidence, cargo_ok, exit_zero


def main() -> int:
    ev = Evidence("P098")
    ev.run(
        "P098-UNIT-storage-limit",
        "UNIT",
        ["cargo", "test", "-p", "larm-storage", "--lib", "import::tests::limite_incluye_ficheros_encontrados_y_elementos_omitidos", "-j1", "--", "--test-threads=1"],
        "Recorrido y preview pasan, incluido el límite agregado de 100.000 entradas.",
        ok=cargo_ok,
    )
    ev.run(
        "P098-UNIT-models-preview",
        "UNIT",
        ["cargo", "test", "-p", "larm-models", "-j1", "--", "--test-threads=1"],
        "El preview detecta variantes y tamaños, incluidos GGUF, CTranslate2, NCNN y MNN, mantiene rutas relativas y no modifica archivos.",
        ok=cargo_ok,
    )
    ev.run(
        "P098-INTEGRATION-api-preview",
        "INTEGRATION",
        ["cargo", "test", "-p", "larm-api", "--test", "import_preview", "-j1", "--", "--test-threads=1"],
        "La ruta exige models:write, valida el cuerpo estricto y no requiere clave de idempotencia.",
        ok=cargo_ok,
    )
    ev.run(
        "P098-INTEGRATION-api-import",
        "INTEGRATION",
        ["cargo", "test", "-p", "larm-api", "--test", "model_import", "-j1", "--", "--test-threads=1"],
        "POST /api/v1/models/import exige models:write e Idempotency-Key y rechaza requests incompletas o propiedades desconocidas.",
        ok=cargo_ok,
    )
    ev.run(
        "P098-INTEGRATION-sqlite-jobs",
        "INTEGRATION",
        ["cargo", "test", "-p", "larm-db", "--lib", "model_import::tests", "-j1", "--", "--test-threads=1"],
        "La cola confirma journal, idempotencia y outbox atómicos, cierra jobs RUNNING, crea/reutiliza fuentes automáticas concurrentemente y falla cerrado si se revoca el adaptador local.",
        ok=cargo_ok,
    )
    ev.run(
        "P098-INTEGRATION-migration-source",
        "INTEGRATION",
        ["cargo", "test", "-p", "larm-db", "--test", "adapter_selection", "-j1", "--", "--test-threads=1"],
        "La migración 0007 instala la fuente local interna sin cambiar la selección de adapters.",
        ok=cargo_ok,
    )
    ev.run(
        "P098-INTEGRATION-migration-compatibility",
        "INTEGRATION",
        ["cargo", "test", "-p", "larm-db", "--test", "integration", "migracion_desde_vacia_crea_exactamente_las_tablas_del_ddl", "-j1", "--", "--test-threads=1"],
        "Una instalación desde una base vacía aplica exactamente las siete migraciones esperadas.",
        ok=cargo_ok,
    )
    ev.run(
        "P098-INTEGRATION-import-worker",
        "INTEGRATION",
        ["cargo", "test", "-p", "larm-models", "--test", "import_worker", "-j1", "--", "--test-threads=1"],
        "El worker crea/reutiliza la fuente de la ruta local canónica y la vincula a las distribuciones GGUF, CTranslate2, NCNN y MNN.",
        ok=cargo_ok,
    )
    ev.run(
        "P098-REGRESSION-db-model-crates",
        "INTEGRATION",
        [
            "cargo", "test", "-p", "larm-db", "-p", "larm-models", "-j1", "--",
            "--test-threads=1",
            "--skip", "backup_y_restore_verificado",
            "--skip", "h01_conserva_los_cinco_canales_en_backup_y_restore",
            "--skip", "restore_en_una_instalacion_nueva_sin_carpeta",
        ],
        "Todas las pruebas de larm-db y larm-models pasan; se omiten solo tres pruebas de backup/restore bloqueadas por permisos de publicación en este Windows.",
        ok=cargo_ok,
    )
    ev.run(
        "P098-INTEGRATION-registry-rebuild",
        "INTEGRATION",
        ["cargo", "test", "-p", "larm-models", "--test", "import", "reconstruye_registro_en_una_base_nueva_desde_manifiestos_y_blobs", "-j1", "--", "--test-threads=1"],
        "Copia los bundles a CAS y reconstruye variantes y ubicaciones en una base SQLite nueva; conserva licencias desconocidas.",
        ok=cargo_ok,
    )
    ev.run(
        "P098-INTEGRATION-import-format-license",
        "INTEGRATION",
        ["cargo", "test", "-p", "larm-models", "--test", "import", "-j1", "--", "--test-threads=1"],
        "El filtro de formato limita las variantes importadas y conserva license_id como declaración, sin elevarla a verificación.",
        ok=cargo_ok,
    )
    ev.run(
        "P098-INTEGRATION-daemon-preview",
        "INTEGRATION",
        ["cargo", "test", "-p", "larm-daemon", "model_import_preview::tests", "-j1", "--", "--test-threads=1"],
        "El daemon explora archivos reales de una carpeta temporal sin modificarlos ni revelar rutas absolutas.",
        ok=cargo_ok,
    )
    ev.run(
        "P098-CONTRACT-openapi",
        "CONTRACT",
        ["cargo", "run", "-p", "larm-daemon", "--", "openapi", "--out", "docs/api/openapi.json", "--check"],
        "OpenAPI coincide con el contrato e identifica el preview y la importación durable como implementados.",
        ok=exit_zero,
    )
    ev.run(
        "P098-UI-tests",
        "INTEGRATION",
        ["npm.cmd", "test"],
        "Las pruebas de escritorio confirman preview, filtro de formato, recuento seleccionado y confirmación explícita.",
        ok=exit_zero,
    )
    ev.run("P098-UI-lint", "CONTRACT", ["npm.cmd", "run", "lint"], "El lint del frontend no encuentra errores.", ok=exit_zero)
    ev.run("P098-UI-typecheck", "CONTRACT", ["npm.cmd", "run", "typecheck"], "El frontend pasa la comprobación de tipos.", ok=exit_zero)
    ev.run("P098-UI-build", "CONTRACT", ["npm.cmd", "run", "build"], "El escritorio compila correctamente.", ok=exit_zero)
    ev.run("P098-FORMAT", "CONTRACT", ["cargo", "fmt", "--all", "--", "--check"], "El código Rust está formateado.", ok=exit_zero)
    ev.run(
        "P098-CLIPPY",
        "CONTRACT",
        ["cargo", "clippy", "-p", "larm-storage", "-p", "larm-db", "-p", "larm-models", "-p", "larm-api", "-p", "larm-daemon", "--all-targets", "-j1", "--", "-D", "warnings"],
        "Los crates afectados pasan Clippy sin advertencias.",
        ok=exit_zero,
    )
    ev.run(
        "P098-PACKAGE-verification",
        "CONTRACT",
        [".venv-tools/Scripts/python.exe", "../LARM_prompts_v3/tools/verify_pack.py"],
        "La revisión contractual 3.2.0 pasa los validadores del paquete.",
        ok=exit_zero,
    )
    ev.run(
        "P098-PACKAGE-copies",
        "CONTRACT",
        [".venv-tools/Scripts/python.exe", "tools/package_copy.py", "check"],
        "Las copias locales sincronizadas coinciden con el paquete 3.2.0.",
        ok=exit_zero,
    )
    ev.blocked(
        "P098-REAL-import-and-recovery",
        "REAL",
        "Importación REAL de bundles representativos, recuperación desde sidecars sin base de datos y recorrido multidisco",
        "Importa bundles reales representativos, reconstruye el registro tras perder SQLite y valida el flujo entre volúmenes.",
        "El handler y el worker durable pasan pruebas con SQLite y fixtures de GGUF, CTranslate2, NCNN y MNN; la reconstrucción desde sidecars pasa en una base nueva. Falta la aceptación REAL autorizada con bundles representativos y el comando completo de recuperación, además del recorrido multidisco. La sandbox de Windows también impide crear junctions.",
    )
    status = ev.finish(
        changed_files=[
            "apps/daemon/src/main.rs",
            "apps/daemon/src/model_import_preview.rs",
            "apps/daemon/src/serve.rs",
            "apps/desktop/src/model-import.tsx",
            "apps/desktop/src/models.test.tsx",
            "apps/desktop/src/styles.css",
            "crates/larm-api/src/import_preview.rs",
            "crates/larm-api/src/model_import.rs",
            "crates/larm-api/src/lib.rs",
            "crates/larm-api/src/openapi.rs",
            "crates/larm-api/tests/import_preview.rs",
            "crates/larm-api/tests/model_import.rs",
            "crates/larm-db/src/lib.rs",
            "crates/larm-db/Cargo.toml",
            "crates/larm-db/src/model_import.rs",
            "crates/larm-db/src/migrate.rs",
            "crates/larm-db/tests/adapter_selection.rs",
            "crates/larm-db/tests/integration.rs",
            "crates/larm-domain/src/lib.rs",
            "crates/larm-domain/src/model_import.rs",
            "crates/larm-models/src/lib.rs",
            "crates/larm-models/Cargo.toml",
            "crates/larm-models/src/import.rs",
            "crates/larm-models/src/import_worker.rs",
            "crates/larm-models/src/scan.rs",
            "crates/larm-models/tests/import.rs",
            "crates/larm-models/tests/import_worker.rs",
            "crates/larm-storage/src/import.rs",
            "docs/api/openapi.json",
            "docs/contracts/REVISION_3_2_0_P098.md",
            "docs/contracts/api-operations.json",
            "docs/contracts/manifest.json",
            "docs/contracts/schemas/larm.schema.json",
            "docs/implementation/STATE.md",
            "docs/reports/P098_importadores-de-todos-los-bundles-y-recuperacion-sin-base-de-datos.md",
            "docs/reports/P123_mis-modelos-y-gestion-multidisco.md",
            "migrations/0007_p098_local_import_source.sql",
            "tools/p098_evidence.py",
        ],
        blockers=[
            "Falta ejecutar recovery end-to-end desde sidecars y backup en un escenario real, fault injection de IO/daemon y aceptación REAL autorizada con bundles representativos en varios volúmenes.",
            "El host Windows restringe la creación de junctions en la prueba de walker; la ruta de junction requiere validación en un entorno cualificado.",
        ],
    )
    return 1 if status == "BLOCKED" else 0


if __name__ == "__main__":
    raise SystemExit(main())
