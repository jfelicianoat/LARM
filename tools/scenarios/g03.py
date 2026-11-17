#!/usr/bin/env python3
"""Escenario global G03 — Storage (P142, contracts/acceptance.json).

Propietarios: P015, P052, P122
Procedimiento: Abrir download preflight conocido y desconocido; intentar confirmar sin destino/upper bound y después con presupuesto duro.
Oráculo: Bytes/confianza/margen/reserva visibles antes del job; unknown sin budget bloquea; upper bound respeta cuota.

El runner reporta el estado ACTUAL de los propietarios desde los evidence.json
y sale 0 sólo si todos están REAL_PASS. La ejecución real del procedimiento
pertenece a cada prompt propietario (ya probada y sellada allí); este runner
no la duplica ni confunde CI o mocks con el entorno del PC IA.
"""
import json
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
ACCEPTANCE = REPO.parent / "LARM_prompts_v3" / "contracts" / "acceptance.json"
REPORTS = REPO / "docs" / "reports"
SEALED = ("CODE_PASS", "REAL_PASS", "BLOCKED_REAL", "BLOCKED")
ORDER = {"NOT_STARTED": 0, "BLOCKED": 1, "BLOCKED_REAL": 2, "CODE_PASS": 3, "REAL_PASS": 4}


def status_of(prompt_id: str) -> tuple[str, str]:
    for path in sorted(REPORTS.glob(f"{prompt_id}_*.evidence.json")):
        document = json.loads(path.read_text(encoding="utf-8"))
        return document.get("status", "UNKNOWN"), path.name
    return "NOT_STARTED", "-"


def main() -> int:
    acceptance = json.loads(ACCEPTANCE.read_text(encoding="utf-8"))
    scenario = next(s for s in acceptance["global_scenarios"] if s["id"] == 'G03')
    print(f"Escenario {scenario['id']} — {scenario['name']}")
    print(f"Procedimiento: {scenario.get('procedure', '')}")
    print(f"Oráculo: {scenario.get('oracle', '')}")
    owners = sorted(scenario.get("owners", []))
    statuses = []
    for owner in owners:
        status, source = status_of(owner)
        statuses.append(status)
        print(f"  {owner}: {status} ({source})")
    if not statuses:
        print("BLOCKED: el escenario no tiene prompts propietarios declarados")
        return 2
    worst = min(statuses, key=lambda s: ORDER.get(s, -1))
    if worst != "REAL_PASS":
        print(f"BLOCKED: el escenario {scenario['id']} requiere todos sus propietarios en REAL_PASS; el peor estado actual es {worst}")
        return 2
    print(f"PASS: escenario {scenario['id']} con todos sus propietarios en REAL_PASS")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
