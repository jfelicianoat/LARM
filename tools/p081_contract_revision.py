"""Publish the explicitly approved P081 execution-host contract, preserving its predecessor."""
from datetime import datetime, timezone
import hashlib
import json
from pathlib import Path
import subprocess
import sys

REPO = Path(__file__).resolve().parents[1]
PACK = REPO.parent / "LARM_prompts_v3"
BACKUP = REPO / "docs/reports/P081_runs/revision-3-15-0/predecessor"
REVISION = "contracts/REVISION_3_15_0_P081.md"

def write(path, value):
    path.write_text(json.dumps(value, ensure_ascii=False, indent=2) + "\n", encoding="utf-8", newline="\n")

REVISION_TEXT = """# Revisión contractual 3.15.0 — hosts de ejecución (P081)

Fecha: 2026-11-14. Predecesora:3.14.0. Publicación e implementación autorizadas expresamente por el operador al responder «Autorizar revisión e implementación» a la propuesta3.15.0. Contrato normativo; no acredita por sí mismo implementación ni REAL_PASS.

## Problema comprobado

La especificación §5.1 exige distinguir cómo se ejecuta un runtime en Windows (`WINDOWS_NATIVE`, `WINDOWS_PYTHON`, `WINDOWS_ML`, `WSL2`, `CONTAINER`, `EXTERNAL_LOCAL`) y §52 declara las estrategias `WSL` y `CONTAINER` de instalación. El dominio ya tiene el enumerado `INSTALLATION_STRATEGIES` con esos valores, pero no existe un modelo de host de ejecución: la tabla `external_host` cubre sólo motores adoptados con `base_url` HTTP (`EXTERNAL_LOCAL`), un runtime Linux bajo WSL no puede referenciarse desde una instalación, y ninguna operación expone los motores virtualizados observados en el equipo. Un runtime WSL aparecería engañosamente como nativo.

Fuentes: `LARM_SPEC_V1.md` §5.1 y §52; `contracts/09_EXTERNAL_PROFILES.md`; `contracts/api-operations.json`; `schemas/domain.schema.json`, `runtime_installation`. El contrato00 exige resolver con el operador los cambios de alcance, conservando los requisitos.

## DTO nuevo y reglas normativas

Se añade el DTO `runtime_execution_host` (registro de motores virtualizados observados read-only) y el campo opcional `execution_host_id` en `runtime_installation`:

```json
{
  "runtime_execution_host": {
    "id": "uuid-v7",
    "created_at": "RFC3339", "updated_at": "RFC3339", "row_version": 1,
    "metadata": "object",
    "kind": "WSL2 | CONTAINER",
    "name": "nombre del distro/engine (1..256)",
    "engine_version": "string|null",
    "wsl_version": "1|2|null",
    "default": "boolean",
    "status": "ONLINE | OFFLINE | UNKNOWN",
    "evidence_refs": "array de strings"
  }
}
```

Reglas:

1. **Detección read-only**: la observación de WSL (`--status`, `--list --verbose`) y del motor de contenedores (`--version`, `info --format`) nunca instala distros, habilita features de Windows ni muta el estado global del motor. Todo comando de sondeo corre con timeout, kill al vencer, entorno mínimo sin credenciales y salida acotada; la ausencia de una herramienta es una observación válida, no un error.
2. **Plan de ejecución prohibido**: `wsl --install/--uninstall/--update/--set-*/--manage`, `dism`/`wusa`/`netsh`, habilitaciones por PowerShell y verbos de motor que mutan estado (`pull`/`push`/`build`/`save`/`load`/`import`/`commit`) o ejecutan con `--privileged`/`--network host` se rechazan con `execution_host_plan_mutates_host`.
3. **Coherencia instalación↔host**: estrategia `WSL` exige `execution_host_id` con host `WSL2`; `CONTAINER` exige host `CONTAINER`; las estrategias nativas exigen `execution_host_id` null. Violación → `execution_host_reference_missing` / `execution_host_strategy_mismatch`.
4. **Política de puertos**: toda exposición de puerto del huésped se declara explícita y bind sólo loopback (`execution_host_port_must_bind_loopback`); `0` (efímero) prohibido.
5. **Mapeo de rutas**: Windows↔WSL explícito bajo `\\\\wsl.localhost\\<distro>\\` (lectura tolera el legado `\\\\wsl$\\`), rutas absolutas y sin subida de directorio; montajes de contenedor con host drive y ruta absoluta POSIX.
6. **Operación de lectura**: `GET /api/v1/execution-hosts` (scope `read`) devuelve `runtime_execution_hostList` sin crear ni promover nada; el registro no acredita compatibilidad ni habilita ejecución.

## Evidencia de implementación (anexa, no contractual)

- Dominio: `ExecutionHostKind` con `allows_strategy`; validación instalación↔host 2×2.
- Hardware: sondeo read-only real (WSL2 Ubuntu v2 y Docker 29.7.2 en el PC IA), parsers tolerantes al locale y a UTF-16LE, ausencia de binarios como observación válida.
- Runtime: mapeo ida/vuelta WSL, política loopback, comando prohibido por clase, 11 pruebas.
- DB/API/UI: migración, repos con journal/evento/outbox, referencia en el DTO sellado, operación GET y pantalla de runtimes.
"""

def apply():
    manifest = json.loads((PACK / "manifest.json").read_bytes())
    assert manifest["package_version"] == "3.14.0", "only the reviewed predecessor may be changed"
    for item in manifest["normative_files"]:
        assert hashlib.sha256((PACK / item["file"]).read_bytes()).hexdigest() == item["sha256"], item["file"]
    assert not BACKUP.exists() and not (PACK / REVISION).exists(), "retain earlier publication attempts"
    names = ["manifest.json", "contracts/api-operations.json", "schemas/domain.schema.json", "schemas/larm.schema.json"]
    for name in names:
        target = BACKUP / name
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes((PACK / name).read_bytes())

    (PACK / REVISION).write_text(REVISION_TEXT, encoding="utf-8", newline="\n")

    # api-operations.json: la operación GET read-only del registro
    api_path = PACK / "contracts/api-operations.json"
    api = json.loads(api_path.read_bytes())
    operation = {
        "id": "P081_get__execution_hosts",
        "method": "GET",
        "path": "/api/v1/execution-hosts",
        "scope": "read",
        "request_schema": None,
        "response_schema": "larm.schema.json#/$defs/runtime_execution_hostList",
        "owner": "P081",
        "mode": "sync",
        "success_status": 200,
        "path_parameters": {},
    }
    assert not any(op["id"] == operation["id"] for op in api["operations"])
    index = next(i for i, op in enumerate(api["operations"]) if op["id"] == "P031_get__runtime_versions")
    api["operations"].insert(index, operation)
    write(api_path, api)

    # domain.schema.json: campo en runtime_installation + DTO nuevo
    domain_path = PACK / "schemas/domain.schema.json"
    domain = json.loads(domain_path.read_bytes())
    defs = domain["$defs"]
    installation = defs["runtime_installation"]
    uuid7 = {"anyOf": [
        {"type": "string", "pattern": "^[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$"},
        {"type": "null"},
    ]}
    assert "execution_host_id" not in installation["properties"]
    installation["properties"]["execution_host_id"] = uuid7
    installation["required"] = sorted(installation["required"] + ["execution_host_id"])
    assert "runtime_execution_host" not in defs
    defs["runtime_execution_host"] = {
        "type": "object",
        "additionalProperties": False,
        "properties": {
            "id": {"type": "string", "pattern": "^[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$"},
            "created_at": {"type": "string", "pattern": "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}\\.\\d{3}Z$"},
            "updated_at": {"type": "string", "pattern": "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}\\.\\d{3}Z$"},
            "row_version": {"type": "integer", "minimum": 1},
            "metadata": {"type": "object"},
            "kind": {"type": "string", "enum": ["WSL2", "CONTAINER"]},
            "name": {"type": "string", "minLength": 1, "maxLength": 256},
            "engine_version": {"anyOf": [{"type": "string", "minLength": 1}, {"type": "null"}]},
            "wsl_version": {"anyOf": [{"type": "integer", "enum": [1, 2]}, {"type": "null"}]},
            "default": {"type": "boolean"},
            "status": {"type": "string", "enum": ["ONLINE", "OFFLINE", "UNKNOWN"]},
            "evidence_refs": {"type": "array", "items": {"type": "string", "minLength": 1}},
        },
        "required": ["id", "created_at", "updated_at", "row_version", "metadata", "kind", "name",
                     "engine_version", "wsl_version", "default", "status", "evidence_refs"],
    }
    write(domain_path, domain)

    # larm.schema.json: refs del DTO y List, y el campo opcional del InstallRequest
    larm_path = PACK / "schemas/larm.schema.json"
    larm = json.loads(larm_path.read_bytes())
    larm_defs = larm["$defs"]
    assert "runtime_execution_host" not in larm_defs
    larm_defs["runtime_execution_host"] = {"$ref": "domain.schema.json#/$defs/runtime_execution_host"}
    larm_defs["runtime_execution_hostList"] = {
        "type": "object",
        "additionalProperties": False,
        "properties": {
            "items": {"type": "array", "items": {"$ref": "domain.schema.json#/$defs/runtime_execution_host"}},
            "next_cursor": {"anyOf": [{"type": "string", "minLength": 1}, {"type": "null"}]},
            "snapshot_at": {"type": "string", "pattern": "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}\\.\\d{3}Z$"},
        },
        "required": ["items", "next_cursor", "snapshot_at"],
    }
    install = larm_defs["InstallRequest"]
    assert "execution_host_id" not in install["properties"]
    install["properties"]["execution_host_id"] = uuid7
    write(larm_path, larm)

    # manifiesto: versión, log, contratos de P081 y hashes normativos
    manifest["package_version"] = "3.15.0"
    manifest["predecessor_package_version"] = "3.14.0"
    manifest["generated_at_utc"] = datetime.now(timezone.utc).isoformat(timespec="seconds").replace("+00:00", "Z")
    next(p for p in manifest["prompts"] if p["code"] == "P081")["contracts"].append(Path(REVISION).name)
    manifest["contract_revision_log"].append({
        "version": "3.15.0", "predecessor": "3.14.0", "date": "2026-11-14",
        "scope": "P081 execution hosts: read-only WSL2/container engine registry, execution_host_id reference on runtime_installation with strategy/host coherence, loopback-only port policy and forbidden host-mutating install plans",
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
    assert sys.argv[1:] == ["apply"], "Usage: p081_contract_revision.py apply"
    raise SystemExit(apply())
