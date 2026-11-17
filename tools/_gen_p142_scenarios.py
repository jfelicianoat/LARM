# -*- coding: utf-8 -*-
"""Genera los 15 runners de escenario global (tools/scenarios/gNN.py) desde
contracts/acceptance.json. Cada runner es determinista: reporta el escenario,
sus prompts propietarios con estado actual y sale 0 sólo si todos están en
REAL_PASS (la aceptación real del propio escenario corresponde a su prompt
propietario, ya probada allí)."""
import json
import io
from pathlib import Path

REPO = Path(r"C:\Procesos\Local AI Runtime Manager — LARM\larm")
ACCEPTANCE = REPO.parent / "LARM_prompts_v3" / "contracts" / "acceptance.json"
OUT = REPO / "tools" / "scenarios"
REPORTS = REPO / "docs" / "reports"

TEMPLATE = '''#!/usr/bin/env python3
"""Escenario global {sid} — {name} (P142, contracts/acceptance.json).

Propietarios: {owners}
Procedimiento: {procedure}
Oráculo: {oracle}

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
ORDER = {{"NOT_STARTED": 0, "BLOCKED": 1, "BLOCKED_REAL": 2, "CODE_PASS": 3, "REAL_PASS": 4}}


def status_of(prompt_id: str) -> tuple[str, str]:
    for path in sorted(REPORTS.glob(f"{{prompt_id}}_*.evidence.json")):
        document = json.loads(path.read_text(encoding="utf-8"))
        return document.get("status", "UNKNOWN"), path.name
    return "NOT_STARTED", "-"


def main() -> int:
    acceptance = json.loads(ACCEPTANCE.read_text(encoding="utf-8"))
    scenario = next(s for s in acceptance["global_scenarios"] if s["id"] == {sid!r})
    print(f"Escenario {{scenario['id']}} — {{scenario['name']}}")
    print(f"Procedimiento: {{scenario.get('procedure', '')}}")
    print(f"Oráculo: {{scenario.get('oracle', '')}}")
    owners = sorted(scenario.get("owners", []))
    statuses = []
    for owner in owners:
        status, source = status_of(owner)
        statuses.append(status)
        print(f"  {{owner}}: {{status}} ({{source}})")
    if not statuses:
        print("BLOCKED: el escenario no tiene prompts propietarios declarados")
        return 2
    worst = min(statuses, key=lambda s: ORDER.get(s, -1))
    if worst != "REAL_PASS":
        print(f"BLOCKED: el escenario {{scenario['id']}} requiere todos sus propietarios en REAL_PASS; el peor estado actual es {{worst}}")
        return 2
    print(f"PASS: escenario {{scenario['id']}} con todos sus propietarios en REAL_PASS")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
'''

acceptance = json.loads(ACCEPTANCE.read_text(encoding="utf-8"))
OUT.mkdir(exist_ok=True)
for scenario in acceptance["global_scenarios"]:
    sid = scenario["id"]
    runner = OUT / f"{sid.lower()}.py"
    text = TEMPLATE.format(
        sid=sid,
        name=scenario.get("name", ""),
        owners=", ".join(sorted(scenario.get("owners", []))),
        procedure=(scenario.get("procedure", "") or "").replace("\n", " "),
        oracle=(scenario.get("oracle", "") or "").replace("\n", " "),
    )
    runner.write_text(text, encoding="utf-8", newline="\n")
    print("generado", runner.name)
