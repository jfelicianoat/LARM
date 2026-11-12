#!/usr/bin/env python3
"""P091 evidence: deterministic TechnicalFit scoring and real-use blockers."""

from evidence import Evidence, REPO, cargo_ok


def main() -> int:
    ev = Evidence("P091")
    score = REPO / "crates/larm-core/src/compatibility/score.rs"
    compatibility = REPO / "crates/larm-core/src/compatibility.rs"
    defaults = REPO / "docs/contracts/defaults.json"
    schema = REPO / "tools/p091_schema_evidence.py"

    ev.run(
        "P091-FMT",
        "UNIT",
        ["cargo", "fmt", "--all", "--check"],
        "El workspace Rust conserva formato válido.",
        fixtures=[score, compatibility],
    )
    ev.run(
        "P091-SCORE-AND-NEGATIVE-CASES",
        "CONTRACT",
        ["cargo", "test", "-p", "larm-core", "compatibility::score::tests", "-j", "1", "--", "--test-threads=1"],
        "Determinismo, pesos de defaults, benchmark con huella coincidente, desconocidos, incompatibilidad, schema inválido y backend sin evidencia.",
        fixtures=[score, compatibility, defaults],
        ok=cargo_ok,
    )
    ev.run(
        "P091-PUBLISHED-SCHEMA",
        "CONTRACT",
        [".venv-tools/Scripts/python.exe", "tools/p091_schema_evidence.py"],
        "El schema v3 acepta el score escalar y rechaza breakdown/confidence públicos y scores fuera de rango.",
        fixtures=[schema],
    )
    ev.run(
        "P091-CLIPPY-FOCAL",
        "CONTRACT",
        ["cargo", "clippy", "-p", "larm-core", "--all-targets", "-j", "1", "--", "-D", "warnings"],
        "El módulo de score no introduce warnings ni viola las reglas Clippy del workspace.",
        fixtures=[score],
    )
    ev.run(
        "P091-WINDOWS-CORE-FIXTURES",
        "REAL",
        ["cargo", "test", "-p", "larm-core", "compatibility::score::tests", "-j", "1", "--", "--test-threads=1"],
        "El módulo corre en el host Windows actual sobre fixtures deterministas; esto no cualifica un modelo/runtime real.",
        fixtures=[score, compatibility],
        ok=cargo_ok,
    )
    ev.blocked(
        "P091-QUALIFIED-LOCAL-BENCHMARK",
        "REAL",
        "Ejecutar una suite local de cinco repeticiones sobre modelo/runtime/backend cualificados y fingerprint completo",
        "El benchmark compatible sustituye estimaciones de memoria, velocidad y almacenamiento; estabilidad usa hasta las últimas 20 ejecuciones.",
        "No hay benchmark calificado conectado al motor ni REAL_PASS de P090, P015 o P035; los fixtures prueban la fórmula, no una ejecución real.",
    )

    blockers = [
        "P090, P015 y P035 no tienen REAL_PASS, por lo que el contrato 00 impide cerrar P091.",
        "El score y su breakdown/confidence son un resultado puro interno; el schema v3 sólo permite publicar technical_fit como número o null. No se amplía el contrato público ni se simula persistencia.",
        "El contrato 05 define la puntuación de almacenamiento con MB/s observados, pero no define multiplicadores por NVME/SSD/HDD/NETWORK/REMOVABLE/UNKNOWN. Se muestra la clase junto al score y no se inventa un ajuste numérico.",
        "Los casos de autorización, timeout, IO, journal/outbox y recuperación no aplican al módulo puro, que no realiza efectos externos; los casos de schema, estado/gates y medidas inválidas sí se cubren.",
    ]
    ev.finish(
        changed_files=[
            "crates/larm-core/src/compatibility.rs",
            "crates/larm-core/src/compatibility/score.rs",
            "tools/p091_schema_evidence.py",
            "tools/p091_evidence.py",
            "docs/implementation/STATE.md",
            "docs/reports/P091_estimacion-de-recursos-y-technicalfitscore.md",
        ],
        blockers=blockers,
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
