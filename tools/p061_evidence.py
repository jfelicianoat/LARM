#!/usr/bin/env python3
"""P061: puerta del núcleo del adaptador llama.cpp — compatibilidad por
metadata GGUF, ModelBundle con mmproj, argumentos de lanzamiento sin shell,
ciclo de vida de llama-server, smoke de inferencia y llama-bench con salida
cruda en los dos builds reales. Las interfaces de integración (Runtime
Adapter Protocol empaquetado, admisión runtime.install con lock cualificado)
siguen bloqueadas por P099/P031/P036 (scope del cierre de la fase)."""

import sys

from evidence import Evidence, REPO

TOOL_PYTHON = REPO / ".venv-tools" / "Scripts" / "python.exe"
PYTHON = str(TOOL_PYTHON) if TOOL_PYTHON.is_file() else sys.executable


def main() -> int:
    ev = Evidence("P061")
    focal = REPO / "adapters/runtime/llama-cpp/tests/test_p061_core.py"

    def real_ok_factory(expected_markers: tuple[str, ...]):
        def observed(code: int, output: str) -> bool:
            return code == 0 and all(marker in output for marker in expected_markers)
        return observed

    ev.run(
        "P061-CONTRACT-core-synthetic",
        "CONTRACT",
        [PYTHON, "-X", "utf8", str(focal.relative_to(REPO)),
         "CompatibilidadSintetica", "FormatoBench"],
        "lectura GGUF sintética (magic/version/kv), arch desconocido incompatible, tool_calling/multimodal false sin evidencia, ModelBundle exige mmproj para mllama, argumentos sólo flags contractuales sin shell, parser llama-bench versionado con formato desconocido fallando",
        fixtures=[focal],
    )
    ev.run(
        "P061-REAL-gguf-compatibility-pc-ia",
        "REAL",
        [PYTHON, "-X", "utf8", str(focal.relative_to(REPO)), "RealLlamaCppPCIA.test_00_compatibilidad_con_gguf_real_de_ollama"],
        "compatibilidad determinada por metadata del GGUF real de Ollama (granite-embedding 30m, arch bert) + capacidades del build b79696f; embeddings declarado, tool_calling/multimodal false",
        ok=real_ok_factory(("REAL compat: arch=bert", "'embeddings': True")),
        timeout=300,
        fixtures=[focal],
    )
    ev.run(
        "P061-REAL-server-lifecycle-health-models-embeddings",
        "REAL",
        [PYTHON, "-X", "utf8", str(focal.relative_to(REPO)), "RealLlamaCppPCIA.test_10_ciclo_de_vida_server_health_models_embeddings_y_parada"],
        "llama-server real (build b79696f) arrancado en puerto loopback del PC IA con el GGUF real: health 200, /v1/models con el modelo, /v1/embeddings con vector 384d y parada determinista",
        ok=real_ok_factory(("REAL server: health=200", "models=1", "embedding_dim=384")),
        timeout=600,
        fixtures=[focal],
    )
    ev.run(
        "P061-REAL-llama-bench-two-builds",
        "REAL",
        [PYTHON, "-X", "utf8", str(focal.relative_to(REPO)), "RealLlamaCppPCIA.test_20_llama_bench_real_en_los_dos_builds"],
        "llama-bench real en los dos builds side-by-side (b79696f y d2d6269): 5 repeticiones, salida cruda con sha256, parser versionado y versiones distintas entre builds",
        ok=real_ok_factory(("REAL bench: versiones=", "b79696f", "d2d6269")),
        timeout=900,
        fixtures=[focal],
    )
    ev.blocked(
        "P061-REAL-text-generation-smoke-chat-model",
        "REAL",
        "smoke de generación de texto con un modelo de chat autorizado",
        "chat completion real (tokens limitados) contra llama-server con un modelo de chat del PC IA",
        "los modelos de chat disponibles (tev1 4b / gemma4 12b) requieren la autorización explícita del operador para servirlos fuera de Ollama; embeddings ya probado con el GGUF real más pequeño",
    )
    ev.blocked(
        "P061-REAL-runtime-adapter-protocol-package",
        "REAL",
        "paquete del Runtime Adapter Protocol de llama.cpp con admisión y leases",
        "worker empaquetado con los métodos runtime.* observados por la cualificación P026 real y admisión runtime.install con profile lock cualificado",
        "la admisión P031 exige lock cualificado (P099 UNQUALIFIED) y las interfaces P034/P036 conservan BLOCKED_REAL (contrato 00: el consumidor no se completa sin REAL_PASS)",
    )

    changed = [
        "adapters/runtime/llama-cpp/core.py",
        "adapters/runtime/llama-cpp/tests/test_p061_core.py",
        "adapters/runtime/llama-cpp/README.md",
        "docs/adapters/llama-cpp.md",
        "docs/implementation/STATE.md",
        "docs/reports/P061_llama-cpp-compatibilidad-serving-smoke-tests-y-llama-bench.md",
        "docs/reports/P061_llama-cpp-compatibilidad-serving-smoke-tests-y-llama-bench.evidence.json",
        "tools/p061_evidence.py",
    ]
    blockers = [
        "paquete del Runtime Adapter Protocol y admisión con leases: dependencias P034/P036/P090/P099/P031 en BLOCKED_REAL (contrato 00)",
        "smoke de generación con modelo de chat: requiere autorización explícita del operador para servir los GGUF de chat del PC IA fuera de Ollama",
    ]
    status = ev.finish(changed, blockers)
    return 0 if "FAIL" not in status else 1


if __name__ == "__main__":
    raise SystemExit(main())
