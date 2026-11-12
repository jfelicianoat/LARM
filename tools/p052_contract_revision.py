#!/usr/bin/env python3
"""Apply and validate the reviewed LARM contract revision 3.1.0 for P052."""

from __future__ import annotations

import hashlib
import json
import subprocess
import sys
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

REPO = Path(__file__).resolve().parents[1]
WORKSPACE = REPO.parent
PACKAGE = WORKSPACE / "LARM_prompts_v3"
MIGRATION = "contracts/migrations/0006_p052_artifact_size_estimate.sql"
DECIMAL = {
    "type": "string",
    "pattern": "^(0|[1-9][0-9]*)$",
    "maxLength": 19,
    "x-maximum-decimal": "9223372036854775807",
}
UUID7 = {
    "type": "string",
    "pattern": "^[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$",
}
SHA256 = {"type": "string", "pattern": "^[0-9a-f]{64}$"}


def read_json(path: Path) -> dict[str, Any]:
    return json.loads(path.read_text(encoding="utf-8"))


def write_json(path: Path, value: Any) -> None:
    path.write_text(json.dumps(value, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")


def nullable(schema: dict[str, Any]) -> dict[str, Any]:
    return {"anyOf": [schema, {"type": "null"}]}


def add_published_local_deltas() -> None:
    """Carry forward the four implementation-side contract additions before syncing copies."""
    local = REPO / "docs/contracts"
    for rel, section, identity in [
        ("contracts/adapter-methods.json", "methods", "method"),
        ("contracts/api-operations.json", "operations", "id"),
    ]:
        source = read_json(PACKAGE / rel)
        implementation = read_json(local / Path(rel).name)
        known = {item[identity] for item in source[section]}
        source[section].extend(item for item in implementation[section] if item[identity] not in known)
        write_json(PACKAGE / rel, source)

    source_schema = read_json(PACKAGE / "schemas/larm.schema.json")
    implementation_schema = read_json(local / "schemas/larm.schema.json")
    for name in ("InstallStatusRequest", "RuntimeProfileKey", "RuntimeProfileLock"):
        if name not in source_schema["$defs"]:
            source_schema["$defs"][name] = implementation_schema["$defs"][name]
    state_enum = source_schema["$defs"]["JobReceipt"]["properties"]["state"]["enum"]
    if "CANCELLED" in implementation_schema["$defs"]["JobReceipt"]["properties"]["state"]["enum"]:
        if "CANCELLED" not in state_enum:
            state_enum.append("CANCELLED")
    write_json(PACKAGE / "schemas/larm.schema.json", source_schema)


def size_estimate_schema() -> dict[str, Any]:
    size = {
        "type": "object",
        "additionalProperties": False,
        "properties": {
            "confidence": {"type": "string", "enum": ["EXACT", "HIGH", "ESTIMATED", "UNKNOWN"]},
            "bytes": nullable(DECIMAL),
            "lower_bound_bytes": nullable(DECIMAL),
            "upper_bound_bytes": nullable(DECIMAL),
            "source": {
                "type": "string",
                "enum": [
                    "MANIFEST",
                    "CONTENT_LENGTH",
                    "GIT_LFS_METADATA",
                    "HTTP_HEAD",
                    "SOURCE_METADATA",
                    "USER_DECLARED",
                    "LOCAL_OBSERVATION",
                    "UNKNOWN",
                ],
            },
            "evidence_refs": {"type": "array", "minItems": 1, "items": {"type": "string", "minLength": 1}},
            "reason": nullable({"type": "string", "minLength": 1}),
        },
        "required": [
            "confidence",
            "bytes",
            "lower_bound_bytes",
            "upper_bound_bytes",
            "source",
            "evidence_refs",
            "reason",
        ],
        "allOf": [
            {
                "if": {"properties": {"confidence": {"const": "EXACT"}}, "required": ["confidence"]},
                "then": {
                    "properties": {
                        "bytes": DECIMAL,
                        "lower_bound_bytes": DECIMAL,
                        "upper_bound_bytes": DECIMAL,
                        "source": {"const": "MANIFEST"},
                    }
                },
            },
            {
                "if": {"properties": {"confidence": {"const": "HIGH"}}, "required": ["confidence"]},
                "then": {
                    "properties": {
                        "bytes": DECIMAL,
                        "lower_bound_bytes": {"type": "null"},
                        "upper_bound_bytes": DECIMAL,
                        "source": {"enum": ["CONTENT_LENGTH", "GIT_LFS_METADATA", "HTTP_HEAD", "SOURCE_METADATA"]},
                    }
                },
            },
            {
                "if": {"properties": {"confidence": {"const": "ESTIMATED"}}, "required": ["confidence"]},
                "then": {
                    "properties": {
                        "source": {"not": {"const": "UNKNOWN"}},
                    },
                    "anyOf": [
                        {
                            "properties": {
                                "bytes": DECIMAL,
                                "lower_bound_bytes": {"type": "null"},
                                "upper_bound_bytes": DECIMAL,
                            }
                        },
                        {
                            "properties": {
                                "bytes": {"type": "null"},
                                "lower_bound_bytes": DECIMAL,
                                "upper_bound_bytes": DECIMAL,
                            }
                        },
                    ],
                },
            },
            {
                "if": {"properties": {"confidence": {"const": "UNKNOWN"}}, "required": ["confidence"]},
                "then": {
                    "properties": {
                        "bytes": {"type": "null"},
                        "lower_bound_bytes": {"type": "null"},
                        "upper_bound_bytes": {"type": "null"},
                        "source": {"const": "UNKNOWN"},
                        "reason": {"type": "string", "minLength": 1},
                    }
                },
            },
        ],
    }
    return size


def install_contract_schema() -> None:
    schema_path = PACKAGE / "schemas/larm.schema.json"
    schema = read_json(schema_path)
    defs = schema["$defs"]
    defs["ArtifactSizeEstimate"] = size_estimate_schema()
    artifact_estimate = {
        "type": "object",
        "additionalProperties": False,
        "properties": {
            "artifact_id": UUID7,
            "relative_path": {"type": "string", "minLength": 1},
            "required": {"type": "boolean"},
            "selected": {"type": "boolean"},
            "size_estimate": {"$ref": "#/$defs/ArtifactSizeEstimate"},
            "sha256": nullable(SHA256),
        },
        "required": ["artifact_id", "relative_path", "required", "selected", "size_estimate", "sha256"],
    }
    defs["DownloadArtifactEstimate"] = artifact_estimate
    defs["DownloadDestinationOption"] = {
        "type": "object",
        "additionalProperties": False,
        "properties": {
            "storage_location_id": UUID7,
            "selected": {"type": "boolean"},
            "eligible": {"type": "boolean"},
            "reason": {"type": "string", "minLength": 1},
            "storage_estimate": {"$ref": "#/$defs/StorageEstimate"},
        },
        "required": ["storage_location_id", "selected", "eligible", "reason", "storage_estimate"],
    }
    defs["DownloadPlanResponse"] = {
        "type": "object",
        "additionalProperties": False,
        "properties": {
            "preview_id": UUID7,
            "preview_hash": SHA256,
            "variant_id": UUID7,
            "optional_artifact_ids": {"type": "array", "maxItems": 4096, "uniqueItems": True, "items": UUID7},
            "artifact_estimates": {"type": "array", "items": {"$ref": "#/$defs/DownloadArtifactEstimate"}},
            "destination_options": {"type": "array", "items": {"$ref": "#/$defs/DownloadDestinationOption"}},
            "selected_storage_location_id": nullable(UUID7),
            "plan_id": nullable(UUID7),
            "plan_hash": nullable(SHA256),
            "storage_estimate": nullable({"$ref": "#/$defs/StorageEstimate"}),
            "source_id": nullable(UUID7),
            "hard_budget_bytes": nullable(DECIMAL),
            "priority": {"$ref": "#/$defs/DownloadPriority"},
            "expires_at": {"type": "string", "pattern": "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}\\.\\d{3}Z$"},
            "operator_confirmation_required": {"const": True},
        },
        "required": [
            "preview_id",
            "preview_hash",
            "variant_id",
            "optional_artifact_ids",
            "artifact_estimates",
            "destination_options",
            "selected_storage_location_id",
            "plan_id",
            "plan_hash",
            "storage_estimate",
            "source_id",
            "hard_budget_bytes",
            "priority",
            "expires_at",
            "operator_confirmation_required",
        ],
        "allOf": [
            {
                "if": {
                    "properties": {"selected_storage_location_id": {"type": "null"}},
                    "required": ["selected_storage_location_id"],
                },
                "then": {
                    "properties": {
                        "plan_id": {"type": "null"},
                        "plan_hash": {"type": "null"},
                        "storage_estimate": {"type": "null"},
                    }
                },
            },
            {
                "if": {"properties": {"plan_id": {"type": "string"}}, "required": ["plan_id"]},
                "then": {
                    "properties": {
                        "selected_storage_location_id": UUID7,
                        "plan_hash": SHA256,
                        "storage_estimate": {"$ref": "#/$defs/StorageEstimate"},
                    }
                },
            },
        ],
    }
    defs["ExecuteDownloadPlan"] = {
        "type": "object",
        "additionalProperties": False,
        "properties": {
            "plan_id": UUID7,
            "plan_hash": SHA256,
            "operator_confirmed": {"const": True},
        },
        "required": ["plan_id", "plan_hash", "operator_confirmed"],
    }

    request = defs["DownloadPlanRequest"]
    request["properties"]["storage_location_id"] = nullable(UUID7)
    request["properties"]["optional_artifact_ids"] = {
        "type": "array",
        "maxItems": 4096,
        "uniqueItems": True,
        "items": UUID7,
    }
    manifest_items = defs["ArtifactManifest"]["properties"]["artifacts"]["items"]
    manifest_items["properties"]["size_estimate"] = {"$ref": "#/$defs/ArtifactSizeEstimate"}
    api = read_json(PACKAGE / "contracts/api-operations.json")
    for operation in api["operations"]:
        if operation["id"] == "P052_post__download_plans":
            operation["response_schema"] = "larm.schema.json#/$defs/DownloadPlanResponse"
        elif operation["id"] == "P051_post__downloads":
            operation["request_schema"] = "larm.schema.json#/$defs/ExecuteDownloadPlan"
    write_json(PACKAGE / "contracts/api-operations.json", api)
    write_json(schema_path, schema)

    domain_path = PACKAGE / "schemas/domain.schema.json"
    domain = read_json(domain_path)
    model_artifact = domain["$defs"]["model_artifact"]
    model_artifact["properties"]["size_estimate"] = {"$ref": "larm.schema.json#/$defs/ArtifactSizeEstimate"}
    if "size_estimate" not in model_artifact["required"]:
        model_artifact["required"].append("size_estimate")
    write_json(domain_path, domain)

    persistence_path = PACKAGE / "contracts/persistence.sql"
    persistence = persistence_path.read_text(encoding="utf-8")
    marker = "  expected_size INTEGER CHECK(expected_size>=0),\n"
    if "size_estimate TEXT NOT NULL CHECK(json_valid(size_estimate))" not in persistence:
        if marker not in persistence:
            raise RuntimeError("model_artifact expected_size column marker not found")
        persistence = persistence.replace(
            marker,
            marker + "  size_estimate TEXT NOT NULL CHECK(json_valid(size_estimate)),\n",
            1,
        )
    persistence_path.write_text(persistence, encoding="utf-8")


def install_text_contracts() -> None:
    package_rule = PACKAGE / "contracts/00_PACKAGE.md"
    package_text = package_rule.read_text(encoding="utf-8")
    revision_note = (
        "\n\nRevisión 3.1.0 (2026-10-22): modifica únicamente el flujo de planes de descarga P052 y "
        "su representación de tamaño por artefacto. La selección de opcionales, los intervalos "
        "y la confirmación explícita se fijan en 02_STORAGE_DOWNLOAD.md y en sus schemas; el resto "
        "de la revisión 3.0.0 se conserva. El manifiesto registra los hashes nuevos y la revisión previa.\n"
    )
    if "Revisión 3.1.0 (2026-10-22)" not in package_text:
        package_rule.write_text(package_text.rstrip() + revision_note, encoding="utf-8")

    storage = PACKAGE / "contracts/02_STORAGE_DOWNLOAD.md"
    storage_text = storage.read_text(encoding="utf-8")
    addendum = """

## Revisión 3.1.0 — planes de descarga P052

`POST /api/v1/download-plans` recibe `DownloadPlanRequest`. `optional_artifact_ids` puede omitirse (equivale a `[]`); si se envía, contiene IDs UUIDv7 únicos de artefactos opcionales del bundle verificado. Los artefactos requeridos siempre se incluyen. Un ID requerido, inexistente, duplicado o perteneciente a otro bundle se rechaza antes de planificar. `storage_location_id:null` solicita preflight de todos los destinos; un ID concreto solicita el plan para ese destino y conserva en la respuesta las alternativas elegibles y las razones de rechazo.

`ArtifactSizeEstimate` se conserva por archivo. EXACT lleva bytes y límites inferior/superior iguales, con evidencia de manifiesto fijado. HIGH conserva el valor y una cota superior respaldados por Content-Length, Git LFS u otra metadata de revisión fijada. ESTIMATED admite un valor estimado con cota superior o un intervalo con ambos límites y sin valor puntual. UNKNOWN lleva bytes y límites null, source UNKNOWN y motivo no vacío. Los bytes son strings decimales comprobados; cero es un tamaño conocido. El origen y `evidence_refs` acompañan cada tamaño conocido. No convertir un intervalo ni UNKNOWN en un tamaño puntual.

La respuesta `DownloadPlanResponse` devuelve cada artefacto del manifiesto con `required`, `selected` y su estimación, además de todas las alternativas de destino con su `StorageEstimate` y motivo. Con destino null, `plan_id`, `plan_hash` y `storage_estimate` son null: la respuesta sirve para elegir destino y no es ejecutable. Con un destino elegible, la respuesta contiene un plan inmutable cuyo hash liga variante/revisión, selección, archivos y hashes, tamaños y evidencias, origen, prioridad, presupuesto, destino y vencimiento. Crear un plan no crea job, no reserva espacio y no empieza transferencia.

`POST /api/v1/downloads` solo acepta `ExecuteDownloadPlan` con `operator_confirmed:true`, `plan_id` y el `plan_hash` exacto visto por el operador. La interfaz solicita esa confirmación explícita después de mostrar artefactos, certeza de tamaño y destino. El servidor vuelve a validar revisión, hashes, destino y reserva; en una transacción durable consume la confirmación, crea job/reserva/journal/outbox y registra actor y hora. Rechaza plan sin destino, caducado o cambiado, falta de confirmación y replay. El plan no autoriza ejecución por sí mismo.

La migración `migrations/0006_p052_artifact_size_estimate.sql` añade la estimación persistida. Las filas existentes reciben UNKNOWN con motivo de migración; el valor histórico de `expected_size` no se eleva a EXACT ni ESTIMATED sin evidencia.
"""
    if "## Revisión 3.1.0 — planes de descarga P052" not in storage_text:
        storage.write_text(storage_text.rstrip() + addendum, encoding="utf-8")

    api_docs = PACKAGE / "contracts/07_API_EVENTS_MCP.md"
    api_text = api_docs.read_text(encoding="utf-8")
    api_addendum = (
        "\n\nP052 3.1.0: el flujo de descarga es preview (`POST /download-plans`) → selección de destino → "
        "confirmación del operador → job (`POST /downloads`). El preview devuelve `DownloadPlanResponse`, "
        "no reserva ni crea jobs; la ejecución requiere `ExecuteDownloadPlan.operator_confirmed=true` y el "
        "hash exacto del plan.\n"
    )
    if "P052 3.1.0: el flujo de descarga" not in api_text:
        api_docs.write_text(api_text.rstrip() + api_addendum, encoding="utf-8")

    p052 = PACKAGE / "prompts/04_models_sources/P052_planificador-de-tamano-y-descarga-multi-fuente.md"
    prompt = p052.read_text(encoding="utf-8")
    prompt_addendum = """

### Revisión contractual 3.1.0

- `DownloadPlanRequest.optional_artifact_ids` expresa la selección de IDs opcionales; omitido equivale a lista vacía. `storage_location_id:null` devuelve alternativas preflight sin crear un plan ejecutable.
- `ArtifactManifest` y `model_artifact` conservan `size_estimate` por fichero con EXACT, HIGH, ESTIMATED (puntual o intervalo) y UNKNOWN, procedencia, evidencia y límites decimales.
- `DownloadPlanResponse` devuelve cada tamaño y todas las opciones de destino. El hash del plan fija la revisión, selección, tamaños/evidencias, origen, prioridad, presupuesto y destino.
- La planificación no crea job, reserva ni inicia transferencia. `POST /downloads` exige `ExecuteDownloadPlan.operator_confirmed:true`; la ejecución revalida y confirma job, reserva, journal y outbox de forma durable.
- Aplicar la migración contractual 0006 a los datos existentes asignando UNKNOWN, sin inferir certeza desde `expected_size`.
"""
    if "### Revisión contractual 3.1.0" not in prompt:
        p052.write_text(prompt.rstrip() + prompt_addendum, encoding="utf-8")
    prompt = p052.read_text(encoding="utf-8")
    aggregate_path = PACKAGE / "ALL_PROMPTS.md"
    aggregate = aggregate_path.read_text(encoding="utf-8")
    marker = "<!-- P052 -->\n"
    start = aggregate.index(marker) + len(marker)
    next_marker = aggregate.find("\n<!-- P", start)
    if next_marker < 0:
        raise RuntimeError("P052 aggregate terminator not found")
    aggregate_path.write_text(aggregate[:start] + prompt + aggregate[next_marker + 1 :], encoding="utf-8")

    (PACKAGE / "contracts/REVISION_3_1_0_P052.md").write_text(
        "# Revisión de contrato LARM 3.1.0 — P052\n\n"
        "Fecha: 2026-10-22  \nBase: paquete 3.0.0  \nEstado: incorporada al paquete contractual.\n\n"
        "Resuelve dos carencias para el preview local: selección explícita de artefactos opcionales y "
        "tamaño con confianza, cotas, procedencia y evidencia por artefacto. Permite preflight sin "
        "destino para comparar alternativas, liga la selección y el destino al hash inmutable y exige "
        "confirmación explícita antes de crear el job. Las filas históricas se migran a UNKNOWN y no "
        "se reinterpretan.\n\n"
        "Archivos normativos actualizados: `02_STORAGE_DOWNLOAD.md`, `07_API_EVENTS_MCP.md`, "
        "`api-operations.json`, `persistence.sql`, `schemas/larm.schema.json`, "
        "`schemas/domain.schema.json`, el prompt P052 y la migración 0006. Se consolidaron además "
        "las cuatro extensiones locales de P031/P099 ya usadas por el proyecto, antes de volver a "
        "sincronizar sus copias.\n",
        encoding="utf-8",
    )


def install_migration() -> None:
    migration = PACKAGE / MIGRATION
    migration.parent.mkdir(parents=True, exist_ok=True)
    migration.write_text(
        "-- P052 contract revision 3.1.0. Preserve every legacy row as unknown until re-observed.\n"
        "ALTER TABLE model_artifact\n"
        "  ADD COLUMN size_estimate TEXT NOT NULL\n"
        "  DEFAULT '{\"confidence\":\"UNKNOWN\",\"bytes\":null,\"lower_bound_bytes\":null,\"upper_bound_bytes\":null,\"source\":\"UNKNOWN\",\"evidence_refs\":[\"migration:0006\"],\"reason\":\"legacy size has no confidence evidence\"}'\n"
        "  CHECK(json_valid(size_estimate));\n",
        encoding="utf-8",
    )


def update_manifest() -> None:
    manifest_path = PACKAGE / "manifest.json"
    manifest = read_json(manifest_path)
    manifest["package_version"] = "3.1.0"
    manifest["predecessor_package_version"] = "3.0.0"
    manifest["generated_at_utc"] = datetime.now(timezone.utc).isoformat(timespec="milliseconds").replace("+00:00", "Z")
    entries = {item["file"]: item for item in manifest["normative_files"]}
    tracked = [
        "ALL_PROMPTS.md",
        "contracts/00_PACKAGE.md",
        "contracts/02_STORAGE_DOWNLOAD.md",
        "contracts/07_API_EVENTS_MCP.md",
        "contracts/REVISION_3_1_0_P052.md",
        "contracts/api-operations.json",
        "contracts/adapter-methods.json",
        "contracts/persistence.sql",
        "schemas/larm.schema.json",
        "schemas/domain.schema.json",
        "prompts/04_models_sources/P052_planificador-de-tamano-y-descarga-multi-fuente.md",
        "tools/verify_pack.py",
        MIGRATION,
    ]
    for rel in tracked:
        path = PACKAGE / rel
        entries[rel] = {"file": rel, "bytes": path.stat().st_size, "sha256": hashlib.sha256(path.read_bytes()).hexdigest()}
    manifest["normative_files"] = [entries[k] for k in sorted(entries)]
    manifest["contract_revision_log"] = [
        {
            "version": "3.1.0",
            "predecessor": "3.0.0",
            "date": "2026-10-22",
            "scope": "P052 optional artifact selection, per-artifact size evidence, destination preflight, and explicit confirmation",
            "audit": "contracts/REVISION_3_1_0_P052.md",
        }
    ]
    write_json(manifest_path, manifest)


def apply() -> None:
    add_published_local_deltas()
    install_contract_schema()
    install_text_contracts()
    install_migration()
    update_manifest()
    result = subprocess.run(
        [sys.executable, str(REPO / "tools/package_copy.py"), "sync"],
        cwd=REPO,
        check=False,
        capture_output=True,
        text=True,
        encoding="utf-8",
    )
    print(result.stdout, end="")
    if result.returncode:
        print(result.stderr, file=sys.stderr)
        raise SystemExit(result.returncode)
    digest = hashlib.sha256((REPO / "docs/contracts/persistence.sql").read_bytes()).hexdigest()
    (REPO / "docs/contracts/persistence.sql.sha256").write_text(digest + "\n", encoding="ascii")


def main() -> int:
    if len(sys.argv) != 2 or sys.argv[1] != "apply":
        print("Usage: python tools/p052_contract_revision.py apply", file=sys.stderr)
        return 2
    apply()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
