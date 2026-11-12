//! Composición de operaciones locales de instalación de runtime.

use larm_domain::{
    error::{ErrorCode, LarmError},
    ports::MutationContext,
    runtime_install::InstallRequest,
};
use std::{io::Read, path::Path, sync::Arc};

const MAX_REQUEST_BYTES: u64 = 64 * 1024;

fn invalid(reason: &str) -> LarmError {
    LarmError::new(
        ErrorCode::ValidationFailed,
        "No se pudo previsualizar la instalación runtime",
        reason,
    )
}

fn read_request(request_file: &Path) -> Result<InstallRequest, LarmError> {
    let file = std::fs::File::open(request_file)
        .map_err(|_| invalid("runtime_install_request_read_failed"))?;
    let mut bytes = Vec::new();
    file.take(MAX_REQUEST_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| invalid("runtime_install_request_read_failed"))?;
    if bytes.len() as u64 > MAX_REQUEST_BYTES {
        return Err(invalid("runtime_install_request_too_large"));
    }
    serde_json::from_slice(&bytes).map_err(|_| invalid("runtime_install_request_invalid"))
}

/// Lee una solicitud cerrada, planifica mediante el worker cualificado y drena
/// el runner. El caso de uso sólo devuelve el plan; no admite un job.
pub async fn plan_only(
    db: &larm_db::pool::Db,
    request_file: &Path,
    actor_id: &str,
) -> Result<larm_domain::artifacts::Plan, LarmError> {
    let request = read_request(request_file)?;

    let runtimes = Arc::new(larm_db::runtimes::SqlRuntimes {
        pool: db.pool.clone(),
    });
    let runner = crate::probes::runner(db);
    let service = larm_runtime::install_service::RuntimeInstallService::new(
        runtimes.clone(),
        runtimes,
        Arc::new(larm_db::runtime_profiles::SqlRuntimeProfileLocks {
            pool: db.pool.clone(),
        }),
        Arc::new(larm_db::runtime_profiles::SqlRuntimeProfileQualifications {
            pool: db.pool.clone(),
        }),
        Arc::new(larm_adapter_host::runtime::discovery::NativeDiscoveryHost {
            runner: runner.clone(),
            access: Arc::new(crate::runtime_discovery::ManifestDiscoveryAccess),
        }),
        // plan_only nunca llama a este repositorio; se incluye porque el
        // servicio comparte composición con la admisión de jobs.
        Arc::new(larm_db::runtimes::SqlRuntimeInstallJobs {
            pool: db.pool.clone(),
        }),
        None,
    );
    let ctx = MutationContext {
        actor_id: actor_id.into(),
        correlation_id: uuid::Uuid::now_v7().to_string(),
    };
    let result = service.plan_only(&request, &ctx).await;
    let drained = runner.shutdown().await;
    match result {
        Err(error) => Err(error),
        Ok(plan) => {
            drained?;
            Ok(plan)
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use std::io::Write;

    #[test]
    fn request_file_is_bounded_and_closed() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        let request = serde_json::json!({
            "runtime_id": uuid::Uuid::now_v7().to_string(),
            "upstream_version": "v1",
            "strategy": "PREBUILT_BINARY",
            "storage_location_id": uuid::Uuid::now_v7().to_string(),
            "profile_lock_hash": "a".repeat(64),
            "external_host_id": null
        });
        serde_json::to_writer(&mut file, &request).unwrap();
        file.flush().unwrap();
        assert!(read_request(file.path()).is_ok());

        let mut extra = request;
        extra["unexpected"] = serde_json::json!(true);
        std::fs::write(file.path(), serde_json::to_vec(&extra).unwrap()).unwrap();
        assert_eq!(
            read_request(file.path()).unwrap_err().details.reason,
            "runtime_install_request_invalid"
        );

        std::fs::write(file.path(), vec![b' '; MAX_REQUEST_BYTES as usize + 1]).unwrap();
        assert_eq!(
            read_request(file.path()).unwrap_err().details.reason,
            "runtime_install_request_too_large"
        );
    }
}
