//! OpenAPI 3.1 generado (P005, contrato 07).
//!
//! El contrato 07 declara `api-operations.json` y los schemas JSON (Draft
//! 2020-12) fuente única de rutas y tipos, y exige que el OpenAPI «las
//! refleje sin inventar otro DTO». Por eso no hay anotaciones paralelas: el
//! documento se construye con esos ficheros embebidos. Los `$defs` de
//! `larm.schema.json` van a `components/schemas/<Nombre>` y los de
//! `domain.schema.json` a `components/schemas/domain.<nombre>`. Cada operación
//! lleva `x-larm-owner`, `x-larm-mode` y `x-larm-implemented`, este último
//! calculado sobre las rutas que el router atiende de verdad.

use serde_json::{json, Map, Value};

const OPERATIONS: &str = include_str!("../../../docs/contracts/api-operations.json");
const LARM_SCHEMA: &str = include_str!("../../../docs/contracts/schemas/larm.schema.json");
const DOMAIN_SCHEMA: &str = include_str!("../../../docs/contracts/schemas/domain.schema.json");

/// Reescribe los `$ref` de un schema del paquete a `#/components/schemas/...`.
fn rewrite_refs(value: &mut Value, local_prefix: &str) {
    match value {
        Value::Object(map) => {
            if let Some(Value::String(r)) = map.get_mut("$ref") {
                *r = component_ref(r, local_prefix);
            }
            for v in map.values_mut() {
                rewrite_refs(v, local_prefix);
            }
        }
        Value::Array(items) => items.iter_mut().for_each(|v| rewrite_refs(v, local_prefix)),
        _ => {}
    }
}

/// `larm.schema.json#/$defs/X` → `#/components/schemas/X`;
/// `domain.schema.json#/$defs/y` → `#/components/schemas/domain.y`;
/// `#/$defs/z` dentro de un fichero → el prefijo de ese fichero.
fn component_ref(reference: &str, local_prefix: &str) -> String {
    let (file, def) = reference.split_once("#/$defs/").unwrap_or(("", reference));
    let prefix = match file {
        "larm.schema.json" => "",
        "domain.schema.json" => "domain.",
        _ => local_prefix,
    };
    format!("#/components/schemas/{prefix}{def}")
}

fn components() -> Result<Map<String, Value>, String> {
    let mut out = Map::new();
    for (text, prefix) in [(LARM_SCHEMA, ""), (DOMAIN_SCHEMA, "domain.")] {
        let doc: Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
        let defs = doc
            .get("$defs")
            .and_then(Value::as_object)
            .ok_or("schema sin $defs")?;
        for (name, schema) in defs {
            let mut schema = schema.clone();
            rewrite_refs(&mut schema, prefix);
            out.insert(format!("{prefix}{name}"), schema);
        }
    }
    Ok(out)
}

fn schema_ref(reference: &str) -> Value {
    json!({ "$ref": component_ref(reference, "") })
}

/// Documento OpenAPI 3.1. `implemented(method, path)` dice si el router la atiende.
///
/// # Errors
/// Texto del fallo si los contratos embebidos no se pueden interpretar.
pub fn document(version: &str, implemented: &dyn Fn(&str, &str) -> bool) -> Result<Value, String> {
    let ops: Value = serde_json::from_str(OPERATIONS).map_err(|e| e.to_string())?;
    let mut paths = Map::new();
    // El contrato repite un id (P014_get__storage_locations para /storage-locations
    // y /storage/locations). OpenAPI exige operationId únicos: a partir de la
    // segunda aparición se añade la ruta y el id del contrato queda en
    // `x-larm-contract-operation-id`.
    let mut seen_ids = std::collections::BTreeSet::new();
    for op in ops["operations"].as_array().ok_or("sin operations")? {
        let text = |k: &str| op[k].as_str().unwrap_or_default().to_string();
        let (method, path, scope, mode) =
            (text("method"), text("path"), text("scope"), text("mode"));
        let status = op["success_status"].as_u64().unwrap_or(200).to_string();
        let mut parameters: Vec<Value> = op["path_parameters"]
            .as_object()
            .into_iter()
            .flatten()
            .map(|(name, schema)| json!({"name": name, "in": "path", "required": true, "schema": schema}))
            .collect();
        for header in op["required_headers"].as_array().into_iter().flatten() {
            parameters.push(json!({"name": header, "in": "header", "required": true, "schema": {"type": "string"}}));
        }
        let mut success = json!({"description": "Éxito"});
        if mode == "sse" {
            success["content"] = json!({"text/event-stream": {"schema": {"type": "string"}}});
        } else if let Some(r) = op["response_schema"].as_str() {
            success["content"] = json!({"application/json": {"schema": schema_ref(r)}});
        }
        let contract_id = text("id");
        let operation_id = if seen_ids.insert(contract_id.clone()) {
            contract_id.clone()
        } else {
            let suffix: String = path
                .chars()
                .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
                .collect();
            format!("{contract_id}__{suffix}")
        };
        let mut operation = json!({
            "operationId": operation_id,
            "x-larm-contract-operation-id": contract_id,
            "x-larm-owner": text("owner"),
            "x-larm-mode": mode,
            "x-larm-scope": scope,
            "x-larm-implemented": implemented(&method, &path),
            "parameters": parameters,
            "responses": {
                status: success,
                "default": {"description": "Error (contrato 07)",
                            "content": {"application/json": {"schema": {"$ref": "#/components/schemas/Error"}}}}
            },
            "security": if scope == "public" { json!([]) } else { json!([{"bearer": [scope]}]) },
        });
        if let Some(r) = op["request_schema"].as_str() {
            operation["requestBody"] = json!({"required": true, "content": {"application/json": {"schema": schema_ref(r)}}});
        }
        let entry = paths.entry(path).or_insert_with(|| json!({}));
        entry[method.to_ascii_lowercase()] = operation;
    }
    Ok(json!({
        "openapi": "3.1.0",
        "jsonSchemaDialect": "https://json-schema.org/draft/2020-12/schema",
        "info": {
            "title": "LARM — API administrativa",
            "version": version,
            "description": "Generado desde docs/contracts/api-operations.json y los schemas del paquete v3. No editar a mano."
        },
        "servers": [{"url": "http://127.0.0.1:8790"}],
        "paths": paths,
        "components": {
            "schemas": components()?,
            "securitySchemes": {"bearer": {"type": "http", "scheme": "bearer"}}
        }
    }))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    fn all_refs(value: &Value, out: &mut Vec<String>) {
        match value {
            Value::Object(m) => {
                if let Some(Value::String(r)) = m.get("$ref") {
                    out.push(r.clone());
                }
                m.values().for_each(|v| all_refs(v, out));
            }
            Value::Array(a) => a.iter().for_each(|v| all_refs(v, out)),
            _ => {}
        }
    }

    #[test]
    fn todas_las_operaciones_y_ninguna_referencia_rota() {
        let doc = document("0.1.0", &|_, _| false).unwrap();
        let count: usize = doc["paths"]
            .as_object()
            .unwrap()
            .values()
            .map(|p| p.as_object().unwrap().len())
            .sum();
        assert_eq!(count, 159);
        let schemas = doc["components"]["schemas"].as_object().unwrap();
        let mut refs = Vec::new();
        all_refs(&doc, &mut refs);
        assert!(!refs.is_empty());
        for r in refs {
            let name = r
                .strip_prefix("#/components/schemas/")
                .unwrap_or_else(|| panic!("{r}"));
            assert!(schemas.contains_key(name), "referencia rota {r}");
        }
    }

    #[test]
    fn search_completo_en_las_dos_rutas() {
        let doc = document("0.1.0", &|_, _| false).unwrap();
        for path in ["/api/v1/models/search", "/api/v1/sources/search"] {
            assert_eq!(
                doc["paths"][path]["post"]["requestBody"]["content"]["application/json"]["schema"]
                    ["$ref"],
                "#/components/schemas/Search",
                "{path}"
            );
        }
        let search = &doc["components"]["schemas"]["Search"];
        assert_eq!(search["required"].as_array().unwrap().len(), 18);
        assert_eq!(search["additionalProperties"], false);
    }

    #[test]
    fn marca_implementadas_publicas_y_sse() {
        let doc = document("0.1.0", &|m, p| m == "GET" && p == "/api/v1/health/live").unwrap();
        let live = &doc["paths"]["/api/v1/health/live"]["get"];
        assert_eq!(live["x-larm-implemented"], true);
        assert_eq!(live["security"], json!([]));
        assert_eq!(
            doc["paths"]["/api/v1/health/ready"]["get"]["x-larm-implemented"],
            false
        );
        assert!(
            doc["paths"]["/api/v1/events"]["get"]["responses"]["200"]["content"]
                ["text/event-stream"]
                .is_object()
        );
        assert!(is_implemented("POST", "/api/v1/runtime-installations"));
        assert!(is_implemented("GET", "/api/v1/execution-hosts"));
        assert!(is_implemented("GET", "/api/v1/custom-rules"));
        assert!(is_implemented("PATCH", "/api/v1/custom-rules"));
        assert!(is_implemented("GET", "/api/v1/compatibility"));
        let runtime_install = document("0.1.0", &is_implemented).unwrap();
        assert_eq!(
            runtime_install["paths"]["/api/v1/runtime-installations"]["post"]["x-larm-implemented"],
            true
        );
        assert!(is_implemented(
            "GET",
            "/api/v1/runtime-profile-locks/{lock_sha256}"
        ));
        assert!(is_implemented(
            "PUT",
            "/api/v1/runtime-profile-locks/{lock_sha256}"
        ));
        let runtime_profiles = document("0.1.0", &is_implemented).unwrap();
        assert_eq!(
            runtime_profiles["paths"]["/api/v1/runtime-profile-locks/{lock_sha256}"]["put"]
                ["x-larm-implemented"],
            true
        );
        let required = runtime_profiles["components"]["schemas"]["RuntimeProfileLock"]
            ["properties"]["fields"]["required"]
            .as_array()
            .unwrap();
        let actual: std::collections::BTreeSet<_> = required
            .iter()
            .map(|value| value.as_str().unwrap())
            .collect();
        let expected: std::collections::BTreeSet<_> =
            larm_domain::runtime_profiles::REQUIRED_FIELDS
                .iter()
                .copied()
                .collect();
        assert_eq!(actual, expected);
        assert_eq!(actual.len(), 22);
    }

    #[test]
    fn operation_ids_unicos_aunque_el_contrato_repita_uno() {
        let doc = document("0.1.0", &|_, _| false).unwrap();
        let mut ids = std::collections::BTreeSet::new();
        for path in doc["paths"].as_object().unwrap().values() {
            for op in path.as_object().unwrap().values() {
                assert!(ids.insert(op["operationId"].as_str().unwrap().to_string()));
            }
        }
        assert_eq!(
            doc["paths"]["/api/v1/storage/locations"]["get"]["x-larm-contract-operation-id"],
            "P014_get__storage_locations"
        );
    }

    #[test]
    fn determinista() {
        let a = document("0.1.0", &|_, _| true).unwrap();
        let b = document("0.1.0", &|_, _| true).unwrap();
        assert_eq!(a, b);
    }
}

/// Operaciones que el código sirve hoy (aplicación P005, seguridad P009 y,
/// con `--bridge-config`, el puente P107). Fuente de `x-larm-implemented`.
pub const IMPLEMENTED: &[(&str, &str)] = &[
    ("GET", "/api/v1/runtimes"),
    ("POST", "/api/v1/runtimes"),
    ("GET", "/api/v1/runtimes/{id}"),
    ("PATCH", "/api/v1/runtimes/{id}/channel-policy"),
    ("GET", "/api/v1/runtimes/{id}/versions"),
    ("GET", "/api/v1/runtime-versions"),
    ("GET", "/api/v1/runtime-versions/{id}"),
    ("GET", "/api/v1/runtime-profile-locks/{lock_sha256}"),
    ("PUT", "/api/v1/runtime-profile-locks/{lock_sha256}"),
    ("GET", "/api/v1/runtime-installations"),
    ("POST", "/api/v1/runtime-installations"),
    ("GET", "/api/v1/runtime-installations/{id}"),
    ("GET", "/api/v1/runtime-backends"),
    ("GET", "/api/v1/runtime-backends/{id}"),
    ("GET", "/api/v1/execution-hosts"),
    ("PUT", "/api/v1/download-admission-policy"),
    ("GET", "/api/v1/download-admission-policy"),
    ("GET", "/api/v1/download-admissions"),
    ("POST", "/api/v1/download-admissions/{id}/revoke"),
    ("GET", "/api/v1/external-hosts"),
    ("POST", "/api/v1/external-hosts"),
    ("GET", "/api/v1/external-hosts/{id}"),
    ("GET", "/api/v1/sources"),
    ("POST", "/api/v1/sources"),
    ("GET", "/api/v1/sources/{id}"),
    ("PATCH", "/api/v1/sources/{id}"),
    ("DELETE", "/api/v1/sources/{id}"),
    ("GET", "/api/v1/source-watches"),
    ("POST", "/api/v1/source-watches"),
    ("GET", "/api/v1/source-watches/{id}"),
    ("PATCH", "/api/v1/source-watches/{id}"),
    ("DELETE", "/api/v1/source-watches/{id}"),
    ("GET", "/api/v1/notifications"),
    ("GET", "/api/v1/notifications/{id}"),
    ("PATCH", "/api/v1/notifications/{id}"),
    ("GET", "/api/v1/source-accounts"),
    ("POST", "/api/v1/source-accounts"),
    ("GET", "/api/v1/source-accounts/{id}"),
    ("DELETE", "/api/v1/source-accounts/{id}"),
    ("POST", "/api/v1/sources/{id}/basic-credential"),
    ("POST", "/api/v1/sources/{id}/license"),
    ("POST", "/api/v1/sources/{id}/probe"),
    ("GET", "/api/v1/adapters"),
    ("GET", "/api/v1/adapters/{id}"),
    ("GET", "/api/v1/health/live"),
    ("GET", "/api/v1/health/ready"),
    ("GET", "/api/v1/events"),
    ("POST", "/api/v1/credentials"),
    ("GET", "/api/v1/credentials"),
    ("GET", "/api/v1/credentials/{id}"),
    ("DELETE", "/api/v1/credentials/{id}"),
    ("POST", "/api/v1/approvals"),
    ("GET", "/api/v1/broker/catalog"),
    ("GET", "/api/v1/launch-profiles"),
    ("POST", "/api/v1/launch-profiles"),
    ("GET", "/api/v1/launch-profiles/{id}"),
    ("PATCH", "/api/v1/launch-profiles/{id}"),
    ("DELETE", "/api/v1/launch-profiles/{id}"),
    ("POST", "/api/v1/leases"),
    ("GET", "/api/v1/leases"),
    ("GET", "/api/v1/leases/{id}"),
    ("DELETE", "/api/v1/leases/{id}"),
    ("POST", "/api/v1/leases/{id}/renew"),
    ("GET", "/api/v1/jobs/{id}"),
    ("POST", "/api/v1/jobs/{id}/cancel"),
    ("GET", "/api/v1/hardware"),
    ("POST", "/api/v1/hardware/refresh"),
    ("GET", "/api/v1/storage-locations"),
    ("POST", "/api/v1/storage-locations"),
    ("GET", "/api/v1/storage-locations/{id}"),
    ("PATCH", "/api/v1/storage-locations/{id}"),
    ("DELETE", "/api/v1/storage-locations/{id}"),
    ("POST", "/api/v1/storage-locations/{id}/probe"),
    ("GET", "/api/v1/storage"),
    ("GET", "/api/v1/storage/locations"),
    ("POST", "/api/v1/models/{id}/move"),
    ("GET", "/api/v1/artifact-locations"),
    ("GET", "/api/v1/artifact-locations/{id}"),
    ("GET", "/api/v1/models"),
    ("GET", "/api/v1/models/{id}"),
    ("GET", "/api/v1/model-distributions"),
    ("GET", "/api/v1/model-distributions/{id}"),
    ("GET", "/api/v1/model-revisions"),
    ("GET", "/api/v1/model-revisions/{id}"),
    ("GET", "/api/v1/variants"),
    ("GET", "/api/v1/variants/{id}"),
    ("GET", "/api/v1/models/{id}/variants"),
    ("GET", "/api/v1/model-bundles"),
    ("GET", "/api/v1/model-bundles/{id}"),
    ("GET", "/api/v1/model-artifacts"),
    ("GET", "/api/v1/model-artifacts/{id}"),
    ("POST", "/api/v1/models/select"),
    ("POST", "/api/v1/models/import/preview"),
    ("POST", "/api/v1/models/import"),
    ("GET", "/api/v1/variants/{id}/detail"),
    ("PATCH", "/api/v1/variants/{id}/promotion"),
    ("POST", "/api/v1/download-plans"),
    ("GET", "/api/v1/downloads"),
    ("POST", "/api/v1/downloads"),
    ("PATCH", "/api/v1/downloads/{id}/priority"),
    ("POST", "/api/v1/downloads/{id}/pause"),
    ("POST", "/api/v1/downloads/{id}/resume"),
    ("POST", "/api/v1/downloads/{id}/cancel"),
    ("POST", "/api/v1/transform-plans"),
    ("POST", "/api/v1/transformations"),
    ("POST", "/api/v1/transformations/{id}/restart"),
    ("GET", "/api/v1/transformations"),
    ("GET", "/api/v1/compatibility"),
    ("GET", "/api/v1/storage-locations/{id}/analysis"),
    ("POST", "/api/v1/storage-locations/{id}/cleanup-plan"),
    ("POST", "/api/v1/storage-locations/{id}/cleanup"),
    ("POST", "/api/v1/models/{id}/delete-plan"),
    ("DELETE", "/api/v1/models/{id}"),
    ("POST", "/api/v1/adapters/declarative"),
    ("POST", "/api/v1/adapters/import"),
    ("POST", "/api/v1/adapters/export"),
    ("GET", "/api/v1/adapters/registry"),
    ("POST", "/api/v1/adapters/registry/sync"),
    ("POST", "/api/v1/adapters/{id}/update"),
    ("POST", "/api/v1/adapters/{id}/probe"),
    ("GET", "/api/v1/custom-rules"),
    ("PATCH", "/api/v1/custom-rules"),
    ("GET", "/api/v1/settings"),
    ("PATCH", "/api/v1/settings"),
];

/// ¿Está `method path` (plantilla del contrato) en [`IMPLEMENTED`]?
#[must_use]
pub fn is_implemented(method: &str, path: &str) -> bool {
    IMPLEMENTED
        .iter()
        .any(|(m, p)| m.eq_ignore_ascii_case(method) && *p == path)
}

#[cfg(test)]
mod implemented_tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    #[test]
    fn p051_controls_and_confirmation_are_published_as_implemented() {
        for (method, path) in [
            ("POST", "/api/v1/downloads"),
            ("PATCH", "/api/v1/downloads/{id}/priority"),
            ("POST", "/api/v1/downloads/{id}/pause"),
            ("POST", "/api/v1/downloads/{id}/resume"),
            ("POST", "/api/v1/downloads/{id}/cancel"),
        ] {
            assert!(super::is_implemented(method, path), "{method} {path}");
        }
        let doc = super::document("0.1.0", &super::is_implemented).unwrap();
        assert_eq!(
            doc["paths"]["/api/v1/downloads/{id}/cancel"]["post"]["responses"]["202"]["content"]
                ["application/json"]["schema"]["$ref"],
            "#/components/schemas/JobReceipt"
        );
    }

    #[test]
    fn cada_operacion_implementada_existe_en_el_contrato() {
        let catalog = larm_domain::auth::Catalog::contract();
        for (method, path) in super::IMPLEMENTED {
            assert!(
                catalog
                    .operations
                    .iter()
                    .any(|o| o.method == *method && o.path == *path),
                "{method} {path} no está en api-operations.json"
            );
        }
    }

    #[test]
    fn p093_routes_are_published_as_implemented() {
        assert!(super::is_implemented("POST", "/api/v1/transform-plans"));
        assert!(super::is_implemented("POST", "/api/v1/transformations"));
        assert!(super::is_implemented("GET", "/api/v1/transformations"));
        assert!(super::is_implemented(
            "POST",
            "/api/v1/transformations/{id}/restart"
        ));
    }

    #[test]
    fn p120_promotion_route_is_published_as_implemented() {
        assert!(super::is_implemented(
            "PATCH",
            "/api/v1/variants/{id}/promotion"
        ));
        let document = super::document("0.1.0", &super::is_implemented).unwrap();
        assert_eq!(
            document["paths"]["/api/v1/variants/{id}/promotion"]["patch"]["x-larm-implemented"],
            serde_json::json!(true)
        );
    }

    #[test]
    fn p128_notification_and_watch_routes_are_published_as_implemented() {
        for (method, path) in [
            ("GET", "/api/v1/notifications"),
            ("GET", "/api/v1/notifications/{id}"),
            ("PATCH", "/api/v1/notifications/{id}"),
            ("GET", "/api/v1/source-watches"),
            ("POST", "/api/v1/source-watches"),
            ("GET", "/api/v1/source-watches/{id}"),
            ("PATCH", "/api/v1/source-watches/{id}"),
            ("DELETE", "/api/v1/source-watches/{id}"),
        ] {
            assert!(super::is_implemented(method, path), "{method} {path}");
        }
        let document = super::document("0.1.0", &super::is_implemented).unwrap();
        assert_eq!(
            document["paths"]["/api/v1/notifications"]["get"]["x-larm-implemented"],
            true
        );
        assert_eq!(
            document["paths"]["/api/v1/source-watches/{id}"]["patch"]["x-larm-implemented"],
            true
        );
    }

    #[test]
    fn p045_source_account_routes_are_published_as_implemented() {
        assert!(super::is_implemented("GET", "/api/v1/source-accounts"));
        assert!(super::is_implemented("POST", "/api/v1/source-accounts"));
        assert!(super::is_implemented("GET", "/api/v1/source-accounts/{id}"));
        assert!(super::is_implemented(
            "DELETE",
            "/api/v1/source-accounts/{id}"
        ));
        assert!(super::is_implemented(
            "POST",
            "/api/v1/sources/{id}/license"
        ));
        assert!(super::is_implemented("POST", "/api/v1/sources/{id}/probe"));
    }
}
