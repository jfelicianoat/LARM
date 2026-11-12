#!/usr/bin/env python3
"""Focused contract and host-probe evidence for P071."""
from __future__ import annotations

import json
import sys
from pathlib import Path

from evidence import Evidence, REPO, exit_zero


def unittest_ok(code: int, output: str) -> bool:
    return code == 0 and "Ran 13 tests" in output and "OK" in output


def main() -> int:
    ev = Evidence("P071")
    ev.run(
        "P071-INTEGRATION-transformers-worker-and-sidecar",
        "INTEGRATION",
        [sys.executable, "-m", "unittest", "discover", "-s", "adapters/runtime/transformers/tests", "-p", "test_worker.py", "-v"],
        "13 pruebas cubren identidad de entorno, intérprete contenido en el prefijo exacto, estrategias declaradas, aislamiento, lectura local de modelos, rechazo de remote code, backend explícito e invariantes del subset OpenAI.",
        ok=unittest_ok,
        timeout=180,
    )
    ev.run(
        "P071-UNIT-python-environment-identity",
        "UNIT",
        ["cargo", "test", "-p", "larm-runtime", "python_env", "--", "--nocapture"],
        "Las cinco pruebas Rust verifican hash de identidad, equivalencia del digest Python/Rust, lock/Python, ruta dentro del runtime store e intérprete gestionado.",
        timeout=600,
    )
    schema_check = (
        "import json,jsonschema,pathlib; "
        "p=pathlib.Path('adapters/runtime/transformers/protocol.schema.json'); "
        "s=json.loads(p.read_text(encoding='utf-8')); "
        "jsonschema.Draft202012Validator.check_schema(s); print('schema Draft 2020-12 válido')"
    )
    ev.run(
        "P071-UNIT-private-protocol-schema",
        "UNIT",
        [sys.executable, "-c", schema_check],
        "El schema privado cumple Draft 2020-12.",
        fixtures=[REPO / "adapters/runtime/transformers/protocol.schema.json"],
    )
    ev.run(
        "P071-REAL-host-refuses-global-python",
        "REAL",
        [sys.executable, "tools/p071_host_probe.py"],
        "El probe del host rechaza el Python de herramientas como entorno runtime y no importa PyTorch/Transformers fuera del prefijo gestionado.",
        ok=exit_zero,
        timeout=30,
        env={"LARM_PYTHON_ROOT": "", "LARM_TRANSFORMERS_MODEL_ROOT": ""},
    )
    ev.blocked(
        "P071-REAL-isolated-runtime-and-tiny-model",
        "REAL",
        "Crear un entorno runtime con la estrategia UV y CONDA/MAMBA fijada por P099; ejecutar un modelo tiny safetensors en CPU y verificar generación, cierre del proceso y ausencia de acceso global",
        "Registrar versiones exactas de Python/Torch/Transformers, lock, provider efectivo y salida de inferencia; validar que REMOTE_CODE_REQUIRED bloquea el modelo personalizado.",
        "P031/P022/P040 no tienen REAL_PASS; P099 todavía no aporta comandos y dependencias runtime cualificados para P071. En el host no hay entorno Python gestionado ni un modelo local tiny autorizado. Los tests mock validan controles y no sustituyen la ejecución real.",
    )

    blockers = [
        "P031/P022/P040 deben llegar a REAL_PASS antes de conectar el worker al supervisor, permisos y cancelación de proceso Windows.",
        "P099 debe fijar versión de Python, comandos observados, wheels/paquetes y profile locks reales para UV y CONDA/MAMBA; el worker no improvisa flags ni instala paquetes.",
        "Pytorch/Transformers aún no se ejecuta en el runtime store ni está cableado al ciclo durable de instalación/launch; el sidecar sólo se enlaza a loopback con token del host.",
        "La puerta oficial quedó sin resultado: cargo_test del workspace no cerró tras más de diez minutos y se interrumpió; no se afirma PASS ni FAIL para esa suite.",
    ]
    changed_files = [
        "crates/larm-runtime/src/lib.rs",
        "crates/larm-runtime/src/python_env.rs",
        "adapters/runtime/transformers/README.md",
        "adapters/runtime/transformers/protocol.schema.json",
        "adapters/runtime/transformers/sidecar.py",
        "adapters/runtime/transformers/worker.py",
        "adapters/runtime/transformers/tests/test_worker.py",
        "docs/adapters/transformers.md",
        "docs/implementation/STATE.md",
        "docs/reports/P071_python-environment-manager-y-pytorch-transformers.md",
        "tools/p071_evidence.py",
        "tools/p071_host_probe.py",
    ]
    status = ev.finish(changed_files=changed_files, blockers=blockers)
    evidence = json.loads((REPO / "docs/reports/P071_python-environment-manager-y-pytorch-transformers.evidence.json").read_text(encoding="utf-8"))
    counts = {name: sum(test["result"] == name for test in evidence["tests"]) for name in ("PASS", "FAIL", "BLOCKED", "INAPPLICABLE")}
    gate_path = REPO / "docs/reports/check_P071_real.json"
    gate_status, gate_detail = "INTERRUMPIDA", " (cargo_test workspace no cerró tras más de diez minutos; no hay resultado oficial)"
    if gate_path.is_file():
        try:
            gate = json.loads(gate_path.read_text(encoding="utf-8"))
            gate_status = gate.get("status", "DESCONOCIDO")
            checks = gate.get("checks", [])
            gate_detail = f" ({sum(check.get('result') == 'PASS' for check in checks)}/{len(checks)} comprobaciones PASS)"
            failed = [check.get("check", "desconocido") for check in checks if check.get("result") == "FAIL"]
            if failed:
                gate_detail += "; fallos: " + ", ".join(failed)
        except json.JSONDecodeError:
            gate_status = "EVIDENCIA INVÁLIDA"
    gate_reference = "[check_P071_real.json](check_P071_real.json)" if gate_path.is_file() else "no se generó `check_P071_real.json`"
    report = f"""# P071 — Python Environment Manager y PyTorch/Transformers

Fecha: {evidence['generated_at'][:10]}  
Estado: {status}

P071 prepara la identidad y el worker privado del runtime Python. `PythonEnvironmentIdentity` incluye runtime, versión, backend, versión exacta de Python y hash SHA256 del lock; el core deriva un path dentro del runtime store sin aceptar destinos del caller. El worker sólo planea entornos, exige que la estrategia solicitada esté anunciada y no lanza instalaciones por su cuenta. Los comandos deben provenir de profile locks cualificados por P099.

El probe carga PyTorch/Transformers sólo si `sys.prefix` coincide con `LARM_PYTHON_ROOT` y `sys.executable` está dentro del mismo prefijo; rechaza intérpretes enlazados o junctions. La inspección se limita a modelos locales dentro de `LARM_TRANSFORMERS_MODEL_ROOT`. La inferencia fija backend, `local_files_only`, `use_safetensors` y `trust_remote_code=false`, y devuelve `REMOTE_CODE_REQUIRED` antes de importar Transformers si hay `auto_map`. El sidecar define un subset OpenAI de texto autenticado, sin streaming/herramientas, y sólo puede enlazar loopback. El worker no lo anuncia ni lo inicia antes de que el host P022/P040 lo cualifique.

Verificación focal: {counts['PASS']} PASS, {counts['FAIL']} FAIL, {counts['BLOCKED']} BLOCKED. Las 13 pruebas Python usan módulos simulados para probar controles; las 5 pruebas Rust cubren identidad, hashing compartido con el worker, store path y validación. El probe real del host confirma que el Python de herramientas no se acepta como runtime. No se instaló PyTorch, Transformers ni ningún paquete en Python global.

La puerta `tools/check.ps1 -Prompt P071 -Real` figura como **{gate_status}{gate_detail}**; {gate_reference}. La inferencia tiny real y las estrategias UV/CONDA/MAMBA siguen bloqueadas porque P031/P022/P040 no tienen `REAL_PASS`, P099 no ha cualificado comandos y locks del entorno, y el host no cuenta con runtime gestionado ni modelo local autorizado. El trabajo no se anuncia como adaptador listo para producción.
"""
    (REPO / "docs/reports/P071_python-environment-manager-y-pytorch-transformers.md").write_text(report, encoding="utf-8")
    return 1 if status == "BLOCKED" else 0


if __name__ == "__main__":
    raise SystemExit(main())
