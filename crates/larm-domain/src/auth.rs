//! Autorización de la API y del MCP de administración (P009, spec §90–§92,
//! contratos 03, 07 y 11/H06). Puro: sin IO.
//!
//! - Scopes de API del contrato 03; `admin` es superconjunto de los scopes de
//!   API y solo existe por concesión explícita.
//! - Permisos MCP de §92 (`mcp-permissions.json`). Un principal con audiencia
//!   `larm-mcp-admin` necesita el permiso MCP de la herramienta **y** el scope
//!   API de la operación, entre por MCP o directamente por la API; una
//!   operación sin herramienta MCP asociada se le niega. El `admin` MCP es
//!   comodín de herramientas, no añade scopes de API.
//! - El Broker solo puede tener `read` y `leases:manage`.
//! - La procedencia (audiencia) sale de la credencial autenticada, nunca de
//!   una cabecera del cliente.

use crate::error::{ErrorCode, LarmError};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;

macro_rules! string_enum {
    ($(#[$m:meta])* $name:ident { $($variant:ident => $text:literal),+ $(,)? }) => {
        $(#[$m])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub enum $name { $($variant),+ }

        impl $name {
            pub const ALL: &'static [$name] = &[$($name::$variant),+];

            #[must_use]
            pub fn as_str(self) -> &'static str {
                match self { $($name::$variant => $text),+ }
            }

            #[must_use]
            pub fn parse(value: &str) -> Option<Self> {
                match value { $($text => Some($name::$variant),)+ _ => None }
            }
        }

        impl Serialize for $name {
            fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                s.serialize_str(self.as_str())
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                let text = String::deserialize(d)?;
                Self::parse(&text).ok_or_else(|| serde::de::Error::custom(format!(
                    concat!(stringify!($name), " desconocido: {}"), text)))
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.as_str())
            }
        }
    };
}

string_enum!(
    /// Scopes de la API (contrato 03).
    ApiScope {
        Read => "read",
        ModelsWrite => "models:write",
        StorageWrite => "storage:write",
        RuntimesWrite => "runtimes:write",
        JobsWrite => "jobs:write",
        LeasesManage => "leases:manage",
        BenchmarksWrite => "benchmarks:write",
        AdaptersWrite => "adapters:write",
        CredentialsWrite => "credentials:write",
        CredentialsRead => "credentials:read",
        SettingsWrite => "settings:write",
        Admin => "admin",
    }
);

string_enum!(
    /// Permisos del MCP de administración (spec §92, `mcp-permissions.json`).
    McpPermission {
        Read => "read",
        Search => "search",
        Download => "download",
        Install => "install",
        Start => "start",
        Stop => "stop",
        Benchmark => "benchmark",
        Move => "move",
        Delete => "delete",
        Update => "update",
        Admin => "admin",
    }
);

string_enum!(
    /// Tipo de principal. Fija qué scopes puede recibir.
    PrincipalKind {
        Operator => "OPERATOR",
        Desktop => "DESKTOP",
        Broker => "BROKER",
        Mcp => "MCP",
    }
);

/// Audiencia del token MCP de administración (schema `McpPrincipal`).
pub const MCP_AUDIENCE: &str = "larm-mcp-admin";

/// Principal autenticado. `name` es el `owner_id` de sus recursos (leases...).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Principal {
    /// UUIDv7 del `credential_reference` que guarda su token.
    pub id: String,
    pub name: String,
    pub kind: PrincipalKind,
    pub api_scopes: BTreeSet<ApiScope>,
    pub mcp_scopes: BTreeSet<McpPermission>,
}

impl Principal {
    /// Construye un principal comprobando las restricciones del contrato.
    ///
    /// # Errors
    /// `VALIDATION_FAILED` si el nombre no es válido, el Broker pide más que
    /// `read`+`leases:manage`, un no-MCP recibe permisos MCP o un MCP no tiene ninguno.
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        kind: PrincipalKind,
        api_scopes: impl IntoIterator<Item = ApiScope>,
        mcp_scopes: impl IntoIterator<Item = McpPermission>,
    ) -> Result<Self, LarmError> {
        let principal = Self {
            id: id.into(),
            name: name.into(),
            kind,
            api_scopes: api_scopes.into_iter().collect(),
            mcp_scopes: mcp_scopes.into_iter().collect(),
        };
        principal.validate()?;
        Ok(principal)
    }

    /// # Errors
    /// Ver [`Principal::new`].
    pub fn validate(&self) -> Result<(), LarmError> {
        let invalid = |reason: &str, detail: String| {
            Err(LarmError::new(
                ErrorCode::ValidationFailed,
                "Principal no válido",
                reason.to_string(),
            )
            .with_technical(detail))
        };
        if self.name.is_empty()
            || self.name.len() > 64
            || !self
                .name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        {
            return invalid("principal_name_invalid", format!("{:.64}", self.name));
        }
        if self.kind == PrincipalKind::Broker {
            let allowed = [ApiScope::Read, ApiScope::LeasesManage];
            if let Some(extra) = self.api_scopes.iter().find(|s| !allowed.contains(s)) {
                return invalid(
                    "broker_scope_not_allowed",
                    format!("el Broker solo admite read y leases:manage, pidió {extra}"),
                );
            }
        }
        if self.kind == PrincipalKind::Mcp {
            if self.mcp_scopes.is_empty() {
                return invalid(
                    "mcp_scopes_missing",
                    "un principal MCP necesita mcp_scopes".into(),
                );
            }
        } else if !self.mcp_scopes.is_empty() {
            return invalid(
                "mcp_scopes_not_allowed",
                format!("{} no es MCP y no puede tener mcp_scopes", self.kind),
            );
        }
        Ok(())
    }

    /// Audiencia de su credencial: la MCP solo para principales MCP.
    #[must_use]
    pub fn audience(&self) -> &'static str {
        if self.kind == PrincipalKind::Mcp {
            MCP_AUDIENCE
        } else {
            "larm-api"
        }
    }

    /// ¿Tiene el scope de API (o `admin`)?
    #[must_use]
    pub fn has_api_scope(&self, scope: ApiScope) -> bool {
        self.api_scopes.contains(&scope) || self.api_scopes.contains(&ApiScope::Admin)
    }
}

/// Requisito de acceso de una operación (`api-operations.json`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationScope {
    /// `health/live`: estado binario público.
    Public,
    /// Callback OAuth: lo autoriza su nonce/state (P141), nunca un Bearer.
    OauthNonce,
    Api(ApiScope),
}

/// Operación de la API del contrato.
#[derive(Debug, Clone)]
pub struct Operation {
    pub id: String,
    pub method: String,
    pub path: String,
    pub scope: OperationScope,
    pub success_status: u16,
    pub required_headers: Vec<String>,
    /// Herramienta MCP que la expone y su permiso, si existe.
    pub mcp_tool: Option<(String, McpPermission)>,
}

impl Operation {
    /// ¿Casa `path` (sin query) con la plantilla, p. ej. `/api/v1/credentials/{id}`?
    #[must_use]
    pub fn matches(&self, method: &str, path: &str) -> bool {
        if !self.method.eq_ignore_ascii_case(method) {
            return false;
        }
        let template: Vec<&str> = self.path.trim_end_matches('/').split('/').collect();
        let actual: Vec<&str> = path.trim_end_matches('/').split('/').collect();
        template.len() == actual.len()
            && template
                .iter()
                .zip(&actual)
                .all(|(t, a)| (t.starts_with('{') && t.ends_with('}') && !a.is_empty()) || t == a)
    }
}

/// Catálogo de operaciones del contrato con su herramienta MCP.
#[derive(Debug)]
pub struct Catalog {
    pub operations: Vec<Operation>,
}

#[derive(Deserialize)]
struct RawOperations {
    operations: Vec<RawOperation>,
}

#[derive(Deserialize)]
struct RawOperation {
    id: String,
    method: String,
    path: String,
    scope: String,
    success_status: u16,
    #[serde(default)]
    required_headers: Vec<String>,
}

#[derive(Deserialize)]
struct RawMcp {
    tools: Vec<RawTool>,
}

#[derive(Deserialize)]
struct RawTool {
    tool: String,
    permission: String,
    api_operation_id: String,
    api_scope: String,
}

impl Catalog {
    /// Catálogo de `docs/contracts/api-operations.json` + `mcp-permissions.json`.
    ///
    /// # Errors
    /// `INTERNAL` si los contratos no se pueden interpretar o se contradicen.
    pub fn from_contracts(operations_json: &str, mcp_json: &str) -> Result<Self, LarmError> {
        let broken = |detail: String| {
            LarmError::new(
                ErrorCode::Internal,
                "Contrato de operaciones ilegible",
                "contract_invalid",
            )
            .with_technical(detail)
        };
        let raw: RawOperations =
            serde_json::from_str(operations_json).map_err(|e| broken(e.to_string()))?;
        let mcp: RawMcp = serde_json::from_str(mcp_json).map_err(|e| broken(e.to_string()))?;
        let mut tools: BTreeMap<String, (String, McpPermission, String)> = BTreeMap::new();
        for t in mcp.tools {
            let permission = McpPermission::parse(&t.permission)
                .ok_or_else(|| broken(format!("permiso MCP desconocido {}", t.permission)))?;
            tools.insert(t.api_operation_id, (t.tool, permission, t.api_scope));
        }
        let mut operations = Vec::with_capacity(raw.operations.len());
        for op in raw.operations {
            let scope = match op.scope.as_str() {
                "public" => OperationScope::Public,
                "oauth_nonce" => OperationScope::OauthNonce,
                other => OperationScope::Api(
                    ApiScope::parse(other)
                        .ok_or_else(|| broken(format!("scope desconocido {other} en {}", op.id)))?,
                ),
            };
            let mcp_tool = match tools.remove(&op.id) {
                Some((tool, permission, api_scope)) => {
                    if OperationScope::Api(ApiScope::parse(&api_scope).unwrap_or(ApiScope::Admin))
                        != scope
                    {
                        return Err(broken(format!(
                            "{tool}: api_scope {api_scope} distinto del de {}",
                            op.id
                        )));
                    }
                    Some((tool, permission))
                }
                None => None,
            };
            operations.push(Operation {
                id: op.id,
                method: op.method,
                path: op.path,
                scope,
                success_status: op.success_status,
                required_headers: op.required_headers,
                mcp_tool,
            });
        }
        if let Some(orphan) = tools.keys().next() {
            return Err(broken(format!("herramienta MCP sin operación: {orphan}")));
        }
        Ok(Self { operations })
    }

    /// Catálogo del contrato vigente, embebido en el binario.
    #[must_use]
    pub fn contract() -> &'static Catalog {
        static CATALOG: OnceLock<Catalog> = OnceLock::new();
        CATALOG.get_or_init(|| {
            Catalog::from_contracts(
                include_str!("../../../docs/contracts/api-operations.json"),
                include_str!("../../../docs/contracts/mcp-permissions.json"),
            )
            .unwrap_or_else(|e| {
                unreachable!("contratos embebidos inválidos: {}", e.technical_detail())
            })
        })
    }

    /// Operación por id del contrato.
    #[must_use]
    pub fn by_id(&self, id: &str) -> Option<&Operation> {
        self.operations.iter().find(|o| o.id == id)
    }

    /// Operación que atiende `method path`; las rutas literales ganan a las de parámetro.
    #[must_use]
    pub fn find(&self, method: &str, path: &str) -> Option<&Operation> {
        self.operations
            .iter()
            .filter(|o| o.matches(method, path))
            .min_by_key(|o| o.path.matches('{').count())
    }
}

/// Decide si `principal` puede ejecutar `operation`. Se llama antes de leer el cuerpo.
///
/// # Errors
/// `FORBIDDEN` si falta el scope de API, el permiso MCP, o la operación no es
/// accesible con esa audiencia.
pub fn authorize(principal: &Principal, operation: &Operation) -> Result<(), LarmError> {
    let forbid = |message: String, reason: &str| {
        Err(
            LarmError::new(ErrorCode::Forbidden, message, reason.to_string())
                .with_resource(operation.id.clone()),
        )
    };
    let scope = match operation.scope {
        OperationScope::Public => return Ok(()),
        OperationScope::OauthNonce => {
            return forbid(
                "Esta operación solo la autoriza su flujo OAuth".into(),
                "oauth_nonce_only",
            )
        }
        OperationScope::Api(scope) => scope,
    };
    if principal.kind == PrincipalKind::Mcp {
        // H06: misma restricción por MCP y por API directa.
        let Some((tool, permission)) = &operation.mcp_tool else {
            return forbid(
                "Operación no disponible para un principal MCP".into(),
                "mcp_operation_unmapped",
            );
        };
        if !principal.mcp_scopes.contains(permission)
            && !principal.mcp_scopes.contains(&McpPermission::Admin)
        {
            return forbid(
                format!("Falta el permiso MCP {permission} para {tool}"),
                "mcp_permission_missing",
            );
        }
    }
    if !principal.has_api_scope(scope) {
        return forbid(format!("Falta el scope {scope}"), "scope_missing");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    const ID: &str = "0192aaaa-0000-7000-8000-000000000001";

    fn mcp(api: &[ApiScope], perms: &[McpPermission]) -> Principal {
        Principal::new(
            ID,
            "athena",
            PrincipalKind::Mcp,
            api.iter().copied(),
            perms.iter().copied(),
        )
        .unwrap()
    }

    fn op(id: &str) -> &'static Operation {
        Catalog::contract()
            .by_id(id)
            .unwrap_or_else(|| panic!("operación {id}"))
    }

    fn allowed(p: &Principal, id: &str) -> bool {
        authorize(p, op(id)).is_ok()
    }

    #[test]
    fn el_catalogo_del_contrato_carga_completo() {
        let c = Catalog::contract();
        // Mantiene el catálogo sincronizado con las operaciones del contrato actual.
        assert_eq!(c.operations.len(), 154);
        let promotion = c
            .find("PATCH", "/api/v1/variants/{id}/promotion")
            .expect("la operación de promoción P120 debe estar en el catálogo");
        assert_eq!(promotion.id, "P120_patch__variants_id_promotion");
        assert_eq!(promotion.scope, OperationScope::Api(ApiScope::ModelsWrite));
        assert_eq!(promotion.success_status, 200);
        let restart = c
            .find("POST", &format!("/api/v1/transformations/{ID}/restart"))
            .unwrap();
        assert_eq!(restart.id, "P093_post__transformations_id_restart");
        assert_eq!(restart.scope, OperationScope::Api(ApiScope::JobsWrite));
        assert_eq!(restart.success_status, 202);
        assert_eq!(
            restart.required_headers,
            vec!["Idempotency-Key", "If-Match"]
        );
        assert_eq!(
            c.operations.iter().filter(|o| o.mcp_tool.is_some()).count(),
            12
        );
        assert!(c
            .by_id("P099_get__runtime_profile_locks_lock_sha256_")
            .is_some());
        assert!(c
            .by_id("P099_put__runtime_profile_locks_lock_sha256_")
            .is_some());
        assert_eq!(
            c.find(
                "GET",
                "/api/v1/credentials/0192aaaa-0000-7000-8000-000000000001"
            )
            .unwrap()
            .id,
            "P009_get__credentials_id_"
        );
        assert_eq!(
            c.find("get", "/api/v1/credentials").unwrap().id,
            "P009_get__credentials"
        );
        assert!(c.find("PATCH", "/api/v1/credentials").is_none());
    }

    #[test]
    fn h06_start_sin_stop_e_install_sin_update() {
        let api = [ApiScope::LeasesManage, ApiScope::RuntimesWrite];
        let p = mcp(&api, &[McpPermission::Start, McpPermission::Install]);
        assert!(allowed(&p, "P100_post__leases"));
        assert!(!allowed(&p, "P033_post__instances_id_stop"));
        assert!(allowed(&p, "P031_post__runtime_installations"));
        assert!(!allowed(&p, "P032_post__runtime_versions_id_update"));
    }

    #[test]
    fn h06_read_search_y_move_delete_son_separados() {
        let p = mcp(
            &[
                ApiScope::Read,
                ApiScope::StorageWrite,
                ApiScope::ModelsWrite,
            ],
            &[McpPermission::Read, McpPermission::Move],
        );
        assert!(allowed(&p, "P010_get__hardware"));
        assert!(!allowed(&p, "P044_post__models_search"));
        assert!(allowed(&p, "P016_post__models_id_move"));
        assert!(!allowed(&p, "P018_delete__models_id_"));
    }

    #[test]
    fn h06_sin_scope_api_se_niega_aunque_haya_permiso_mcp() {
        let p = mcp(
            &[ApiScope::Read],
            &[McpPermission::Start, McpPermission::Admin],
        );
        let e = authorize(&p, op("P100_post__leases")).unwrap_err();
        assert_eq!(
            (e.code, e.details.reason.as_str()),
            (ErrorCode::Forbidden, "scope_missing")
        );
    }

    #[test]
    fn h06_admin_mcp_no_anade_scopes_api_y_admin_api_no_anade_herramientas() {
        let mcp_admin = mcp(&[ApiScope::Read], &[McpPermission::Admin]);
        assert!(allowed(&mcp_admin, "P010_get__hardware"));
        assert!(!allowed(&mcp_admin, "P016_post__models_id_move"));
        let api_admin = mcp(&[ApiScope::Admin], &[McpPermission::Read]);
        assert!(allowed(&api_admin, "P010_get__hardware"));
        assert_eq!(
            authorize(&api_admin, op("P100_post__leases"))
                .unwrap_err()
                .details
                .reason,
            "mcp_permission_missing"
        );
    }

    #[test]
    fn h06_mcp_por_api_directa_a_operacion_sin_herramienta_se_niega() {
        let p = mcp(&[ApiScope::Admin], &[McpPermission::Admin]);
        let e = authorize(&p, op("P009_get__credentials")).unwrap_err();
        assert_eq!(e.details.reason, "mcp_operation_unmapped");
    }

    #[test]
    fn broker_solo_read_y_leases_manage() {
        let ok = Principal::new(
            ID,
            "ai-broker",
            PrincipalKind::Broker,
            [ApiScope::Read, ApiScope::LeasesManage],
            [],
        )
        .unwrap();
        assert!(allowed(&ok, "P100_post__leases"));
        assert!(
            !allowed(&ok, "P009_get__credentials"),
            "credentials:read es distinto de read"
        );
        for extra in [
            ApiScope::Admin,
            ApiScope::CredentialsRead,
            ApiScope::ModelsWrite,
        ] {
            let e = Principal::new(
                ID,
                "ai-broker",
                PrincipalKind::Broker,
                [ApiScope::Read, extra],
                [],
            )
            .unwrap_err();
            assert_eq!(e.details.reason, "broker_scope_not_allowed");
        }
    }

    #[test]
    fn mcp_scopes_solo_en_principales_mcp() {
        let e = Principal::new(
            ID,
            "gui",
            PrincipalKind::Desktop,
            [ApiScope::Read],
            [McpPermission::Read],
        )
        .unwrap_err();
        assert_eq!(e.details.reason, "mcp_scopes_not_allowed");
        let e = Principal::new(ID, "athena", PrincipalKind::Mcp, [ApiScope::Read], []).unwrap_err();
        assert_eq!(e.details.reason, "mcp_scopes_missing");
        assert!(Principal::new(ID, "nombre con espacio", PrincipalKind::Operator, [], []).is_err());
    }

    #[test]
    fn admin_api_cubre_scopes_salvo_rutas_publicas_y_oauth() {
        let admin = Principal::new(
            ID,
            "operator",
            PrincipalKind::Operator,
            [ApiScope::Admin],
            [],
        )
        .unwrap();
        let c = Catalog::contract();
        for o in &c.operations {
            let r = authorize(&admin, o);
            match o.scope {
                OperationScope::OauthNonce => assert!(r.is_err(), "{}", o.id),
                _ => assert!(r.is_ok(), "{}", o.id),
            }
        }
        let reader =
            Principal::new(ID, "lector", PrincipalKind::Operator, [ApiScope::Read], []).unwrap();
        assert!(!allowed(&reader, "P009_post__approvals"));
        assert!(allowed(&reader, "P005_get__health_live"));
    }

    #[test]
    fn scopes_y_permisos_roundtrip_y_desconocidos_rechazados() {
        for s in ApiScope::ALL {
            assert_eq!(ApiScope::parse(s.as_str()), Some(*s));
        }
        assert_eq!(ApiScope::ALL.len(), 12);
        assert_eq!(McpPermission::ALL.len(), 11);
        assert!(serde_json::from_str::<ApiScope>("\"root\"").is_err());
        assert!(serde_json::from_str::<McpPermission>("\"execute\"").is_err());
    }

    #[test]
    fn contrato_contradictorio_se_rechaza() {
        let ops = r#"{"operations":[{"id":"x","method":"GET","path":"/a","scope":"read","success_status":200}]}"#;
        let bad = r#"{"tools":[{"tool":"t","permission":"read","api_operation_id":"x","api_scope":"admin"}]}"#;
        assert!(Catalog::from_contracts(ops, bad).is_err());
        let orphan = r#"{"tools":[{"tool":"t","permission":"read","api_operation_id":"y","api_scope":"read"}]}"#;
        assert!(Catalog::from_contracts(ops, orphan).is_err());
        let unknown = r#"{"operations":[{"id":"x","method":"GET","path":"/a","scope":"superuser","success_status":200}]}"#;
        assert!(Catalog::from_contracts(unknown, r#"{"tools":[]}"#).is_err());
    }
}
