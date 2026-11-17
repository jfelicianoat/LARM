"""Publish the explicitly approved P119 download-admission contract, preserving its predecessor."""
from datetime import datetime, timezone
import hashlib
import json
from pathlib import Path
import subprocess
import sys

REPO = Path(__file__).resolve().parents[1]
PACK = REPO.parent / "LARM_prompts_v3"
BACKUP = REPO / "docs/reports/P119_runs/revision-3-16-0/predecessor"
REVISION = "contracts/REVISION_3_16_0_P119.md"

def write(path, value):
    path.write_text(json.dumps(value, ensure_ascii=False, indent=2) + "\n", encoding="utf-8", newline="\n")

REVISION_TEXT = """# Revisión contractual 3.16.0 — admisión autorizada de descargas automáticas (P119)

Fecha: 2026-11-16. Predecesora:3.15.0. Publicación e implementación autorizadas expresamente por el operador al responder «Autorizar revisión e implementación» a la propuesta3.16.0. Contrato normativo; no acredita por sí mismo implementación ni REAL_PASS.

## Problema comprobado

La especificación §121/P119 exige descargas automáticas autorizadas: política opt-in del operador (allowlist fuente/capability/license/trust, cuota diaria de bytes, destino de almacenamiento, aprobación versionada), watcher que por defecto sólo notifica, auto-job con prioridad BACKGROUND que revalida contra la política vigente al activarse (suspende si cambia), historial y revocación inmediata de futuras admisiones. El watcher actual rechaza `policy.automatic_download=true` con `watch_auto_download_not_implemented`: no existe el modelo de política ni su historial ni las operaciones API, y las descargas automáticas no pueden entrar como BACKGROUND dentro del límite compartido de Settings sin evade los presupuestos.

Fuentes: `LARM_SPEC_V1.md` §121 y H08; `contracts/02_STORAGE_DOWNLOAD.md`; `contracts/07_API_EVENTS_MCP.md`; `contracts/api-operations.json`; `schemas/domain.schema.json`. El contrato00 exige resolver con el operador los cambios de alcance, conservando los requisitos.

## DTOs nuevos y reglas normativas

```json
{
  "download_admission_policy": {
    "enabled": false,
    "source_allowlist": ["source-uuid"],
    "capability_allowlist": ["capability-id"],
    "license_allowlist": ["license-id"],
    "min_trust": "CANDIDATE",
    "daily_quota_bytes": "1073741824",
    "storage_location_id": "uuid|null"
  }
}
```

- `min_trust` enumera los estados de `Trust` (CANDIDATE/TECHNICALLY_APPROVED/EVALUATION_APPROVED); la confianza de la variante debe alcanzarlo o superarlo.
- `download_admission`: registro inmutable del resultado con `{id, created_at, updated_at, row_version, metadata, variant_id, source_id, estimated_bytes, decision: ADMITTED|BLOCKED|REVOKED, reason, policy_row_version, day_key, job_id}`. `day_key` es la fecha UTC del intento (cuota diaria). Un registro REVOKED anula admisiones futuras del mismo `variant_id` mientras siga vigente.

Reglas:

1. **Opt-in**: sin política `enabled=true` ninguna detección produce descarga; el watcher sólo notifica.
2. **Evaluación determinista antes de planear**: source en allowlist, capability en allowlist, license en allowlist (o variant sin license si la allowlist está vacía), trust ≥ min_trust y cuota diaria no agotada; cualquier fallo produce admisión BLOCKED con motivo estable y SIN job.
3. **Auto-job BACKGROUND**: la descarga admitida entra como BACKGROUND (nunca evita presupuestos/allowlists eligiendo prioridad) dentro del límite compartido de Settings; reserva y revalidación de tamaño/trust/license/revisión se aplican igual que una descarga manual.
4. **Clausura de dependencias**: `REAL_PASS` exige que las dependencias del manifiesto estén en `REAL_PASS`; violación → `closure_incomplete::<id>`.
5. **Evidencia huérfana**: cualquier fichero `*.evidence.json` cuyo prompt no esté en `required_prompt_ids` falla (`evidence_orphan`).
6. **Evidencia ilegible**: JSON inválido → `evidence_unreadable`.

## Evidencia de implementación (anexa, no contractual)

- Dominio: evaluador puro con las seis reglas y unit tests por regla.
- DB: migración 0016 con tabla singleton de política versionada, historial inmutable con day_key, triggers de revocación y outbox; repositorio con journal/evento/outbox en una transacción.
- API: PUT/GET de política y GET/POST de admisiones con scopes contractuales.
- Watcher: detección → evaluación → BACKGROUND dentro de cuota; sin política sólo notifica.
- Real en PC IA: fuente controlada con revisión compatible → notificación sin descarga; cuota pequeña autorizada → admisión y bloqueo por cuota.
"""

def apply():
    manifest = json.loads((PACK / "manifest.json").read_bytes())
    assert manifest["package_version"] == "3.15.0", "only the reviewed predecessor may be changed"
    for item in manifest["normative_files"]:
        assert hashlib.sha256((PACK / item["file"]).read_bytes()).hexdigest() == item["sha256"], item["file"]
    assert not BACKUP.exists() and not (PACK / REVISION).exists(), "retain earlier publication attempts"
    names = ["manifest.json", "contracts/api-operations.json", "schemas/domain.schema.json", "schemas/larm.schema.json"]
    for name in names:
        target = BACKUP / name
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes((PACK / name).read_bytes())

    (PACK / REVISION).write_text(REVISION_TEXT, encoding="utf-8", newline="\n")

    api_path = PACK / "contracts/api-operations.json"
    api = json.loads(api_path.read_bytes())
    uuid7 = {"type": "string", "pattern": "^[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$"}
    uuid7_nullable = {"anyOf": [dict(uuid7), {"type": "null"}]}
    policy_operation = {
        "id": "P119_put__download_admission_policy",
        "method": "PUT",
        "path": "/api/v1/download-admission-policy",
        "scope": "jobs:write",
        "request_schema": "larm.schema.json#/$defs/download_admission_policy",
        "response_schema": "larm.schema.json#/$defs/download_admission_policy",
        "owner": "P119",
        "mode": "sync",
        "success_status": 200,
        "path_parameters": {},
        "required_headers": ["If-Match"],
    }
    policy_get = {
        "id": "P119_get__download_admission_policy",
        "method": "GET",
        "path": "/api/v1/download-admission-policy",
        "scope": "read",
        "request_schema": None,
        "response_schema": "larm.schema.json#/$defs/download_admission_policy",
        "owner": "P119",
        "mode": "sync",
        "success_status": 200,
        "path_parameters": {},
    }
    admissions_list = {
        "id": "P119_get__download_admissions",
        "method": "GET",
        "path": "/api/v1/download-admissions",
        "scope": "read",
        "request_schema": None,
        "response_schema": "larm.schema.json#/$defs/download_admissionList",
        "owner": "P119",
        "mode": "sync",
        "success_status": 200,
        "path_parameters": {},
    }
    revoke = {
        "id": "P119_post__download_admissions_id_revoke",
        "method": "POST",
        "path": "/api/v1/download-admissions/{id}/revoke",
        "scope": "jobs:write",
        "request_schema": None,
        "response_schema": "larm.schema.json#/$defs/download_admission",
        "owner": "P119",
        "mode": "sync",
        "success_status": 200,
        "path_parameters": {"id": uuid7},
    }
    assert not any(op["id"] == policy_operation["id"] for op in api["operations"])
    index = next(i for i, op in enumerate(api["operations"]) if op["id"] == "P052_post__download_plans")
    for offset, operation in enumerate([policy_operation, policy_get, admissions_list, revoke]):
        api["operations"].insert(index + offset, operation)
    write(api_path, api)

    domain_path = PACK / "schemas/domain.schema.json"
    domain = json.loads(domain_path.read_bytes())
    defs = domain["$defs"]
    assert "download_admission_policy" not in defs
    defs["download_admission_policy"] = {
        "type": "object",
        "additionalProperties": False,
        "properties": {
            "enabled": {"type": "boolean"},
            "source_allowlist": {"type": "array", "items": dict(uuid7), "uniqueItems": True},
            "capability_allowlist": {"type": "array", "items": {"type": "string", "minLength": 1}, "uniqueItems": True},
            "license_allowlist": {"type": "array", "items": {"type": "string", "minLength": 1}, "uniqueItems": True},
            "min_trust": {"type": "string", "enum": ["CANDIDATE", "TECHNICALLY_APPROVED", "EVALUATION_APPROVED"]},
            "daily_quota_bytes": {
                "anyOf": [
                    {"type": "string", "pattern": "^[0-9]+$"},
                    {"type": "null"},
                ]
            },
            "storage_location_id": dict(uuid7_nullable),
        },
        "required": ["enabled", "source_allowlist", "capability_allowlist", "license_allowlist",
                     "min_trust", "daily_quota_bytes", "storage_location_id"],
    }
    defs["download_admission"] = {
        "type": "object",
        "additionalProperties": False,
        "properties": {
            "id": {"type": "string", "pattern": "^[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$"},
            "created_at": {"type": "string", "pattern": "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}\\.\\d{3}Z$"},
            "updated_at": {"type": "string", "pattern": "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}\\.\\d{3}Z$"},
            "row_version": {"type": "integer", "minimum": 1},
            "metadata": {"type": "object"},
            "variant_id": dict(uuid7),
            "source_id": {"type": "string", "minLength": 1},
            "estimated_bytes": {"type": "string", "pattern": "^[0-9]+$"},
            "decision": {"type": "string", "enum": ["ADMITTED", "BLOCKED", "REVOKED"]},
            "reason": {"type": "string", "minLength": 1},
            "policy_row_version": {"type": "integer", "minimum": 1},
            "day_key": {"type": "string", "pattern": "^\\d{4}-\\d{2}-\\d{2}$"},
            "job_id": dict(uuid7_nullable),
        },
        "required": ["id", "created_at", "updated_at", "row_version", "metadata", "variant_id",
                     "source_id", "estimated_bytes", "decision", "reason", "policy_row_version",
                     "day_key", "job_id"],
    }
    write(domain_path, domain)

    larm_path = PACK / "schemas/larm.schema.json"
    larm = json.loads(larm_path.read_bytes())
    larm_defs = larm["$defs"]
    assert "download_admission_policy" not in larm_defs
    larm_defs["download_admission_policy"] = {"$ref": "domain.schema.json#/$defs/download_admission_policy"}
    larm_defs["download_admission"] = {"$ref": "domain.schema.json#/$defs/download_admission"}
    larm_defs["download_admissionList"] = {
        "type": "object",
        "additionalProperties": False,
        "properties": {
            "items": {"type": "array", "items": {"$ref": "domain.schema.json#/$defs/download_admission"}},
            "next_cursor": {"anyOf": [{"type": "string", "minLength": 1}, {"type": "null"}]},
            "snapshot_at": {"type": "string", "pattern": "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}\\.\\d{3}Z$"},
        },
        "required": ["items", "next_cursor", "snapshot_at"],
    }
    write(larm_path, larm)

    manifest["package_version"] = "3.16.0"
    manifest["predecessor_package_version"] = "3.15.0"
    manifest["generated_at_utc"] = datetime.now(timezone.utc).isoformat(timespec="seconds").replace("+00:00", "Z")
    next(p for p in manifest["prompts"] if p["code"] == "P119")["contracts"].append(Path(REVISION).name)
    manifest["contract_revision_log"].append({
        "version": "3.16.0", "predecessor": "3.15.0", "date": "2026-11-16",
        "scope": "P119 authorized automatic downloads: opt-in admission policy (source/capability/license/trust allowlists, daily byte quota, storage target), immutable admission history with day_key and revocation, watcher notify-only default and BACKGROUND auto-jobs",
        "audit": REVISION,
    })
    entries = {i["file"]: i for i in manifest["normative_files"]}
    for name in names[1:] + [REVISION]:
        data = (PACK / name).read_bytes()
        entries[name] = {"file": name, "bytes": len(data), "sha256": hashlib.sha256(data).hexdigest()}
    manifest["normative_files"] = [entries[key] for key in sorted(entries)]
    write(PACK / "manifest.json", manifest)
    return subprocess.call([sys.executable, "tools/package_copy.py", "sync"], cwd=REPO)

if __name__ == "__main__":
    assert sys.argv[1:] == ["apply"], "Usage: p119_contract_revision.py apply"
    raise SystemExit(apply())
