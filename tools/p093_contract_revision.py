"""Publish the explicitly approved P093 restart contract, preserving its predecessor."""
from datetime import datetime, timezone
import hashlib
import json
from pathlib import Path
import subprocess
import sys

REPO=Path(__file__).resolve().parents[1]
PACK=REPO.parent/"LARM_prompts_v3"
BACKUP=REPO/"docs/reports/P093_runs/revision-3-13-0/predecessor"
REVISION="contracts/REVISION_3_13_0_P093.md"

def write(path,value):
    path.write_text(json.dumps(value,ensure_ascii=False,indent=2)+"\n",encoding="utf-8",newline="\n")

def apply():
    manifest=json.loads((PACK/"manifest.json").read_bytes())
    assert manifest["package_version"]=="3.12.1", "only the reviewed predecessor may be changed"
    for item in manifest["normative_files"]:
        assert hashlib.sha256((PACK/item["file"]).read_bytes()).hexdigest()==item["sha256"],item["file"]
    assert not BACKUP.exists() and not (PACK/REVISION).exists(), "retain earlier publication attempts"
    names=["manifest.json","contracts/api-operations.json","contracts/00_PACKAGE.md",
           "contracts/07_API_EVENTS_MCP.md","contracts/11_CONFORMIDAD_V3.md"]
    for name in names:
        target=BACKUP/name; target.parent.mkdir(parents=True,exist_ok=True)
        target.write_bytes((PACK/name).read_bytes())
    proposal=(REPO/"docs/reports/P093_revision_3_13_0_propuesta.md").read_text(encoding="utf-8")
    revision=proposal.replace("# Propuesta 3.13.0 — reinicio explícito de transformaciones",
                              "# Revisión contractual 3.13.0 — reinicio explícito de transformaciones",1)
    revision=revision.replace("Fecha: 2026-10-30. Estado: PROPUESTA; no publicada ni implementada como contrato vigente.",
        "Fecha: 2026-10-30. Predecesora:3.12.1. Publicación e implementación autorizadas expresamente por el operador al responder «Autorizar revisión e implementación» a la propuesta3.13.0. Contrato normativo; no acredita por sí mismo implementación ni REAL_PASS.",1)
    revision=revision.replace("## Cambio concreto propuesto","## Operación y reglas normativas",1)
    revision=revision.replace("Añadir una operación, reutilizando los DTOs existentes:","Se añade una operación, reutilizando los DTOs existentes:",1)
    revision=revision.replace("## Publicación e implementación si se autoriza","## Alcance de implementación y puertas conservadas",1)
    (PACK/REVISION).write_text(revision,encoding="utf-8",newline="\n")
    api_path=PACK/"contracts/api-operations.json"
    api=json.loads(api_path.read_bytes())
    operation={"id":"P093_post__transformations_id_restart","method":"POST",
        "path":"/api/v1/transformations/{id}/restart","scope":"jobs:write",
        "request_schema":"larm.schema.json#/$defs/ExecutePlan",
        "response_schema":"larm.schema.json#/$defs/JobReceipt","owner":"P093","mode":"job",
        "success_status":202,"path_parameters":{"id":{"type":"string",
        "pattern":"^[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$"}},
        "required_headers":["Idempotency-Key","If-Match"]}
    assert not any(op["id"]==operation["id"] for op in api["operations"])
    index=next(i for i,op in enumerate(api["operations"]) if op["id"]=="P093_post__transformations")
    api["operations"].insert(index+1,operation); write(api_path,api)
    note="\n\n## Revisión 3.13.0 — reinicio explícito de transformaciones\n\n"
    note+=("Autorizada por el operador el2026-10-30. `POST /api/v1/transformations/{id}/restart` identifica el trabajo anterior, exige `jobs:write`, `Idempotency-Key` e `If-Match`, y reutiliza `ExecutePlan`/`JobReceipt` con respuesta202. Sólo FAILED/FAILED_RECOVERABLE sin output publicado; input original íntegro, plan nuevo confirmado, cualificación/espacio/guards/aprobación actuales, CAS y un único incremento de attempt. El replay liga actor, job, cuerpo y revisión y conserva el recibo original aunque el trabajo avance. Historial y temporales se conservan con ownership; COMPLETED/CANCELLED siguen terminales. La aceptación exige los oráculos H07 y proceso real, sin sustituirlos por esquemas o mocks. [Reglas y oráculos completos](REVISION_3_13_0_P093.md).\n")
    for name in ["contracts/00_PACKAGE.md","contracts/07_API_EVENTS_MCP.md"]:
        path=PACK/name; path.write_text(path.read_text(encoding="utf-8").rstrip()+note,encoding="utf-8",newline="\n")
    path=PACK/"contracts/11_CONFORMIDAD_V3.md"
    content=path.read_text(encoding="utf-8")
    marker="Aceptación: crash injection, roundtrip del estado y evento, acción reiniciar con y sin input íntegro; hash del input preservado después del intento exitoso y del fallido."
    assert content.count(marker)==1
    content=content.replace(marker,"Revisión3.13.0: reinicio explícito del mismo job mediante `POST /api/v1/transformations/{id}/restart`, con plan nuevo confirmado, `If-Match` e idempotencia ligados al trabajo. El primer claim inicial cuenta intento1; aceptar un reinicio incrementa al siguiente intento y su claim no vuelve a incrementarlo. Conservar el intento anterior y probar rollback/concurrencia/replay, además del crash/reinicio real del daemon. [Contrato completo](REVISION_3_13_0_P093.md).\n\n"+marker)
    path.write_text(content,encoding="utf-8",newline="\n")
    manifest["package_version"]="3.13.0"; manifest["predecessor_package_version"]="3.12.1"
    manifest["generated_at_utc"]=datetime.now(timezone.utc).isoformat(timespec="seconds").replace("+00:00","Z")
    next(p for p in manifest["prompts"] if p["code"]=="P093")["contracts"].append(Path(REVISION).name)
    manifest["contract_revision_log"].append({"version":"3.13.0","predecessor":"3.12.1","date":"2026-10-30",
        "scope":"Explicitly authorized P093 same-job transformation restart with a new confirmed plan, CAS, durable idempotency and recovery oracles","audit":REVISION})
    entries={i["file"]:i for i in manifest["normative_files"]}
    for name in names[1:]+[REVISION]:
        data=(PACK/name).read_bytes(); entries[name]={"file":name,"bytes":len(data),"sha256":hashlib.sha256(data).hexdigest()}
    manifest["normative_files"]=[entries[key] for key in sorted(entries)]
    write(PACK/"manifest.json",manifest)
    return subprocess.call([sys.executable,"tools/package_copy.py","sync"],cwd=REPO)

if __name__=="__main__":
    assert sys.argv[1:]==["apply"], "Usage: p093_contract_revision.py apply"
    raise SystemExit(apply())
