#!/usr/bin/env python3
"""P019 - Guards de artefactos, reservas y journal transaccional: pruebas y evidencia."""
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from evidence import Evidence, cargo_ok  # noqa: E402

PY = sys.executable


def main() -> int:
    ev = Evidence("P019")
    ev.run("P019-UNIT-orden-global", "UNIT", ["cargo", "test", "-p", "larm-domain", "--lib", "--", "guard::tests"],
           "orden volumen<variante<artefacto<pool por id; mismo conjunto en otro orden = misma adquisición; exclusivo manda", ok=cargo_ok)
    tests = [
        ("P019-INTEGRATION-modos-y-lease", "compartido_exclusivo", "SHARED+SHARED; EXCLUSIVE rechazado con resource_locked; lease viva = ACTIVE_LEASE"),
        ("P019-FAULT-todo-o-nada", "todo_o_nada", "fallo en adquisición: ningún lock ni journal a medias; journal LOCKED/COMPLETED en la misma transacción"),
        ("P019-INTEGRATION-fencing", "fencing", "generación vieja o liberada no escribe; compartido no da escritura"),
        ("P019-FAULT-crash-reconciliacion", "reconciliacion", "solo se libera con journal o lease terminados; vivo se conserva; huérfano no se libera por tiempo"),
        ("P019-INTEGRATION-carrera", "carrera", "20 rondas: 10 admisiones contra move y delete; nunca escritor con lectores; perdedores con CONFLICT limpio"),
        ("P019-INTEGRATION-sin-interbloqueo", "orden_global", "30 rondas de A,B contra B,A: exactamente uno gana, sin esperas cíclicas"),
        ("P019-INTEGRATION-cas-compartido", "cas_compartido", "blob referenciado por variantes y bloqueado no es borrable"),
        ("P019-INTEGRATION-reservas", "dos_raices", "dos raíces del mismo volumen = una reserva; pool con presupuesto, sustitución y desbordamiento"),
    ]
    for test_id, filt, expected in tests:
        kind = test_id.split("-")[1]
        ev.run(test_id, kind, ["cargo", "test", "-p", "larm-db", "--test", "guard", "--", filt], expected, ok=cargo_ok)
    ev.run("P019-UNIT-cargo-fmt", "UNIT", ["cargo", "fmt", "--all", "--check"], "sin diferencias de formato")
    ev.run("P019-UNIT-cargo-clippy", "UNIT", ["cargo", "clippy", "--workspace", "--all-targets", "--", "-D", "warnings"], "clippy sin avisos")
    ev.run("P019-UNIT-cargo-test", "UNIT", ["cargo", "test", "--workspace"], "pruebas del workspace en verde", ok=cargo_ok)
    ev.run("P019-REAL-architecture", "REAL",
           ["powershell", "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", "tools/check_architecture.ps1"], "grafo de crates intacto")
    ev.run("P019-REAL-build-worker", "REAL", ["cargo", "build", "-p", "larm-db", "--example", "guard_worker"], "trabajador de prueba compilado")
    ev.run("P019-REAL-concurrencia", "REAL", [PY, "tools/p019_real.py", "concurrencia"],
           "PC IA: 30 lectores y 6 movedores en procesos distintos sobre un fichero real entre C: y \\\\localhost\\Temp: "
           "sin solapes, hash íntegro, contención real; crash con lock y reconciliación por journal; sin locks al final",
           timeout=1200)
    ev.blocked("P019-REAL-dos-volumenes-fisicos", "REAL",
               "repetir la concurrencia con el fichero alternando entre dos volúmenes físicos distintos",
               "mismas invariantes con copia entre discos",
               "el PC IA solo tiene un volumen escribible (bloqueo de P000). La prueba usa C: y \\\\localhost\\Temp: dos volume_key, mismo disco físico")
    ev.blocked("P019-REAL-leases-reales", "REAL",
               "repetir la carrera con leases reales de P100 como titulares de los locks compartidos",
               "un move contra una lease ACTIVE real recibe ACTIVE_LEASE y no toca el artefacto",
               "P100 (leases) aún no está implementado; el prompt pide repetir la prueba cuando lo esté")
    changed = [
        "crates/larm-domain/src/guard.rs", "crates/larm-domain/src/ports.rs", "crates/larm-domain/src/lib.rs",
        "crates/larm-db/src/guard.rs", "crates/larm-db/src/reservations.rs", "crates/larm-db/src/lib.rs",
        "crates/larm-db/tests/guard.rs", "crates/larm-db/examples/guard_worker.rs", "docs/operations/guard.md",
        "tools/p019_real.py", "tools/p019_evidence.py",
    ]
    ev.finish(changed, [])
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
