#!/usr/bin/env python3
"""Evidencia para estimación de memoria y scheduler de instancias (P035)."""

from __future__ import annotations

from evidence import Evidence, REPO, cargo_ok


def main() -> int:
    ev = Evidence("P035")
    memory = REPO / "crates/larm-runtime/src/memory.rs"
    scheduler = REPO / "crates/larm-runtime/src/scheduler.rs"
    guard_db = REPO / "crates/larm-db/src/guard.rs"
    target = {"CARGO_TARGET_DIR": str(REPO / "target/verification-final")}

    ev.run(
        "P035-MEMORY-ESTIMATE-AND-PREFLIGHT",
        "CONTRACT",
        [
            "cargo",
            "test",
            "-p",
            "larm-runtime",
            "--lib",
            "memory::tests",
            "-j",
            "1",
            "--",
            "--test-threads=1",
        ],
        expected=(
            "Fórmula KV con ceil, overhead y margen, metadata incompleta UNKNOWN, multimodal delegado al adapter, "
            "memoria unificada deduplicada, mínimo de límites perfil/lease/budget, cero/overflow y reserva insuficiente."
        ),
        ok=cargo_ok,
        fixtures=[memory, REPO / "crates/larm-domain/src/estimate.rs", REPO / "crates/larm-domain/src/hardware.rs"],
        env=target,
    )
    ev.run(
        "P035-SCHEDULER-PRIORITY-AND-UNLOAD",
        "CONTRACT",
        [
            "cargo",
            "test",
            "-p",
            "larm-runtime",
            "--lib",
            "scheduler::tests",
            "-j",
            "1",
            "--",
            "--test-threads=1",
        ],
        expected=(
            "Prioridades PINNED/HIGH/NORMAL/BACKGROUND, FIFO de starts, límite de arranques, IDLE_TIMEOUT, "
            "MEMORY_PRESSURE y LRU protegen leases, warm, PINNED y procesos externos."
        ),
        ok=cargo_ok,
        fixtures=[scheduler, REPO / "crates/larm-core/src/config/mod.rs", REPO / "docs/contracts/defaults.json"],
        env=target,
    )
    ev.run(
        "P035-ATOMIC-MULTI-POOL-RESERVATION",
        "INTEGRATION",
        [
            "cargo",
            "test",
            "-p",
            "larm-db",
            "--test",
            "guard",
            "reserva_de_pools",
            "-j",
            "1",
            "--",
            "--test-threads=1",
        ],
        expected=(
            "SQLite conserva reservas existentes si falla un pool, rechaza claves repetidas y sustituye "
            "el conjunto multi-pool de forma atómica."
        ),
        ok=cargo_ok,
        fixtures=[guard_db, REPO / "crates/larm-db/tests/guard.rs", REPO / "migrations/0001_bootstrap.sql"],
        env=target,
    )
    ev.run(
        "P035-SINGLE-POOL-COMPATIBILITY",
        "INTEGRATION",
        [
            "cargo",
            "test",
            "-p",
            "larm-db",
            "--test",
            "guard",
            "reserva_de_un_pool_conserva_otros_pools_del_titular",
            "-j",
            "1",
            "--",
            "--exact",
            "--test-threads=1",
        ],
        expected="Actualizar una reserva individual sustituye su pool sin borrar reservas del mismo titular en otros pools.",
        ok=cargo_ok,
        fixtures=[guard_db, REPO / "crates/larm-db/tests/guard.rs"],
        env=target,
    )
    ev.run(
        "P035-GUARD-AND-SQLITE-ADMISSION",
        "INTEGRATION",
        [
            "cargo",
            "test",
            "-p",
            "larm-runtime",
            "--lib",
            "memory::tests::admission_real_reserva_en_sqlite_bajo_guard_p019",
            "-j",
            "1",
            "--",
            "--exact",
            "--test-threads=1",
        ],
        expected=(
            "El estimate supera preflight, adquiere guards P019, registra la reserva SQLite por pool y libera "
            "los guards temporales tras la confirmación."
        ),
        ok=cargo_ok,
        fixtures=[memory, guard_db, REPO / "crates/larm-db/src/guard.rs", REPO / "crates/larm-domain/src/guard.rs"],
        env=target,
    )
    ev.run(
        "P035-OPERATOR-HARD-BUDGET-ADMISSION",
        "INTEGRATION",
        [
            "cargo",
            "test",
            "-p",
            "larm-runtime",
            "--lib",
            "memory::tests::admission_unknown_persistida_exige_override_y_budget_duro",
            "-j",
            "1",
            "--",
            "--exact",
            "--test-threads=1",
        ],
        expected=(
            "UNKNOWN no crea una reserva automática; sólo el modo explícito OperatorHardBudget y un límite duro "
            "por pool permiten persistir el límite verificado en SQLite bajo P019."
        ),
        ok=cargo_ok,
        fixtures=[memory, guard_db, REPO / "crates/larm-db/src/guard.rs"],
        env=target,
    )
    ev.run(
        "P035-REAL-WINDOWS-MEMORY-PREFLIGHT",
        "REAL",
        [
            "cargo",
            "test",
            "-p",
            "larm-runtime",
            "--lib",
            "memory::tests::hardware_real_de_windows_verifica_admision_con_memoria_observada",
            "-j",
            "1",
            "--",
            "--exact",
            "--nocapture",
            "--test-threads=1",
        ],
        expected=(
            "En Windows, GlobalMemoryStatusEx observa RAM real y el mismo preflight P035 admite o rechaza "
            "por capacidad disponible, sin fixture de capacidad del host."
        ),
        ok=cargo_ok,
        fixtures=[memory, REPO / "crates/larm-domain/src/hardware.rs"],
        env=target,
    )
    ev.blocked(
        "P035-REAL-LAUNCH-AND-PERSISTED-INSTANCE",
        "REAL",
        "LaunchProfile guardado → LaunchPlan → RuntimeInstance/lease → memory reservation → proceso supervisado",
        "Un perfil persistido participa en el launch real y la instancia conserva la admisión hasta detenerse.",
        "P034 y P033 siguen BLOCKED_REAL en la integración productiva LaunchPlan→RuntimeInstance; P011 y P019 "
        "tampoco alcanzan REAL_PASS, por lo que no se puede declarar cualificada la admisión de un consumidor.",
    )

    ev.finish(
        changed_files=[
            "crates/larm-domain/src/ports.rs",
            "crates/larm-db/src/guard.rs",
            "crates/larm-db/tests/guard.rs",
            "crates/larm-runtime/Cargo.toml",
            "crates/larm-runtime/src/lib.rs",
            "crates/larm-runtime/src/memory.rs",
            "crates/larm-runtime/src/scheduler.rs",
            "tools/p035_evidence.py",
            "docs/implementation/STATE.md",
            "docs/reports/P035_memory-estimator-y-scheduler-de-instancias.md",
        ],
        blockers=[
            "La reserva multi-pool y el preflight funcionan en pruebas, pero P035 aún no está conectado al flujo "
            "productivo de LaunchPlan/RuntimeInstance/lease; no se declara admisión real de un modelo gestionado."
        ],
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
