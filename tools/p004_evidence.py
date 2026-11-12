#!/usr/bin/env python3
"""P004 - persistencia SQLite y framework de migraciones: pruebas y evidencia."""
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from evidence import REPO, Evidence, cargo_ok  # noqa: E402

PY = sys.executable


def main() -> int:
    ev = Evidence("P004", extra_inputs=["contracts/foreign-keys.json"])
    ddl = REPO / "docs/contracts/persistence.sql"
    migration = REPO / "migrations/0001_bootstrap.sql"
    fx = [ddl, migration, REPO / "migrations/0002_adapter_selection.sql", REPO / "migrations/0003_runtime_channel_identity.sql"]
    integration = [
        ("P004-INTEGRATION-pragmas", "pragmas_del_contrato", "WAL, foreign_keys=ON, busy_timeout=5000, quick_check ok"),
        ("P004-CONTRACT-ddl-tables", "migracion_desde_vacia",
         "desde vacía: las 49 tablas del DDL y auxiliares del producto; migraciones con sha256 y applied_at ms+Z"),
        ("P004-INTEGRATION-idempotent", "reaplicar_migraciones", "reaplicar (y tras reabrir) no cambia esquema ni schema_metadata"),
        ("P004-FAULT-migration-atomic", "migracion_que_falla", "migración rota a mitad no deja tablas ni registro; corregida se aplica una vez"),
        ("P004-FAULT-migration-drift", "migracion_aplicada_modificada", "migración aplicada con otro contenido -> CONFLICT"),
        ("P004-FAULT-tx-rollback", "rollback_transaccional", "fallo inducido revierte la inserción válida; una transacción correcta persiste"),
        ("P004-FAULT-fk-deferred", "fk_diferida", "FK huérfana rechazada en COMMIT; borrar un padre referenciado falla"),
        ("P004-CONTRACT-h07", "h07_transformation", "FAILED_RECOVERABLE con causa persistida; estado fuera del contrato rechazado"),
        ("P004-CONTRACT-h01-h02-h08", "enums_del_ddl", "5 canales, 15 estrategias y 3 prioridades persisten; valores ajenos rechazados"),
        ("P004-CONTRACT-bytes-i64", "bytes_hasta_i64_max", "i64::MAX roundtrip como string decimal; negativo rechazado por CHECK"),
        ("P004-INTEGRATION-backup-restore", "backup_y_restore", "backup verificado por filas; cambio detectado; restore devuelve filas y conserva la previa"),
        ("P004-FAULT-restore-invalid", "restore_de_un_fichero", "backup no SQLite o inexistente no toca la base viva"),
        ("P004-UNIT-temp-db-helper", "helper_de_base_temporal", "helper crea base migrada sana y la borra al soltarla"),
    ]
    for test_id, filt, expected in integration:
        kind = test_id.split("-")[1]
        ev.run(test_id, kind, ["cargo", "test", "-p", "larm-db", "--test", "integration", "--", filt],
               expected, ok=cargo_ok, fixtures=fx)
    ev.run("P004-UNIT-db-lib", "UNIT", ["cargo", "test", "-p", "larm-db", "--lib"],
           "dto: bytes hasta i64::MAX y negativo -> INTEGRITY_FAILED; pool: bloqueo -> RESOURCE_UNAVAILABLE reintentable", ok=cargo_ok)
    ev.run("P004-INTEGRATION-literal-paths", "INTEGRATION",
           ["cargo", "test", "-p", "larm-db", "--test", "sqlite_paths", "-j1", "--", "--test-threads=1"],
           "Rutas canónicas Windows, Unicode, porcentaje, almohadilla y apóstrofe conservan el archivo literal al abrir/reabrir y al copiar/verificar/restaurar.",
           ok=cargo_ok, fixtures=[REPO / "crates/larm-db/src/pool.rs", REPO / "crates/larm-db/src/backup.rs", REPO / "crates/larm-db/tests/sqlite_paths.rs"])
    ev.run("P004-CONTRACT-acquisition-migration", "CONTRACT",
           [PY, "-c", "from pathlib import Path; import hashlib; "
            "a=Path('migrations/0014_model_acquisition_manifest.sql').read_bytes(); "
            "b=Path('docs/contracts/migrations/0014_model_acquisition_manifest.sql').read_bytes(); "
            "assert a==b; print('0014 normativa idéntica sha256 '+hashlib.sha256(a).hexdigest())"],
           "La migración nueva coincide con la revisión contractual; integration verifica el DDL convergente tras aplicar toda la cadena, sin reescribir bootstrap histórico.",
           fixtures=fx+[REPO / "migrations/0014_model_acquisition_manifest.sql", REPO / "docs/contracts/migrations/0014_model_acquisition_manifest.sql"])

    ev.run("P004-UNIT-cargo-fmt", "UNIT", ["cargo", "fmt", "--all", "--check"], "sin diferencias de formato")
    ev.run("P004-UNIT-cargo-clippy", "UNIT", ["cargo", "clippy", "--workspace", "--all-targets", "--", "-D", "warnings"],
           "clippy sin avisos en todo el workspace (incluido larm-db)")
    ev.run("P004-UNIT-cargo-test", "UNIT", ["cargo", "test", "--workspace"], "pruebas del workspace en verde", ok=cargo_ok)

    ev.run("P004-REAL-daemon-build", "REAL", ["cargo", "build", "-p", "larm-daemon"], "binario del daemon compilado")
    for sub, expected in [
        ("dbinfo", "db-info dos veces: 14 migraciones hasta adquisición 3.12.0 y luego ninguna; base sana; 54 tablas; sin rutas personales"),
        ("backup-restore", "25 filas -> db-backup -> borradas -> db-restore -> 25 filas; previa conservada"),
        ("restore-in-use", "base abierta por otro proceso -> RESOURCE_UNAVAILABLE, base intacta y sin restos"),
        ("busy-timeout", "bloqueo retenido -> RESOURCE_UNAVAILABLE tras busy_timeout sin esquema a medias; liberado migra"),
    ]:
        ev.run(f"P004-REAL-{sub}", "REAL", [PY, "tools/p004_real.py", sub], expected, fixtures=fx)

    changed = [
        "crates/larm-db/Cargo.toml", "crates/larm-db/src/lib.rs", "crates/larm-db/src/migrate.rs",
        "crates/larm-db/src/pool.rs", "crates/larm-db/src/backup.rs", "crates/larm-db/src/dto.rs",
        "crates/larm-db/src/tx.rs", "crates/larm-db/src/testing.rs", "crates/larm-db/tests/integration.rs",
        "crates/larm-db/tests/sqlite_paths.rs",
        "apps/daemon/Cargo.toml", "apps/daemon/src/main.rs", "docs/operations/database.md",
        "tools/p004_real.py", "tools/p004_evidence.py",
    ]
    ev.finish(changed, [])
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
