#!/usr/bin/env python3
"""P016 - ArtifactLocation, resolución de rutas y movimiento seguro: pruebas y evidencia."""
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from evidence import Evidence, cargo_ok  # noqa: E402

PY = sys.executable


def main() -> int:
    ev = Evidence("P016")
    ev.run("P016-UNIT-rutas-y-resolutor", "UNIT", ["cargo", "test", "-p", "larm-domain", "--lib", "--", "artifacts::tests"],
           "rutas relativas sin absolutas, unidad, ADS, ./.. ni nombres reservados; raíz desde el GUID con letra cambiada, "
           "sin letra (\\\\?\\Volume{…}) y montaje en carpeta; desconectado no inventa ruta; fases y serialización", ok=cargo_ok)
    ev.run("P016-UNIT-plan", "UNIT", ["cargo", "test", "-p", "larm-storage", "--lib", "--", "artifacts::tests"],
           "RENAME en el mismo volumen, COPY entre volúmenes; EXTERNAL, HOST_MANAGED y compartidos se conservan; locks en orden "
           "global; espacio solo de las copias; hash determinista que cambia con row_version; negativos de estado", ok=cargo_ok)
    tests = [
        ("P016-INTEGRATION-mismo-volumen", "mismo_volumen_renombra", "renombrado atómico, registro v2, evento artifact.moved, sin locks"),
        ("P016-INTEGRATION-otro-volumen", "otro_volumen_copia_verifica_y_borra", "copia+flush+hash releído, reserva durante el job, origen borrado, plan de un uso"),
        ("P016-FAULT-hash-distinto", "hash_distinto_conserva_el_origen", "copia corrupta: INTEGRITY_FAILED, origen válido, sin provisionales"),
        ("P016-FAULT-destino-desaparece", "destino_desaparece_antes_del_commit", "destino borrado antes del cambio: origen sigue siendo el registrado"),
        ("P016-INTEGRATION-lease-activa", "lease_activa_bloquea_y_al_soltarla_se_mueve", "ACTIVE_LEASE 409 bajo el lock, plan intacto; al soltar la lease se mueve"),
        ("P016-INTEGRATION-bloqueado", "bloqueado_por_otra_operacion", "otro titular: CONFLICT resource_locked reintentable"),
        ("P016-INTEGRATION-plan-obsoleto", "plan_obsoleto_caducado_o_ausente", "sin hash, hash inventado, otro destino, fila cambiada, caducado, variante/destino/origen ausentes"),
        ("P016-INTEGRATION-external-compartido", "external_y_compartidos_se_conservan", "EXTERNAL no se mueve ni se borra"),
        ("P016-FAULT-acceso-fallido", "acceso_fallido_tras_el_cambio_devuelve_el_registro", "destino ilegible tras el cambio: registro vuelve al origen con CAS, ambas copias"),
        ("P016-FAULT-caida-copiando", "caida_copiando_recupera_al_origen", "caída en COPYING: el lock sobrevive; recuperación deshace"),
        ("P016-FAULT-caida-verificado", "caida_verificado_retira_la_copia_colocada", "caída tras colocar la copia: se retira, origen válido"),
        ("P016-FAULT-caida-tras-cambio", "caida_tras_cambio_de_registro_termina", "caída en DB_SWITCHED: la recuperación prueba acceso, borra origen y completa"),
        ("P016-FAULT-caida-tras-cambio-sin-destino", "caida_tras_cambio_con_destino_perdido_vuelve_al_origen", "caída en DB_SWITCHED y destino perdido: vuelve al origen"),
        ("P016-FAULT-caida-borrando", "caida_borrando_el_origen_termina", "caída en ACCESS_TESTED: la recuperación termina el borrado"),
        ("P016-FAULT-caida-renombrando", "caida_renombrando_se_deshace", "caída con el renombrado hecho: se deshace"),
        ("P016-INTEGRATION-cambio-con-fencing", "cambio_de_registro_exige_lock_y_version", "sin lock exclusivo o con versión vieja no cambia nada; con ambos, fila+journal+evento"),
        ("P016-FAULT-sin-lock-no-borra", "sin_lock_no_se_borra_el_origen", "lock perdido: se detiene antes de borrar el origen"),
    ]
    for test_id, filt, expected in tests:
        kind = test_id.split("-")[1]
        ev.run(test_id, kind, ["cargo", "test", "-p", "larm-daemon", "--test", "move", "--", filt, "--exact"], expected, ok=cargo_ok)
    ev.run("P016-INTEGRATION-guard-lease-variante", "INTEGRATION",
           ["cargo", "test", "-p", "larm-db", "--test", "guard", "--", "lease_de_la_variante_bajo_el_mismo_lock"],
           "exclusivo sobre una variante con lease viva: ACTIVE_LEASE en la misma transacción; compartido convive", ok=cargo_ok)
    ev.run("P016-INTEGRATION-journal-desde-fase", "INTEGRATION",
           ["cargo", "test", "-p", "larm-db", "--test", "guard", "--", "journal_solo_desde_las_fases_esperadas"],
           "un plan PLANNED solo se bloquea una vez: el segundo uso es CONFLICT journal_phase_conflict sin locks", ok=cargo_ok)
    ev.run("P016-INTEGRATION-sondeo-concurrente", "INTEGRATION",
           ["cargo", "test", "-p", "larm-db", "--test", "storage", "--", "ubicacion_creada_durante_el_sondeo_no_se_marca_offline"],
           "defecto heredado de P012/P014: una ubicación creada durante un sondeo ya no se marca OFFLINE", ok=cargo_ok)
    ev.run("P016-INTEGRATION-api", "INTEGRATION", ["cargo", "test", "-p", "larm-api", "--test", "artifacts"],
           "POST /models/{id}/move: scopes 401/403, schema y patrones 400, Idempotency-Key, ACTIVE_LEASE/STALE_REVISION tal cual, "
           "aprobación ligada a plan_hash y recursos y de un uso; GET /artifact-locations paginado con cursor firmado y 404", ok=cargo_ok)
    ev.run("P016-UNIT-cargo-fmt", "UNIT", ["cargo", "fmt", "--all", "--check"], "sin diferencias de formato")
    ev.run("P016-UNIT-cargo-clippy", "UNIT", ["cargo", "clippy", "--workspace", "--all-targets", "--", "-D", "warnings"], "clippy sin avisos")
    ev.run("P016-UNIT-cargo-test", "UNIT", ["cargo", "test", "--workspace"], "pruebas del workspace en verde", ok=cargo_ok)
    ev.run("P016-REAL-architecture", "REAL",
           ["powershell", "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", "tools/check_architecture.ps1"], "grafo de crates intacto")
    ev.run("P016-REAL-build-daemon", "REAL", ["cargo", "build", "-p", "larm-daemon"], "daemon compilado")
    ev.run("P016-REAL-openapi", "REAL", [PY, "tools/p005_real.py", "openapi"],
           "docs/api/openapi.json al día con POST /models/{id}/move y GET /artifact-locations implementados")
    ev.run("P016-REAL-movimiento", "REAL", [PY, "tools/p016_real.py", "serve"],
           "PC IA con el daemon: plan y renombrado en C:, copia verificada a \\\\localhost\\Temp y vuelta, lease ACTIVE 409, "
           "plan obsoleto 409, hash distinto real, TerminateProcess copiando 1 GiB y recuperación desde el journal",
           timeout=1800)
    ev.blocked("P016-REAL-G05-dos-volumenes-fisicos", "REAL",
               "G05: mover la variante entre dos volúmenes físicos con cortes tras cada fase del journal",
               "lease bloquea; origen hasta hash+cambio+acceso; estado final, ruta y hash correctos tras recovery",
               "el PC IA solo tiene un volumen escribible (bloqueo de P000). \\\\localhost\\Temp es otra volume_key sobre el mismo disco")
    ev.blocked("P016-REAL-letra-cambiada", "REAL",
               "cambiar la letra de un volumen con modelos y resolver sus rutas desde el GUID",
               "el modelo se abre en la letra nueva sin tocar el registro",
               "necesita un segundo volumen cuya letra se pueda cambiar (bloqueo de P000); el resolutor está probado con observaciones")
    ev.blocked("P016-REAL-lease-p100", "REAL",
               "repetir el bloqueo con una lease real de P100 en vez de una fila sembrada",
               "POST /move contra un modelo con lease ACTIVE real devuelve ACTIVE_LEASE",
               "P100 (leases) aún no está implementado")
    changed = [
        "crates/larm-domain/src/artifacts.rs", "crates/larm-domain/src/ports.rs", "crates/larm-domain/src/guard.rs",
        "crates/larm-domain/src/lib.rs", "crates/larm-db/src/artifacts.rs", "crates/larm-db/src/guard.rs",
        "crates/larm-db/src/storage.rs", "crates/larm-db/src/lib.rs", "crates/larm-db/tests/guard.rs",
        "crates/larm-db/tests/storage.rs", "crates/larm-db/examples/guard_worker.rs", "crates/larm-core/src/hardware.rs",
        "crates/larm-storage/src/artifacts.rs", "crates/larm-storage/src/move_job.rs", "crates/larm-storage/src/locations.rs",
        "crates/larm-storage/src/lib.rs", "crates/larm-storage/Cargo.toml", "crates/larm-api/src/artifacts.rs",
        "crates/larm-api/src/lib.rs", "crates/larm-api/src/openapi.rs", "crates/larm-api/tests/artifacts.rs",
        "apps/daemon/src/hardware.rs", "apps/daemon/src/serve.rs", "apps/daemon/tests/move.rs", "apps/daemon/Cargo.toml",
        "docs/operations/moves.md", "docs/api/openapi.json", "docs/operations/guard.md", "docs/operations/storage.md", "tools/p016_real.py", "tools/p016_evidence.py",
    ]
    ev.finish(changed, [])
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
