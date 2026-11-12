"""Independent P092 preparation; retain the real dependency/import gates."""
import sys
from evidence import Evidence, REPO, cargo_ok, exit_zero


def main():
    ev = Evidence("P092", extra_inputs=["contracts/06_ECOSYSTEM.md"])
    files = [
        "crates/larm-core/src/recommendation.rs", "crates/larm-core/tests/recommendation.rs",
        "crates/larm-core/src/lib.rs", "crates/larm-domain/src/evaluation.rs",
        "crates/larm-domain/tests/evaluation.rs", "crates/larm-domain/src/lib.rs",
        "tests/fixtures/evaluation-reference.json", "tools/p092_schema_evidence.py", "tools/p092_evidence.py",
    ]
    fixtures = [REPO / name for name in files[:7]]
    ev.run("P092-PURE-COMPOSER", "UNIT", ["cargo", "test", "-p", "larm-domain", "--test", "evaluation", "-p", "larm-core", "--test", "recommendation", "--offline", "-j1", "--", "--test-threads=1"],
           "Tupla completa ligada a compatibilidad/contexto; gates y coverage; ranking con/sin evaluación comparable, pesos explícitos, fuentes equivalentes, top-N y empates deterministas. Fixtures sintéticas, no cualificación/importación.", ok=cargo_ok, fixtures=fixtures)
    ev.run("P092-EVALUATION-WIRE", "CONTRACT", [sys.executable, "-X", "utf8", "tools/p092_schema_evidence.py"],
           "El fixture y los nullable explícitos cumplen el schema ya publicado; campos ausentes/extra, enums y tipos inválidos se rechazan. No autentica el origen.", ok=exit_zero, fixtures=[REPO / files[6], REPO / "docs/contracts/schemas/domain.schema.json"])
    ev.run("P092-ARCHITECTURE", "CONTRACT", [sys.executable, "-X", "utf8", "tools/check_architecture.py"],
           "Grafo contractual intacto; Core usa dominio/SDK existentes, sin dependencias nuevas.")
    ev.run("P092-PACKAGE-COPIES", "CONTRACT", [sys.executable, "-X", "utf8", "tools/package_copy.py", "check"],
           "Las copias siguen siendo idénticas al paquete 3.12.1, sin revisión normativa nueva.")
    ev.blocked("P092-REAL-QUALIFIED-CANDIDATES", "REAL",
               "Componer candidatos desde catálogo/compatibilidad/benchmark cualificados en Windows y la política efectiva; importar evaluación mediante P116 y conectar Search antes de aceptar Descubrir.",
               "Tuplas físicas y referencias externas autorizadas llegan al compositor y producen ranking y razones verificables en la app.",
               "P044/P091 no tienen REAL_PASS integral; faltan el productor P116 autenticado, composición de candidatos físicos y conexión con Search. El módulo puro no los sustituye.")
    ev.finish(files, blockers=["Preparación independiente: sin endpoint nuevo, importación externa, permisos de ejecución, promoción ni UI de recomendaciones activada."])


if __name__ == "__main__":
    main()
