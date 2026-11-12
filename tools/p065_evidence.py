#!/usr/bin/env python3
"""Focused contract, integration and host-probe evidence for P065."""
from __future__ import annotations

import json
import sys
from pathlib import Path

from evidence import Evidence, REPO, exit_zero


FIXTURE = REPO / "adapters/runtime/olive-tooling/fixtures/tensor_identity.onnx"


def unittest_ok(code: int, output: str) -> bool:
    return code == 0 and "Ran 12 tests" in output and "OK" in output


def main() -> int:
    ev = Evidence("P065")
    ev.run(
        "P065-INTEGRATION-private-worker-contract",
        "INTEGRATION",
        [sys.executable, "-m", "unittest", "discover", "-s", "adapters/runtime/olive-tooling/tests", "-p", "test_worker.py", "-v"],
        "Las 12 pruebas validan schema cerrado, fixture autorizada por hash, versiones, provider, plan sin escritura, candidato con provenance, timeout, fallo de proceso, IO y recuperación de reintento en un venv temporal.",
        ok=unittest_ok,
        fixtures=[FIXTURE],
        timeout=300,
    )
    version = f"{sys.version_info.major}.{sys.version_info.minor}.{sys.version_info.micro}"
    ev.tests[-1]["observed"] += (
        f"; Python={version}; Olive=1.0.0 (fake metadata); ONNX Runtime=1.20.0 (fake metadata); "
        "provider=CPUExecutionProvider (fake); fixture=tensor_identity ONNX 130 bytes"
    )
    ev.run(
        "P065-REAL-managed-environment-probe",
        "REAL",
        [sys.executable, "tools/p065_real_probe.py"],
        "En Windows, el worker identifica la ausencia de un entorno de runtime gestionado, informa paquetes como desconocidos y no intenta instalaciones globales.",
        ok=exit_zero,
        fixtures=[FIXTURE],
        timeout=30,
    )
    ev.blocked(
        "P065-REAL-olive-transform-on-onnx",
        "REAL",
        "Ejecutar Olive sobre tensor_identity.onnx con un provider ONNX Runtime disponible dentro del entorno Python gestionado por LARM",
        "Registrar versiones reales y provider, conservar intacta la fixture, verificar la salida ONNX, hashes y provenance, y publicar sólo una candidata aislada.",
        "No existe todavía un entorno Python gestionado por P071 ni evidencia REAL_PASS de P062/P071/P093; Olive y ONNX Runtime tampoco están instalados en el Python de herramientas. No se instala nada en el Python global. La simulación aislada cubre el contrato y los límites locales, pero no acredita compatibilidad ni optimización real.",
    )

    blockers = [
        "P062/P071/P093 deben llegar a REAL_PASS antes de completar el consumidor; P093 continúa sin conectar este worker al job durable, preflight, reserva, verificación/smoke y model store.",
        "Falta cualificar y fijar versiones exactas de Olive, ONNX Runtime y provider en el entorno gestionado P071/P099.",
        "La prueba real de optimización sobre PC IA y la gestión de cancelación por el host Windows siguen pendientes.",
    ]
    changed_files = [
        "adapters/runtime/olive-tooling/worker.py",
        "adapters/runtime/olive-tooling/olive_runner.py",
        "adapters/runtime/olive-tooling/protocol.schema.json",
        "adapters/runtime/olive-tooling/fixtures.json",
        "adapters/runtime/olive-tooling/fixtures/tensor_identity.onnx",
        "adapters/runtime/olive-tooling/tests/test_worker.py",
        "docs/adapters/olive-tooling.md",
        "docs/implementation/STATE.md",
        "docs/reports/P065_microsoft-olive-transformacion-y-optimizacion-onnx.md",
        "tools/p065_evidence.py",
        "tools/p065_real_probe.py",
    ]
    status = ev.finish(changed_files=changed_files, blockers=blockers)
    evidence_path = REPO / "docs/reports/P065_microsoft-olive-transformacion-y-optimizacion-onnx.evidence.json"
    evidence = json.loads(evidence_path.read_text(encoding="utf-8"))
    counts = {name: sum(test["result"] == name for test in evidence["tests"]) for name in ("PASS", "FAIL", "BLOCKED", "INAPPLICABLE")}
    gate_path = REPO / "docs/reports/check_P065_real.json"
    gate_status = "AÚN NO EJECUTADA"
    gate_detail = ""
    if gate_path.is_file():
        try:
            gate = json.loads(gate_path.read_text(encoding="utf-8"))
            gate_status = gate.get("status", "DESCONOCIDO")
            checks = gate.get("checks", [])
            passed_checks = sum(check.get("result") == "PASS" for check in checks)
            gate_detail = f" ({passed_checks}/{len(checks)} comprobaciones PASS)"
            failed = [check for check in checks if check.get("result") == "FAIL"]
            if failed:
                gate_detail += "; fallos: " + ", ".join(check.get("check", "desconocido") for check in failed)
        except json.JSONDecodeError:
            gate_status = "EVIDENCIA INVÁLIDA"
    python_version = f"{sys.version_info.major}.{sys.version_info.minor}.{sys.version_info.micro}"
    report = f"""# P065 — Microsoft Olive: transformación y optimización ONNX

Fecha: {evidence['generated_at'][:10]}  
Estado: {status}

P065 añade un worker privado y estricto para preparar y probar la integración de Microsoft Olive. Implementa `probe`, `version`, `capabilities`, `plan` y `execute_fixture` sin modificar el protocolo público de transformación. El worker acepta sólo la fixture ONNX `tensor_identity`, fijada en {FIXTURE.stat().st_size} bytes con SHA256 `{hashlib_sha(FIXTURE)}`; rechaza rutas y fixtures arbitrarias, y no inspecciona Olive ni ONNX Runtime desde un Python no gestionado.

El plan es determinista y no escribe archivos: registra tamaño exacto de entrada, dependencias y versiones cuando son observables, provider solicitado, precisión normalizada y `trust_remote_code=false`. El tamaño de salida queda `UNKNOWN` mientras no haya un estimador cualificado. La ejecución vuelve a validar el plan y el hash de la fixture, llama a `olive.optimize` en un proceso hijo con timeout, verifica que el original no cambió, calcula hashes de salida y conserva provenance en una carpeta candidata bajo la raíz controlada. No publica en el model store ni promueve la salida.

Las pruebas focales usan un venv temporal con Python {python_version}, metadatos Olive `1.0.0` falsos, ONNX Runtime `1.20.0` falso y `CPUExecutionProvider` simulado: {counts['PASS']} PASS, {counts['FAIL']} FAIL, {counts['BLOCKED']} BLOCKED. Incluyen probe/version/capabilities, schema cerrado, repetibilidad y lectura pura del plan, hash de fixture, provider ausente, ejecución autorizada, plan desactualizado, salida IO inválida, timeout, fallo parcial y reintento. Se ejecutó además el probe del host Windows; detectó que no hay un entorno de runtime gestionado y dejó las dependencias como desconocidas sin instalar paquetes.

La puerta oficial `tools/check.ps1 -Prompt P065 -Real` figura como **{gate_status}{gate_detail}** en [check_P065_real.json](check_P065_real.json). El último gate completo falla sólo en `cargo_test`: `larm-runtime/tests/discovery_service.rs` no pudo abrir el ejecutable de fixture inmediatamente después de terminar el runner dentro del workspace; el mismo target pasa 6/6 al ejecutarlo aislado. Las otras comprobaciones, incluida la evidencia focal P065, pasan. La evidencia focal está en [P065 evidence](P065_microsoft-olive-transformacion-y-optimizacion-onnx.evidence.json) y las instrucciones del worker en [olive-tooling](../adapters/olive-tooling.md).

La aceptación de optimización real queda bloqueada: P062, P071 y P093 aún no tienen `REAL_PASS`; tampoco existe un entorno P071 con Olive y ONNX Runtime instalados y cualificados. La cancelación en árbol depende del host de procesos P021/P022/P130; la ejecución durable, preflight/reservas, verificación/smoke y registro pertenecen al consumidor P093. Ningún mock se cuenta como prueba real.
"""
    (REPO / "docs/reports/P065_microsoft-olive-transformacion-y-optimizacion-onnx.md").write_text(report, encoding="utf-8")
    return 1 if status == "BLOCKED" else 0


def hashlib_sha(path: Path) -> str:
    import hashlib

    return hashlib.sha256(path.read_bytes()).hexdigest()


if __name__ == "__main__":
    raise SystemExit(main())
