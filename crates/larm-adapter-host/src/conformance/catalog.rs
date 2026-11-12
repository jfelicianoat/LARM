//! Registro del composition root. No carga suites ni planes JSON del cliente.
use super::*;
#[derive(Default)]
pub struct NativeCatalog {
    targets: Mutex<BTreeMap<String, NativeTarget>>,
}
impl NativeCatalog {
    pub fn register(&self, installation_id: &str, target: NativeTarget) -> Result<(), LarmError> {
        if !larm_domain::bridge::is_uuid_v7(installation_id) {
            return Err(invalid("conformance_installation_invalid"));
        }
        let mut targets = self
            .targets
            .lock()
            .map_err(|_| error(ErrorCode::Internal, "conformance_catalog_unavailable"))?;
        if targets.contains_key(installation_id) {
            return Err(error(
                ErrorCode::Conflict,
                "conformance_target_already_registered",
            ));
        }
        targets.insert(installation_id.into(), target);
        Ok(())
    }
}
#[async_trait]
impl NativeTargets for NativeCatalog {
    async fn target(
        &self,
        installation: &AdapterInstallation,
        _: &MutationContext,
    ) -> Result<NativeTarget, LarmError> {
        self.targets
            .lock()
            .map_err(|_| error(ErrorCode::Internal, "conformance_catalog_unavailable"))?
            .get(&installation.id)
            .cloned()
            .ok_or_else(|| {
                error(
                    ErrorCode::UnsupportedCapability,
                    "conformance_suite_unavailable",
                )
            })
    }
}
