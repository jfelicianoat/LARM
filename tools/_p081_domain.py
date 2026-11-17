# -*- coding: utf-8 -*-
"""P081 sub 3 (dominio): ExecutionHost::validate, InstallRequest.execution_host_id
con coherencia, y los tres métodos del trait RuntimeRepository."""
import io

def edit(path, pairs):
    s = io.open(path, encoding='utf-8', newline='').read()
    for old, new in pairs:
        assert s.count(old) == 1, (path, old[:80], s.count(old))
        s = s.replace(old, new)
    io.open(path, 'w', encoding='utf-8', newline='').write(s)
    print('editado', path)

# --- runtime.rs: validate del DTO ExecutionHost (tras su definición) ---
edit('crates/larm-domain/src/runtime.rs', [(
'''/// P081: host de ejecución virtualizado observado read-only (registro de
/// WSL2/motor de contenedores). Nunca acredita compatibilidad ni habilita
/// ejecución: es observación estructurada con su evidencia.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionHost {
    pub id: String,
    pub created_at: String,
    pub updated_at: String,
    pub row_version: i64,
    pub metadata: Map<String, Value>,
    pub kind: ExecutionHostKind,
    pub name: String,
    #[serde(deserialize_with = "Option::deserialize")]
    pub engine_version: Option<String>,
    /// 1|2 para hosts WSL2 (de `wsl --list --verbose`); null en contenedores.
    #[serde(deserialize_with = "Option::deserialize")]
    pub wsl_version: Option<u8>,
    pub default: bool,
    pub status: ExternalHostState,
    pub evidence_refs: Vec<String>,
}''',
'''/// P081: host de ejecución virtualizado observado read-only (registro de
/// WSL2/motor de contenedores). Nunca acredita compatibilidad ni habilita
/// ejecución: es observación estructurada con su evidencia.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionHost {
    pub id: String,
    pub created_at: String,
    pub updated_at: String,
    pub row_version: i64,
    pub metadata: Map<String, Value>,
    pub kind: ExecutionHostKind,
    pub name: String,
    #[serde(deserialize_with = "Option::deserialize")]
    pub engine_version: Option<String>,
    /// 1|2 para hosts WSL2 (de `wsl --list --verbose`); null en contenedores.
    #[serde(deserialize_with = "Option::deserialize")]
    pub wsl_version: Option<u8>,
    pub default: bool,
    pub status: ExternalHostState,
    pub evidence_refs: Vec<String>,
}

impl ExecutionHost {
    pub fn validate(&self) -> Result<(), LarmError> {
        record(&self.id, &self.created_at, &self.updated_at, self.row_version)?;
        if self.name.is_empty()
            || self.name.len() > 256
            || self.name.chars().any(char::is_control)
            || self.name.starts_with('.')
            || self.name.contains(['/', '\\\\', ':'])
        {
            return Err(invalid("execution_host_name_invalid"));
        }
        if self.engine_version.as_deref().is_some_and(|v| v.is_empty() || v.len() > 64) {
            return Err(invalid("execution_host_engine_version_invalid"));
        }
        if self.wsl_version.is_some() != (self.kind == ExecutionHostKind::Wsl2) {
            return Err(invalid("execution_host_wsl_version_invalid"));
        }
        if self.evidence_refs.len() > 64
            || self
                .evidence_refs
                .iter()
                .any(|reference| reference.is_empty() || reference.len() > 512)
        {
            return Err(invalid("execution_host_evidence_invalid"));
        }
        Ok(())
    }
}''')])

# --- runtime_install.rs: campo + coherencia en validate ---
edit('crates/larm-domain/src/runtime_install.rs', [(
'''#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstallRequest {
    pub runtime_id: String,
    pub upstream_version: String,
    pub strategy: InstallationStrategy,
    pub storage_location_id: String,
    pub profile_lock_hash: String,
    #[serde(deserialize_with = "Option::deserialize")]
    pub external_host_id: Option<String>,
}''',
'''#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstallRequest {
    pub runtime_id: String,
    pub upstream_version: String,
    pub strategy: InstallationStrategy,
    pub storage_location_id: String,
    pub profile_lock_hash: String,
    #[serde(deserialize_with = "Option::deserialize")]
    pub external_host_id: Option<String>,
    /// P081: host de ejecución referenciado para estrategias WSL/CONTAINER.
    #[serde(deserialize_with = "Option::deserialize")]
    pub execution_host_id: Option<String>,
}'''), (
'''        if let Some(host) = &self.external_host_id {
            id(host)?;
        }
        if self.upstream_version.is_empty() {
            return Err(invalid("upstream_version_invalid"));
        }
        Ok(())
    }''',
'''        if let Some(host) = &self.external_host_id {
            id(host)?;
        }
        if let Some(host) = &self.execution_host_id {
            id(host)?;
        }
        // P081: coherencia estructural sin base de datos — las estrategias
        // virtualizadas exigen host referenciado y las nativas lo prohíben.
        let virtualized = matches!(
            self.strategy,
            InstallationStrategy::Wsl | InstallationStrategy::Container
        );
        if virtualized && self.execution_host_id.is_none() {
            return Err(invalid("execution_host_reference_missing"));
        }
        if !virtualized && self.execution_host_id.is_some() {
            return Err(invalid("execution_host_reference_unexpected"));
        }
        if self.upstream_version.is_empty() {
            return Err(invalid("upstream_version_invalid"));
        }
        Ok(())
    }''')])

# --- runtime_ports.rs: los tres métodos del trait ---
edit('crates/larm-domain/src/runtime_ports.rs', [(
'''    async fn record_external_host(
        &self,
        host: &ExternalHost,
        ctx: &MutationContext,
    ) -> Result<(), LarmError>;
    async fn external_host(&self, id: &str) -> Result<Option<ExternalHost>, LarmError>;
    async fn external_hosts(
        &self,
        after: Option<(String, String)>,
        limit: u32,
        filter: &RuntimeFilter,
    ) -> Result<Vec<ExternalHost>, LarmError>;
}''',
'''    async fn record_external_host(
        &self,
        host: &ExternalHost,
        ctx: &MutationContext,
    ) -> Result<(), LarmError>;
    async fn external_host(&self, id: &str) -> Result<Option<ExternalHost>, LarmError>;
    async fn external_hosts(
        &self,
        after: Option<(String, String)>,
        limit: u32,
        filter: &RuntimeFilter,
    ) -> Result<Vec<ExternalHost>, LarmError>;
    /// P081: registro read-only de hosts de ejecución observados. El registro
    /// no habilita ejecución ni acredita compatibilidad.
    async fn record_execution_host(
        &self,
        host: &ExecutionHost,
        ctx: &MutationContext,
    ) -> Result<(), LarmError>;
    async fn execution_host(&self, id: &str) -> Result<Option<ExecutionHost>, LarmError>;
    async fn execution_hosts(
        &self,
        after: Option<(String, String)>,
        limit: u32,
        q: Option<&str>,
        status: Option<&str>,
    ) -> Result<Vec<ExecutionHost>, LarmError>;
}''')])
print('dominio sub 3 listo')
