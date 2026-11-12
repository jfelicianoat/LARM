//! Perfiles locales: sólo recetas compiladas; Probe nunca recibe este documento.
use async_trait::async_trait;
use larm_adapter_host::conformance::profiles::LocalProfile;
use larm_adapter_host::conformance::{NativeTarget, NativeTargets};
use larm_domain::adapters::AdapterInstallation;
use larm_domain::conformance::hash;
use larm_domain::conformance_profiles::{invalid, ConformanceProfile};
use larm_domain::error::{ErrorCode, LarmError};
use larm_domain::ports::{
    AdapterInstallationRepository, ApprovalRepository, ConformanceProfileRepository,
    MutationContext,
};
use std::path::Path;

pub struct PersistentTargets {
    pub repo: larm_db::conformance_profiles::SqlConformanceProfiles,
}
#[async_trait]
impl NativeTargets for PersistentTargets {
    async fn target(
        &self,
        a: &AdapterInstallation,
        ctx: &MutationContext,
    ) -> Result<NativeTarget, LarmError> {
        let p = self.repo.conformance_profile(&a.id).await?.ok_or_else(|| {
            LarmError::new(
                ErrorCode::UnsupportedCapability,
                "Falta el perfil de conformidad",
                "conformance_suite_unavailable",
            )
        })?;
        let config: LocalProfile = serde_json::from_value(p.configuration.clone())
            .map_err(|_| invalid("conformance_profile_configuration_invalid"))?;
        config.target(&p, a, ctx)
    }
}
/// Administración local por el operador de Windows. No concede VALIDATED.
pub async fn register(
    db: &larm_db::pool::Db,
    data_root: &Path,
    installation: &str,
    actor: &str,
    approval: &str,
    file: &Path,
    expected: Option<&str>,
) -> Result<ConformanceProfile, LarmError> {
    use std::io::Read;
    larm_adapter_host::package::check_managed_path(file)?;
    let mut bytes = Vec::new();
    std::fs::File::open(file)
        .map_err(|_| invalid("conformance_profile_file_unreadable"))?
        .take((1 << 20) + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| invalid("conformance_profile_file_unreadable"))?;
    if bytes.len() > 1 << 20 {
        return Err(invalid("conformance_profile_file_too_large"));
    }
    let configuration: LocalProfile = serde_json::from_slice(&bytes)
        .map_err(|_| invalid("conformance_profile_configuration_invalid"))?;
    let a = larm_db::adapters::SqlAdapterInstallations {
        pool: db.pool.clone(),
    }
    .get(installation)
    .await?
    .ok_or_else(|| invalid("conformance_profile_subject_missing"))?;
    larm_adapter_host::package::component(&a.adapter_id)?;
    larm_adapter_host::package::component(&a.version)?;
    let expected_package = data_root
        .join("adapters")
        .join(&a.adapter_id)
        .join(&a.version);
    larm_adapter_host::package::check_managed_path(&expected_package)?;
    let package = std::fs::canonicalize(&expected_package)
        .map_err(|_| invalid("conformance_profile_package_missing"))?;
    larm_adapter_host::package::check_managed_path(&configuration.package_dir)?;
    if std::fs::canonicalize(&configuration.package_dir)
        .ok()
        .as_ref()
        != Some(&package)
    {
        return Err(invalid("conformance_profile_package_mismatch"));
    }
    let record = larm_db::security::SqlApprovals {
        pool: db.pool.clone(),
    }
    .get(approval)
    .await?
    .ok_or_else(|| invalid("conformance_profile_approval_missing"))?;
    let p = ConformanceProfile {
        id: uuid::Uuid::now_v7().to_string(),
        installation_id: a.id.clone(),
        actor_id: actor.into(),
        manifest_sha256: hash(&a.manifest)?,
        configuration: serde_json::to_value(&configuration)
            .map_err(|_| invalid("conformance_profile_configuration_invalid"))?,
        approval: record,
        created_at: larm_domain::bridge::timestamp(chrono::Utc::now()),
    };
    let ctx = MutationContext {
        actor_id: actor.into(),
        correlation_id: uuid::Uuid::now_v7().to_string(),
    };
    let target = configuration.target(&p, &a, &ctx)?;
    larm_adapter_host::package::verify_published(
        &package,
        &target.spec.manifest,
        &Default::default(),
    )?;
    for input in &target.immutable_inputs {
        larm_adapter_host::package::check_managed_path(input)?;
        if !input.is_file() {
            return Err(invalid("conformance_profile_input_missing"));
        }
    }
    // Se consume en la misma transacción que el perfil, nunca antes de su commit.
    larm_db::conformance_profiles::SqlConformanceProfiles {
        pool: db.pool.clone(),
    }
    .store_conformance_profile(&p, expected, &ctx)
    .await?;
    Ok(p)
}
