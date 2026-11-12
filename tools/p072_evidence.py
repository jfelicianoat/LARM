#!/usr/bin/env python3
"""Focused contract and host-probe evidence for P072."""
from __future__ import annotations

import json
import sys
from pathlib import Path

from evidence import Evidence, REPO, exit_zero


def unittest_ok(code: int, output: str) -> bool:
    return code == 0 and "Ran 12 tests" in output and "OK" in output


def main() -> int:
    ev = Evidence("P072")
    test_path = REPO / "adapters/runtime/diffusers/tests/test_worker.py"
    ev.run(
        "P072-INTEGRATION-diffusers-worker-and-protocol-hooks",
        "INTEGRATION",
        [sys.executable, "-m", "unittest", "discover", "-s", "adapters/runtime/diffusers/tests", "-p", "test_worker.py", "-v"],
        "12 pruebas observan identidad separada por lock, bundle Diffusers local, roles contractuales, rechazo de código remoto, safetensors, salida PNG con hash, progreso, cancelación y limpieza ante fallos.",
        ok=unittest_ok,
        fixtures=[test_path, REPO / "adapters/runtime/diffusers/protocol.schema.json"],
        timeout=180,
    )
    ev.run(
        "P072-QUALITY-cargo-format",
        "CONTRACT",
        ["cargo", "fmt", "--all", "--check"],
        "El workspace conserva el formato Rust contractual.",
        timeout=180,
    )
    ev.run(
        "P072-QUALITY-cargo-clippy",
        "CONTRACT",
        ["cargo", "clippy", "--workspace", "--all-targets", "--", "-D", "warnings"],
        "Clippy del workspace pasa con warnings tratados como errores.",
        timeout=600,
    )
    ev.run(
        "P072-UNIT-contract-schemas",
        "UNIT",
        [sys.executable, "tools/p072_schema_probe.py"],
        "Ambos schemas cumplen Draft 2020-12, las referencias P072 resuelven al contrato público y invoke/salida pasan validación.",
        fixtures=[
            REPO / "docs/contracts/schemas/larm.schema.json",
            REPO / "adapters/runtime/diffusers/protocol.schema.json",
            REPO / "tools/p072_schema_probe.py",
        ],
    )
    ev.run(
        "P072-REAL-host-refuses-unmanaged-python",
        "REAL",
        [sys.executable, "tools/p072_host_probe.py"],
        "El Python de herramientas no se acepta como runtime y el probe no importa Torch ni Diffusers.",
        ok=exit_zero,
        timeout=30,
        env={
            "LARM_PYTHON_ROOT": "",
            "LARM_DIFFUSERS_MODEL_ROOT": "",
            "LARM_DIFFUSERS_MODEL_PATH": "",
            "LARM_DIFFUSERS_OUTPUT_ROOT": "",
        },
    )
    ev.blocked(
        "P072-REAL-local-diffusion-inference",
        "REAL",
        "Crear un runtime gestionado con lock P099, cargar un bundle tiny safetensors local en CPU y registrar imagen/hash/tiempo/memoria/cancelación",
        "La inferencia genera un PNG local validado por el contrato, mide segundos por imagen y carga, informa memoria real si el backend la expone y cancela sin artefactos parciales.",
        "El host no dispone de un runtime Diffusers gestionado ni de un bundle tiny autorizado con pesos reales. El profile lock y los comandos deben fijarse en P099; el fixture de esta suite usa bytes y pipeline simulados.",
    )
    ev.blocked(
        "P072-REAL-full-quality-gate",
        "REAL",
        "tools/check.ps1 -Prompt P072 -Real -SkipFrontend -SkipDesktop",
        "La puerta común completa su suite workspace, arquitectura, copias contractuales y evidencia P072.",
        "No se repite la suite workspace en esta fase: la puerta oficial P071 quedó sin resultado tras más de diez minutos en cargo_test y P072 no modifica Rust, UI ni contratos canónicos. No se afirma PASS ni FAIL para la puerta común.",
    )

    blockers = [
        "P099 debe cualificar versiones de Python/Diffusers/Torch, wheels, estrategia de entorno y comandos observados antes de instalar o anunciar el runtime.",
        "P103 debe alojar el worker en su ciclo protocol.invoke, job durable, progreso, cancelación, recuperación y cierre de proceso; sidecar.py sólo aporta hooks internos.",
        "P090 debe emitir la huella de ejecución y autorizar admisión antes de la primera inferencia.",
        "El worker no implementa aprobación REMOTE_CODE; pipelines y componentes personalizados permanecen bloqueados hasta que el flujo de aprobación cualificado esté integrado.",
    ]
    changed_files = [
        "adapters/runtime/diffusers/README.md",
        "adapters/runtime/diffusers/protocol.schema.json",
        "adapters/runtime/diffusers/sidecar.py",
        "adapters/runtime/diffusers/worker.py",
        "adapters/runtime/diffusers/tests/test_worker.py",
        "docs/adapters/diffusers.md",
        "docs/implementation/STATE.md",
        "docs/reports/P072_diffusers-pytorch-runtime.md",
        "tools/p072_evidence.py",
        "tools/p072_host_probe.py",
        "tools/p072_schema_probe.py",
    ]
    status = ev.finish(changed_files=changed_files, blockers=blockers)
    evidence = json.loads((REPO / "docs/reports/P072_diffusers-pytorch-runtime.evidence.json").read_text(encoding="utf-8"))
    counts = {name: sum(test["result"] == name for test in evidence["tests"]) for name in ("PASS", "FAIL", "BLOCKED", "INAPPLICABLE")}
    report = f"""# P072 — Diffusers/PyTorch runtime

Fecha: {evidence['generated_at'][:10]}  
Estado: {status}

P072 prepara un worker privado para `image.generate`. El plan de entorno conserva la identidad runtime/versión/backend/Python/hash de lock de P071, por lo que locks de Diffusers diferentes derivan carpetas distintas. El worker no instala paquetes. La generación exige el intérprete gestionado y un profile lock cualificado por P099.

El inspector acepta únicamente `StableDiffusionPipeline`, `StableDiffusionXLPipeline` y `FluxPipeline`, valida cada componente antes de importar Torch/Diffusers y requiere pesos safetensors. El bundle representa UNet/transformer con el rol contractual `weights` y `facts.diffusers_component`; conserva `vae`, `text_encoder`, `tokenizer`, `config`, `processor` y `diffusers_index` para los demás artefactos. `auto_map`, clases personalizadas y módulos externos fallan con `REMOTE_CODE_REQUIRED`. La carga fija `local_files_only=true`, `use_safetensors=true`, `trust_remote_code=false` y backend explícito sin fallback.

`sidecar.py` aporta hooks internos para `protocol.invoke`, progreso por fase, cancelación y heartbeat a P103. No abre un puerto ni crea cola durable. Una inferencia requiere además la huella P090 y escribe un PNG dentro del root autorizado con hash SHA-256. La metadata local registra pipeline, hash del índice, backend, versiones, dimensiones, steps, seed, tiempo de carga, segundos por imagen y memoria pico; CPU o backends sin medidor quedan como desconocidos. El fixture diminuto usa archivos y pipeline simulados, no pesos reales.

Verificación focal: {counts['PASS']} PASS, {counts['FAIL']} FAIL, {counts['BLOCKED']} BLOCKED. Las 12 pruebas cubren validación de bundle, roles, rechazo de código remoto, loader offline/safetensors, salida de imagen contractual, progreso, cancelación y limpieza ante fallos. También pasan formato Rust y Clippy del workspace. La prueba de contrato resuelve las referencias privadas contra el schema público y valida `protocol.invoke` y la salida; el probe Windows rechaza el Python de herramientas sin importar Torch/Diffusers.

La aceptación REAL sigue bloqueada por P071, P036, P103, P099 y P090 sin `REAL_PASS`, y por la falta de runtime/modelo local autorizado. La puerta común no se repitió porque su `cargo_test` de P071 no terminó tras más de diez minutos; no hay resultado oficial de P072. La documentación oficial confirma que `local_files_only` evita descargar desde Hub y que `trust_remote_code=true` puede ejecutar código de pipelines comunitarios [Diffusers loading](https://huggingface.co/docs/diffusers/main/api/diffusion_pipeline), [community pipelines](https://huggingface.co/docs/diffusers/main/using-diffusers/custom_pipeline_overview). P072 permanece `BLOCKED_REAL` y no se anuncia como adaptador disponible.
"""
    (REPO / "docs/reports/P072_diffusers-pytorch-runtime.md").write_text(report, encoding="utf-8")
    return 1 if status == "BLOCKED" else 0


if __name__ == "__main__":
    raise SystemExit(main())
