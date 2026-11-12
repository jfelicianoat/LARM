//! Inspección y selección del registro (P024). Ningún comando ejecuta paquetes.
use larm_adapter_host::package::ArchivePlan;
use larm_adapter_host::registry::{LocalFolderRegistry, RegistrySource};
use larm_domain::error::{ErrorCode, LarmError};
use larm_domain::ports::{AdapterInstallationRepository, AdapterPackageExporter, MutationContext};
use std::sync::Arc;

pub struct PackageExportService {
    db: larm_db::pool::Db,
    installations: Arc<dyn AdapterInstallationRepository>,
    root: std::path::PathBuf,
}

impl PackageExportService {
    pub fn new(
        db: larm_db::pool::Db,
        installations: Arc<dyn AdapterInstallationRepository>,
        root: std::path::PathBuf,
    ) -> Result<Arc<Self>, LarmError> {
        let root = std::fs::canonicalize(root).map_err(|error| {
            LarmError::new(
                ErrorCode::ResourceUnavailable,
                "La raíz de adaptadores no está disponible",
                "adapter_root_invalid",
            )
            .with_technical(error.to_string())
        })?;
        Ok(Arc::new(Self {
            db,
            installations,
            root,
        }))
    }
}

#[async_trait::async_trait]
impl AdapterPackageExporter for PackageExportService {
    async fn export_package(
        &self,
        request: &larm_domain::adapter_packages::AdapterExport,
        ctx: &MutationContext,
    ) -> Result<larm_domain::adapter_packages::AdapterExportReceipt, LarmError> {
        request.validate()?;
        let installation = self
            .installations
            .get(&request.adapter_installation_id)
            .await?
            .ok_or_else(|| {
                LarmError::new(
                    ErrorCode::NotFound,
                    "Instalación de adaptador desconocida",
                    "not_found",
                )
                .with_resource(&request.adapter_installation_id)
            })?;
        if !matches!(
            installation.state,
            larm_domain::adapters::AdapterState::Installed
                | larm_domain::adapters::AdapterState::Validated
        ) {
            return Err(LarmError::new(
                ErrorCode::Conflict,
                "El adaptador no está instalado",
                "adapter_export_state_invalid",
            )
            .with_resource(&installation.id));
        }
        larm_adapter_host::package::component(&installation.adapter_id)?;
        larm_adapter_host::package::component(&installation.version)?;
        let source = self
            .root
            .join(&installation.adapter_id)
            .join(&installation.version);
        let destination = std::path::PathBuf::from(&request.destination_path);
        let control = larm_adapter_host::package::PackageControl::default();
        let export_source = source.clone();
        let export_destination = destination.clone();
        let exported = tokio::task::spawn_blocking(move || {
            larm_adapter_host::package::export_published_package(
                &export_source,
                &export_destination,
                &control,
            )
        })
        .await
        .map_err(|error| {
            LarmError::new(
                ErrorCode::Internal,
                "Falló la tarea de exportación",
                "adapter_export_task_failed",
            )
            .with_technical(error.to_string())
        })??;

        let control = larm_adapter_host::package::PackageControl::default();
        let archive = exported.destination.clone();
        let archive_hash = exported.archive_hash.clone();
        let plan = tokio::task::spawn_blocking(move || {
            larm_adapter_host::package::ArchivePlan::inspect(&archive, &archive_hash, &control)
        })
        .await
        .map_err(|error| {
            LarmError::new(
                ErrorCode::Internal,
                "Falló la verificación del ZIP exportado",
                "adapter_export_verify_task_failed",
            )
            .with_technical(error.to_string())
        })??;
        if plan.manifest.id != installation.adapter_id
            || plan.manifest.version != installation.version
            || plan.manifest.package_sha256 != installation.package_hash
        {
            let _ = std::fs::remove_file(&exported.destination);
            return Err(LarmError::new(
                ErrorCode::IntegrityFailed,
                "El ZIP no coincide con la instalación seleccionada",
                "adapter_export_installation_mismatch",
            )
            .with_resource(&installation.id));
        }

        let mut tx = self.db.pool.begin().await.map_err(|error| {
            LarmError::new(
                ErrorCode::ResourceUnavailable,
                "No se pudo registrar la exportación",
                "adapter_export_event_failed",
            )
            .with_technical(error.to_string())
        })?;
        let event_id = match larm_db::events::append(
            &mut tx,
            &larm_db::events::NewEvent {
                event_type: "adapter.exported",
                resource_type: "adapter_installation",
                resource_id: &installation.id,
                resource_version: installation.row_version,
                data: serde_json::json!({
                    "adapter_id": &installation.adapter_id,
                    "version": &installation.version,
                    "archive_sha256": &exported.archive_hash,
                    "archive_bytes": exported.archive_bytes.to_string(),
                }),
            },
            ctx,
        )
        .await
        {
            Ok(event_id) => event_id,
            Err(error) => {
                drop(tx);
                let _ = std::fs::remove_file(&exported.destination);
                return Err(error);
            }
        };
        tx.commit().await.map_err(|error| {
            LarmError::new(
                ErrorCode::ResourceUnavailable,
                "No se pudo confirmar la exportación",
                "adapter_export_event_commit_failed",
            )
            .with_technical(error.to_string())
        })?;

        Ok(larm_domain::adapter_packages::AdapterExportReceipt {
            resource_id: installation.id,
            row_version: installation.row_version,
            event_id,
        })
    }
}

fn summary(plan: &ArchivePlan) -> serde_json::Value {
    serde_json::json!({"adapter_id":plan.manifest.id,"version":plan.manifest.version,
        "family":plan.manifest.family,"archive_sha256":plan.archive_hash,
        "package_sha256":plan.manifest.package_sha256,"archive_bytes":plan.archive_bytes.to_string(),
        "unpacked_bytes":plan.unpacked_bytes.to_string(),"required_free_bytes":plan.required_bytes().to_string()})
}

pub fn inspect(command: &Option<crate::Command>) -> i32 {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(e) => {
            eprintln!("No se pudo preparar la inspección: {e}");
            return 1;
        }
    };
    let result = runtime.block_on(async {
        let plans = match command {
            Some(crate::Command::InspectAdapterRegistry { folder }) => {
                LocalFolderRegistry {
                    root: folder.clone(),
                }
                .packages()
                .await?
            }
            Some(crate::Command::InspectAdapterPackage {
                file,
                expected_hash,
            }) => vec![
                larm_adapter_host::package::inspect_async(file.clone(), expected_hash.clone())
                    .await?,
            ],
            _ => {
                return Err(LarmError::new(
                    ErrorCode::ValidationFailed,
                    "Comando de inspección desconocido",
                    "inspection_command_invalid",
                ))
            }
        };
        serde_json::to_string_pretty(
            &serde_json::json!({"packages":plans.iter().map(summary).collect::<Vec<_>>()}),
        )
        .map_err(|e| {
            LarmError::new(
                ErrorCode::Internal,
                "No se pudo serializar la inspección",
                "serialization_failed",
            )
            .with_technical(e.to_string())
        })
    });
    runtime.shutdown_timeout(std::time::Duration::from_secs(1));
    match result {
        Ok(json) => {
            println!("{json}");
            0
        }
        Err(error) => {
            eprintln!("{}", serde_json::to_string(&error).unwrap_or_default());
            1
        }
    }
}

/// Operación local bajo la identidad Windows del operador (igual que P009).
/// El esquema público no añade rutas de promoción/rollback; ésta es la
/// herramienta local. Las ejecuciones siguen pasando por HostPolicy.
pub struct SelectionContext {
    pub mutation: larm_domain::ports::MutationContext,
    pub qualification: std::sync::Arc<dyn larm_domain::ports::AdapterQualification>,
}
pub async fn selection(
    db: &larm_db::pool::Db,
    adapter_id: &str,
    promote: Option<&str>,
    rollback: bool,
    enabled: Option<bool>,
    expected_revision: Option<i64>,
    context: SelectionContext,
) -> Result<serde_json::Value, LarmError> {
    use larm_domain::adapters::AdapterSelectionChange;
    use larm_domain::ports::AdapterSelectionRepository;

    larm_adapter_host::package::component(adapter_id)?;
    let change = if let Some(id) = promote {
        Some(AdapterSelectionChange::Promote {
            installation_id: id.into(),
        })
    } else if rollback {
        Some(AdapterSelectionChange::Rollback)
    } else {
        enabled.map(|enabled| AdapterSelectionChange::SetEnabled { enabled })
    };
    let repo = larm_db::adapter_selection::SqlAdapterSelections {
        pool: db.pool.clone(),
        qualification: Some(context.qualification),
    };
    let result = match (change, expected_revision) {
        (Some(change), Some(revision)) => Some(
            repo.change_selection(adapter_id, revision, &change, &context.mutation)
                .await?,
        ),
        (None, None) => repo.selection(adapter_id).await?,
        _ => {
            return Err(LarmError::new(
                ErrorCode::ValidationFailed,
                "Un cambio necesita su revisión esperada",
                "adapter_selection_revision_required",
            ))
        }
    };
    Ok(serde_json::json!({"selection":result}))
}

const REGISTRY_INDEX_MAX_BYTES: usize = 16 * 1024 * 1024;

#[async_trait::async_trait]
trait RegistryIndexFetcher: Send + Sync {
    async fn fetch(&self, url: &str) -> Result<Vec<u8>, LarmError>;
}

struct HttpsRegistryIndexFetcher {
    client: reqwest::Client,
}

impl HttpsRegistryIndexFetcher {
    fn new() -> Result<Self, LarmError> {
        let client = reqwest::Client::builder()
            .https_only(true)
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(std::time::Duration::from_secs(5))
            .timeout(std::time::Duration::from_secs(20))
            .build()
            .map_err(|error| {
                LarmError::new(
                    ErrorCode::Internal,
                    "No se pudo preparar la conexión segura al registro",
                    "registry_client_unavailable",
                )
                .with_technical(error.to_string())
            })?;
        Ok(Self { client })
    }
}

#[async_trait::async_trait]
impl RegistryIndexFetcher for HttpsRegistryIndexFetcher {
    async fn fetch(&self, url: &str) -> Result<Vec<u8>, LarmError> {
        let response = self
            .client
            .get(url)
            .header(reqwest::header::ACCEPT, "application/json")
            .send()
            .await
            .map_err(|error| {
                LarmError::new(
                    ErrorCode::UpstreamUnavailable,
                    "No se pudo consultar el registro HTTPS",
                    "registry_fetch_failed",
                )
                .retryable(true)
                .with_technical(error.to_string())
            })?;
        if response.status() != reqwest::StatusCode::OK {
            return Err(LarmError::new(
                ErrorCode::UpstreamUnavailable,
                "El registro no devolvió un índice disponible",
                "registry_http_status_invalid",
            )
            .retryable(response.status().is_server_error()));
        }
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .split(';')
            .next()
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        if content_type != "application/json"
            && !(content_type.starts_with("application/") && content_type.ends_with("+json"))
        {
            return Err(LarmError::new(
                ErrorCode::ProtocolError,
                "El registro no devolvió JSON",
                "registry_content_type_invalid",
            ));
        }
        if response
            .content_length()
            .is_some_and(|length| length > REGISTRY_INDEX_MAX_BYTES as u64)
        {
            return Err(LarmError::new(
                ErrorCode::ValidationFailed,
                "El índice del registro supera el tamaño máximo",
                "registry_index_too_large",
            ));
        }
        let mut response = response;
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|error| {
            LarmError::new(
                ErrorCode::UpstreamUnavailable,
                "No se pudo leer el índice del registro",
                "registry_body_read_failed",
            )
            .retryable(true)
            .with_technical(error.to_string())
        })? {
            if body.len().saturating_add(chunk.len()) > REGISTRY_INDEX_MAX_BYTES {
                return Err(LarmError::new(
                    ErrorCode::ValidationFailed,
                    "El índice del registro supera el tamaño máximo",
                    "registry_index_too_large",
                ));
            }
            body.extend_from_slice(&chunk);
        }
        Ok(body)
    }
}

/// Cliente P028 del registro firmado. El transporte no envía secretos ni
/// sigue redirects; el estado sólo cambia después de verificar la firma.
pub struct RegistrySyncService {
    db: larm_db::pool::Db,
    registry_url: Option<String>,
    pinned_keys: std::collections::BTreeMap<String, String>,
    allowed_origins: std::collections::BTreeSet<String>,
    fetcher: std::sync::Arc<dyn RegistryIndexFetcher>,
}

impl RegistrySyncService {
    pub fn new(
        db: larm_db::pool::Db,
        security: &larm_core::config::SecuritySettings,
    ) -> Result<std::sync::Arc<Self>, LarmError> {
        Self::with_fetcher(
            db,
            security.registry_url.clone(),
            security.publisher_keys.clone(),
            security.registry_allowed_origins.iter().cloned().collect(),
            std::sync::Arc::new(HttpsRegistryIndexFetcher::new()?),
        )
    }

    fn with_fetcher(
        db: larm_db::pool::Db,
        registry_url: Option<String>,
        pinned_keys: std::collections::BTreeMap<String, String>,
        allowed_origins: std::collections::BTreeSet<String>,
        fetcher: std::sync::Arc<dyn RegistryIndexFetcher>,
    ) -> Result<std::sync::Arc<Self>, LarmError> {
        if let Some(url) = &registry_url {
            let parsed = url::Url::parse(url).map_err(|_| {
                LarmError::new(
                    ErrorCode::ValidationFailed,
                    "La URL del registro no es válida",
                    "registry_url_invalid",
                )
            })?;
            if parsed.scheme() != "https"
                || parsed.host_str().is_none()
                || !parsed.username().is_empty()
                || parsed.password().is_some()
                || parsed.query().is_some()
                || parsed.fragment().is_some()
                || pinned_keys.is_empty()
            {
                return Err(LarmError::new(
                    ErrorCode::ValidationFailed,
                    "La configuración del registro no cumple la política HTTPS",
                    "registry_configuration_invalid",
                ));
            }
        }
        Ok(std::sync::Arc::new(Self {
            db,
            registry_url,
            pinned_keys,
            allowed_origins,
            fetcher,
        }))
    }
}

#[async_trait::async_trait]
impl larm_domain::adapter_registry::AdapterRegistryManager for RegistrySyncService {
    async fn status(&self) -> Result<larm_domain::adapter_registry::RegistryStatus, LarmError> {
        larm_db::adapter_registry::status(&self.db.pool, self.registry_url.as_deref()).await
    }

    async fn sync(
        &self,
        ctx: &larm_domain::ports::MutationContext,
    ) -> Result<larm_domain::adapter_registry::RegistrySyncReceipt, LarmError> {
        use chrono::SecondsFormat;

        let registry_url = self.registry_url.as_deref().ok_or_else(|| {
            LarmError::new(
                ErrorCode::UpstreamUnavailable,
                "El registro remoto está deshabilitado",
                "adapter_registry_disabled",
            )
        })?;
        let bytes = self.fetcher.fetch(registry_url).await?;
        let envelope: larm_adapter_host::registry::RegistryIndexEnvelope =
            serde_json::from_slice(&bytes).map_err(|error| {
                LarmError::new(
                    ErrorCode::ProtocolError,
                    "El índice del registro tiene un formato no válido",
                    "registry_envelope_invalid",
                )
                .with_technical(error.to_string())
            })?;
        let checkpoint =
            larm_db::adapter_registry::checkpoint_by_url(&self.db.pool, registry_url).await?;
        let now = chrono::Utc::now();
        let verified = larm_adapter_host::registry::verify_index(
            &envelope,
            &self.pinned_keys,
            registry_url,
            &self.allowed_origins,
            now,
            checkpoint
                .as_ref()
                .map(|value| (value.sequence, value.payload_sha256.as_str())),
        )?;
        let accepted_at = now.to_rfc3339_opts(SecondsFormat::Millis, true);
        let envelope_json = serde_json::to_string(&envelope).map_err(|error| {
            LarmError::new(
                ErrorCode::Internal,
                "No se pudo conservar el índice firmado",
                "registry_envelope_store_failed",
            )
            .with_technical(error.to_string())
        })?;
        let revocations =
            verified
                .payload
                .revocations
                .iter()
                .map(|revocation| {
                    larm_domain::adapter_registry::RegistryRevocationRecord {
                registry_id: verified.payload.registry_id.clone(),
                target_type: match revocation.target_type {
                    larm_adapter_host::registry::RegistryRevocationType::PackageSha256 => {
                        larm_domain::adapter_registry::RegistryRevocationTargetType::PackageSha256
                    }
                    larm_adapter_host::registry::RegistryRevocationType::PublisherKey => {
                        larm_domain::adapter_registry::RegistryRevocationTargetType::PublisherKey
                    }
                },
                target: revocation.target.clone(),
                revoked_at: revocation.revoked_at.clone(),
                reason_code: revocation.reason_code.clone(),
                source_sequence: verified.payload.sequence,
            }
                })
                .collect::<Vec<_>>();
        larm_db::adapter_registry::accept_index(
            &self.db.pool,
            registry_url,
            &verified.payload.registry_id,
            verified.payload.sequence,
            &verified.payload_sha256,
            &verified.payload.issued_at,
            &verified.payload.expires_at,
            &accepted_at,
            &envelope_json,
            u64::try_from(verified.payload.packages.len()).unwrap_or(u64::MAX),
            &revocations,
            ctx,
        )
        .await
    }
}

#[cfg(test)]
mod registry_sync_tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use chrono::SecondsFormat;
    use ed25519_dalek::{Signer, SigningKey};
    use larm_adapter_host::registry::{
        RegistryIndexEnvelope, RegistryIndexPayload, RegistryRevocation, RegistryRevocationType,
    };
    use larm_domain::adapter_registry::AdapterRegistryManager;

    struct FixtureFetcher {
        response: parking_lot::Mutex<Vec<u8>>,
    }

    #[async_trait::async_trait]
    impl RegistryIndexFetcher for FixtureFetcher {
        async fn fetch(&self, _url: &str) -> Result<Vec<u8>, LarmError> {
            Ok(self.response.lock().clone())
        }
    }

    fn signed_index(key: &SigningKey, sequence: u64, revocations: bool) -> Vec<u8> {
        let now = chrono::Utc::now();
        let payload = RegistryIndexPayload {
            schema_version: "1.0".into(),
            registry_id: "fixture-v1".into(),
            sequence,
            issued_at: (now - chrono::Duration::minutes(1))
                .to_rfc3339_opts(SecondsFormat::Millis, true),
            expires_at: (now + chrono::Duration::hours(1))
                .to_rfc3339_opts(SecondsFormat::Millis, true),
            packages: Vec::new(),
            revocations: if revocations {
                vec![RegistryRevocation {
                    target_type: RegistryRevocationType::PackageSha256,
                    target: "a".repeat(64),
                    revoked_at: (now - chrono::Duration::minutes(2))
                        .to_rfc3339_opts(SecondsFormat::Millis, true),
                    reason_code: "MALICIOUS_PACKAGE".into(),
                }]
            } else {
                Vec::new()
            },
        };
        let value = serde_json::to_value(&payload).unwrap();
        let canonical = larm_domain::canonical::canonicalize(&value).unwrap();
        let signature = key.sign(canonical.as_bytes());
        serde_json::to_vec(&RegistryIndexEnvelope {
            payload,
            signature: larm_adapter_sdk::manifest::Signature {
                algorithm: larm_adapter_sdk::manifest::SignatureAlgorithm::Ed25519,
                key_id: "registry-test".into(),
                value: hex::encode(signature.to_bytes()),
            },
        })
        .unwrap()
    }

    fn manager(
        db: larm_db::pool::Db,
        key: &SigningKey,
        fetcher: std::sync::Arc<FixtureFetcher>,
    ) -> std::sync::Arc<RegistrySyncService> {
        RegistrySyncService::with_fetcher(
            db,
            Some("https://registry.example.test/index.json".into()),
            std::collections::BTreeMap::from([(
                "registry-test".into(),
                hex::encode(key.verifying_key().to_bytes()),
            )]),
            std::collections::BTreeSet::new(),
            fetcher,
        )
        .unwrap()
    }

    #[tokio::test]
    async fn firma_checkpoint_replay_y_rollback_recuperan_estado_durable() {
        let (_dir, db) = larm_db::testing::temp_migrated().await.unwrap();
        let key = SigningKey::from_bytes(&[11; 32]);
        let fetcher = std::sync::Arc::new(FixtureFetcher {
            response: parking_lot::Mutex::new(signed_index(&key, 1, true)),
        });
        let service = manager(db.clone(), &key, fetcher.clone());
        let ctx = larm_domain::ports::MutationContext {
            actor_id: "operator-1".into(),
            correlation_id: "0192f000-0000-7000-8000-000000000001".into(),
        };

        let first = service.sync(&ctx).await.unwrap();
        assert_eq!(first.sequence, 1);
        assert!(!first.replay);
        let replay = service.sync(&ctx).await.unwrap();
        assert!(replay.replay);
        let stored = service.status().await.unwrap();
        assert_eq!(stored.checkpoint.unwrap().sequence, 1);
        assert_eq!(stored.revocations.len(), 1);

        *fetcher.response.lock() = signed_index(&key, 2, false);
        assert_eq!(service.sync(&ctx).await.unwrap().sequence, 2);
        *fetcher.response.lock() = signed_index(&key, 1, true);
        assert_eq!(
            service.sync(&ctx).await.unwrap_err().details.reason,
            "registry_sequence_rollback"
        );
        let stored = service.status().await.unwrap();
        assert_eq!(stored.checkpoint.unwrap().sequence, 2);
        assert_eq!(stored.revocations.len(), 1);
        let events: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM event WHERE event_type='adapter.registry.synced'",
        )
        .fetch_one(&db.pool)
        .await
        .unwrap();
        assert_eq!(events, 2);
    }

    #[tokio::test]
    async fn registro_deshabilitado_no_consulta_red() {
        let (_dir, db) = larm_db::testing::temp_migrated().await.unwrap();
        let fetcher = std::sync::Arc::new(FixtureFetcher {
            response: parking_lot::Mutex::new(Vec::new()),
        });
        let service = RegistrySyncService::with_fetcher(
            db,
            None,
            std::collections::BTreeMap::new(),
            std::collections::BTreeSet::new(),
            fetcher,
        )
        .unwrap();
        let ctx = larm_domain::ports::MutationContext {
            actor_id: "operator-1".into(),
            correlation_id: "0192f000-0000-7000-8000-000000000001".into(),
        };
        assert_eq!(
            service.sync(&ctx).await.unwrap_err().details.reason,
            "adapter_registry_disabled"
        );
        assert!(!service.status().await.unwrap().enabled);
    }
}

/// La identidad y el libre salen del mismo profiler P012; el mínimo de
/// política reutiliza P015. No se infiere el volumen por la letra de unidad.
pub struct NativePackageSpace {
    pub prober: std::sync::Arc<dyn larm_domain::ports::StorageProber>,
    pub locations: std::sync::Arc<dyn larm_domain::ports::StorageLocationRepository>,
    pub minimum_free_bytes: u64,
}

#[async_trait::async_trait]
impl larm_domain::ports::AdapterPackageSpace for NativePackageSpace {
    async fn measure_package_space(
        &self,
        root: &str,
        required_bytes: u64,
    ) -> Result<
        (
            larm_domain::ports::VolumeDemand,
            chrono::DateTime<chrono::Utc>,
        ),
        LarmError,
    > {
        let plain = if let Some(unc) = root.strip_prefix(r"\\?\UNC\") {
            format!(r"\\{unc}")
        } else {
            root.strip_prefix(r"\\?\").unwrap_or(root).to_string()
        };
        let kind = larm_storage::locations::check_syntax(&plain)?;
        let unc = match &kind {
            larm_storage::locations::RootKind::Unc(u) => vec![u.root()],
            _ => vec![],
        };
        let observation = self.prober.probe(&unc).await?;
        let locations = self.locations.all().await?;
        let volume =
            larm_storage::locations::volume_for(&observation, &plain, &kind).ok_or_else(|| {
                LarmError::new(
                    ErrorCode::ResourceUnavailable,
                    "No se pudo identificar el volumen de adaptadores",
                    "adapter_volume_unknown",
                )
            })?;
        if !volume.online || volume.read_only {
            return Err(LarmError::new(
                ErrorCode::ResourceUnavailable,
                "El volumen de adaptadores no está disponible para escritura",
                "adapter_volume_unavailable",
            ));
        }
        let (Some(key), Some(free)) = (&volume.identity.volume_key, volume.free_bytes) else {
            return Err(LarmError::new(
                ErrorCode::ResourceUnavailable,
                "No se pudo medir el espacio del volumen",
                "adapter_volume_unknown",
            ));
        };
        if key.is_empty() || key.starts_with(larm_domain::locations::UNKNOWN_VOLUME_PREFIX) {
            return Err(LarmError::new(
                ErrorCode::ResourceUnavailable,
                "No se verificó la identidad del volumen",
                "adapter_volume_unknown",
            ));
        }
        if matches!(kind, larm_storage::locations::RootKind::Local)
            && (volume.identity.volume_guid.as_deref() != Some(key.as_str())
                || !uuid::Uuid::parse_str(key).is_ok_and(|id| id.to_string() == *key))
        {
            return Err(LarmError::new(
                ErrorCode::ResourceUnavailable,
                "La identidad local no está verificada",
                "adapter_volume_unknown",
            ));
        }
        let observed = chrono::DateTime::parse_from_rfc3339(&observation.observed_at)
            .map_err(|e| {
                LarmError::new(
                    ErrorCode::IntegrityFailed,
                    "Fecha de sondeo no válida",
                    "storage_observation_invalid",
                )
                .with_technical(e.to_string())
            })?
            .with_timezone(&chrono::Utc);
        Ok((
            larm_domain::ports::VolumeDemand {
                volume_key: key.clone(),
                required_bytes,
                os_free: free.get(),
                policy_minimum: larm_storage::estimate::policy_minimum(
                    key,
                    &locations,
                    self.minimum_free_bytes,
                ),
            },
            observed,
        ))
    }
}

fn read_document(file: &std::path::Path) -> Result<Vec<u8>, LarmError> {
    use std::io::Read;
    let mut data = Vec::new();
    std::fs::File::open(file)
        .and_then(|f| f.take((1 << 20) + 1).read_to_end(&mut data))
        .map_err(|e| {
            LarmError::new(
                ErrorCode::ResourceUnavailable,
                "No se pudo leer el documento",
                "adapter_document_io_failed",
            )
            .with_technical(e.to_string())
        })?;
    if data.len() > 1 << 20 {
        return Err(LarmError::new(
            ErrorCode::ValidationFailed,
            "Documento demasiado grande",
            "request_body_too_large",
        ));
    }
    Ok(data)
}

pub fn coordinator(
    db: &larm_db::pool::Db,
    settings: &larm_core::config::Settings,
    data_root: &std::path::Path,
    trust_file: Option<&std::path::Path>,
) -> Result<std::sync::Arc<larm_adapter_host::updates::UpdateCoordinator>, LarmError> {
    use std::sync::Arc;
    let data_root = std::path::absolute(data_root).map_err(|e| {
        LarmError::new(
            ErrorCode::ResourceUnavailable,
            "Raíz de datos no disponible",
            "adapter_root_invalid",
        )
        .with_technical(e.to_string())
    })?;
    let prober = crate::hardware::storage_prober().ok_or_else(|| {
        LarmError::new(
            ErrorCode::ResourceUnavailable,
            "Sondeo de almacenamiento no disponible",
            "storage_probe_unavailable",
        )
    })?;
    let trust = trust_store(settings, &data_root, trust_file)?;
    larm_adapter_host::updates::UpdateCoordinator::open(
        data_root.join("adapters"),
        larm_adapter_host::updates::UpdatePorts {
            jobs: Arc::new(larm_db::adapter_updates::SqlAdapterUpdates {
                pool: db.pool.clone(),
            }),
            installations: Arc::new(larm_db::adapters::SqlAdapterInstallations {
                pool: db.pool.clone(),
            }),
            approvals: Arc::new(larm_db::security::SqlApprovals {
                pool: db.pool.clone(),
            }),
            reservations: Arc::new(larm_db::reservations::SqlVolumeReservations {
                pool: db.pool.clone(),
            }),
            guard: Arc::new(larm_db::guard::SqlResourceGuard {
                pool: db.pool.clone(),
            }),
            space: Arc::new(NativePackageSpace {
                prober: Arc::new(prober),
                locations: Arc::new(larm_db::locations::SqlStorageLocations {
                    pool: db.pool.clone(),
                }),
                minimum_free_bytes: settings.storage.minimum_free_bytes.get(),
            }),
        },
        trust,
    )
}

pub fn import_coordinator(
    db: &larm_db::pool::Db,
    settings: &larm_core::config::Settings,
    data_root: &std::path::Path,
    trust_file: Option<&std::path::Path>,
) -> Result<std::sync::Arc<larm_adapter_host::imports::ImportCoordinator>, LarmError> {
    use std::sync::Arc;
    let data_root = std::path::absolute(data_root).map_err(|e| {
        LarmError::new(
            ErrorCode::ResourceUnavailable,
            "Raíz de datos no disponible",
            "adapter_root_invalid",
        )
        .with_technical(e.to_string())
    })?;
    let prober = crate::hardware::storage_prober().ok_or_else(|| {
        LarmError::new(
            ErrorCode::ResourceUnavailable,
            "Sondeo de almacenamiento no disponible",
            "storage_probe_unavailable",
        )
    })?;
    let trust = trust_store(settings, &data_root, trust_file)?;
    larm_adapter_host::imports::ImportCoordinator::open(
        data_root.join("adapters"),
        larm_adapter_host::imports::ImportPorts {
            jobs: Arc::new(larm_db::adapter_imports::SqlAdapterImports {
                pool: db.pool.clone(),
            }),
            reservations: Arc::new(larm_db::reservations::SqlVolumeReservations {
                pool: db.pool.clone(),
            }),
            guard: Arc::new(larm_db::guard::SqlResourceGuard {
                pool: db.pool.clone(),
            }),
            space: Arc::new(NativePackageSpace {
                prober: Arc::new(prober),
                locations: Arc::new(larm_db::locations::SqlStorageLocations {
                    pool: db.pool.clone(),
                }),
                minimum_free_bytes: settings.storage.minimum_free_bytes.get(),
            }),
        },
        trust,
    )
}

fn trust_store(
    settings: &larm_core::config::Settings,
    data_root: &std::path::Path,
    explicit: Option<&std::path::Path>,
) -> Result<larm_adapter_sdk::permissions::TrustStore, LarmError> {
    let default = data_root.join("security/adapter-trust.json");
    let selected = explicit.or_else(|| default.exists().then_some(default.as_path()));
    let mut trust = selected
        .map(|file| {
            let file = if file.is_absolute() {
                file.to_path_buf()
            } else {
                std::env::current_dir()
                    .map_err(|e| {
                        LarmError::new(
                            ErrorCode::ResourceUnavailable,
                            "Directorio no disponible",
                            "adapter_trust_store_invalid",
                        )
                        .with_technical(e.to_string())
                    })?
                    .join(file)
            };
            larm_adapter_host::package::check_managed_path(&file)?;
            serde_json::from_slice::<larm_adapter_sdk::permissions::TrustStore>(&read_document(
                &file,
            )?)
            .map_err(|e| {
                LarmError::new(
                    ErrorCode::ValidationFailed,
                    "TrustStore no válido",
                    "adapter_trust_store_invalid",
                )
                .with_technical(e.to_string())
            })
        })
        .transpose()?
        .unwrap_or_default();
    let mut seen = std::collections::BTreeSet::new();
    if trust.keys.iter().any(|k| {
        k.key_id.is_empty()
            || k.publisher.trim().is_empty()
            || !seen.insert(k.key_id.clone())
            || !larm_domain::adapter_updates::is_sha256(&k.public_key)
    }) || trust
        .revoked_packages
        .iter()
        .any(|h| !larm_domain::adapter_updates::is_sha256(h))
    {
        return Err(LarmError::new(
            ErrorCode::ValidationFailed,
            "TrustStore ambiguo o mal formado",
            "adapter_trust_store_invalid",
        ));
    }
    if explicit.is_none() {
        // Settings fija las claves permitidas; el fichero aporta el binding
        // key_id/publicador, official/revoked y revocaciones de paquetes.
        // Vaciar publisher_keys no reactiva claves antiguas del fichero.
        let mut pinned = Vec::new();
        for (id, key) in &settings.security.publisher_keys {
            pinned.push(
                trust
                    .keys
                    .iter()
                    .find(|k| &k.key_id == id && &k.public_key == key)
                    .cloned()
                    .ok_or_else(|| {
                        LarmError::new(
                            ErrorCode::ValidationFailed,
                            "Falta el binding explícito de una clave configurada",
                            "publisher_key_binding_missing",
                        )
                    })?,
            );
        }
        trust.keys = pinned;
    }
    Ok(trust)
}

/// Herramienta local, identidad Windows del operador. La API conservará
/// como actor al principal autenticado al conectarse en el siguiente paso.
pub async fn update(
    db: &larm_db::pool::Db,
    settings: &larm_core::config::Settings,
    data_root: &std::path::Path,
    installation: &str,
    file: &std::path::Path,
    key: &str,
    trust_file: Option<&std::path::Path>,
) -> Result<serde_json::Value, LarmError> {
    use larm_domain::ports::AdapterUpdateRepository;
    use sha2::{Digest, Sha256};
    let data = read_document(file)?;
    let request: larm_domain::adapter_updates::AdapterImport = serde_json::from_slice(&data)
        .map_err(|e| {
            LarmError::new(
                ErrorCode::ValidationFailed,
                "Solicitud de actualización no válida",
                "adapter_import_invalid",
            )
            .with_technical(e.to_string())
        })?;
    request.validate()?;
    let c = coordinator(db, settings, data_root, trust_file)?;
    c.clone().recover().await?;
    let jobs = larm_db::adapter_updates::SqlAdapterUpdates {
        pool: db.pool.clone(),
    };
    let (receipt, new) = jobs
        .enqueue_update(
            installation,
            &request,
            &hex::encode(Sha256::digest(&data)),
            key,
            &larm_domain::ports::MutationContext {
                actor_id: "local-operator".into(),
                correlation_id: larm_domain::correlation::CorrelationId::new().to_string(),
            },
        )
        .await?;
    if new {
        c.execute(
            receipt.job_id.clone(),
            larm_adapter_host::package::PackageControl::default(),
        )
        .await
        .map_err(|e| e.with_resource(&receipt.job_id))?;
    }
    let state = jobs
        .update_job(&receipt.job_id)
        .await?
        .ok_or_else(|| LarmError::new(ErrorCode::NotFound, "Trabajo desconocido", "job_unknown"))?
        .public_state();
    if let Some(e) = state.error {
        return Err(e.with_resource(&receipt.job_id));
    }
    Ok(serde_json::json!({"receipt":receipt,"job":state}))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use larm_domain::ports::AdapterPackageSpace;
    use larm_domain::storage::{
        DriveKind, PerformanceClass, StorageObservation, StorageVolume, VolumeIdentity,
    };
    use larm_domain::units::DecimalU64;
    struct Probe(StorageObservation);
    #[async_trait::async_trait]
    impl larm_domain::ports::StorageProber for Probe {
        async fn probe(&self, _: &[String]) -> Result<StorageObservation, LarmError> {
            Ok(self.0.clone())
        }
    }
    fn observation() -> StorageObservation {
        StorageObservation {
            volumes: vec![StorageVolume {
                identity: VolumeIdentity {
                    volume_key: Some("0192aaaa-0000-7000-8000-000000000077".into()),
                    volume_guid: Some("0192aaaa-0000-7000-8000-000000000077".into()),
                    unc_path: None,
                    mount_points: vec!["C:\\".into()],
                    drive_letter: Some("C:".into()),
                },
                label: None,
                filesystem: Some("NTFS".into()),
                drive_kind: DriveKind::Fixed,
                bus: None,
                seek_penalty: None,
                performance_class: PerformanceClass::Unknown,
                capacity_bytes: DecimalU64::new(100 << 30),
                free_bytes: DecimalU64::new(20 << 30),
                read_only: false,
                online: true,
                evidence_refs: vec![],
            }],
            unknown: vec![],
            observed_at: larm_domain::bridge::timestamp(chrono::Utc::now()),
        }
    }
    #[tokio::test]
    async fn export_service_verifica_el_paquete_y_publica_evento_outbox() {
        use larm_adapter_sdk::manifest::{Family, MethodTable};
        use larm_adapter_sdk::permissions::package_hash;
        use larm_domain::adapters::{AdapterInstallation, AdapterState, AdapterTrust};
        use larm_domain::ports::{AdapterInstallationRepository, MutationContext};
        use serde_json::json;
        use sha2::{Digest, Sha256};

        let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap();
        let base = repo.join("target/p028-export-daemon-tests");
        std::fs::create_dir_all(&base).unwrap();
        let temp = tempfile::tempdir_in(base).unwrap();
        let root = temp.path().join("adapters");
        let package = root.join("test.runtime").join("1.0.0");
        std::fs::create_dir_all(package.join("bin")).unwrap();
        std::fs::write(package.join("bin/worker.exe"), b"MZ fixture worker").unwrap();
        std::fs::write(
            root.join("operator-private.txt"),
            b"must stay outside the export",
        )
        .unwrap();
        let table = MethodTable::contract();
        let mut methods = table.mandatory_common.clone();
        methods.extend(
            table
                .required_for(Family::Runtime)
                .iter()
                .map(ToString::to_string),
        );
        let package_hash = package_hash(&package, &["manifest.json"]).unwrap();
        let manifest = json!({
            "id":"test.runtime","version":"1.0.0","family":"runtime","protocol_version":"1.0",
            "publisher":"Fixture","executable":{"relative_path":"bin/worker.exe","args":[]},
            "platforms":["windows"],"architectures":["x86_64"],"methods":methods,
            "permissions":{"read_roots":[],"write_roots":[],"network_origins":[],"process_allowlist":[],"secret_refs":[]},
            "capabilities":["runtime.gguf"],"package_sha256":package_hash,"signature":null,"installation_strategies":[]
        });
        std::fs::write(package.join("manifest.json"), manifest.to_string()).unwrap();

        let (_db_root, db) = larm_db::testing::temp_migrated().await.unwrap();
        let installations = Arc::new(larm_db::adapters::SqlAdapterInstallations {
            pool: db.pool.clone(),
        });
        let now = larm_domain::bridge::timestamp(chrono::Utc::now());
        let installation = AdapterInstallation {
            id: "0192f000-0000-7000-8000-000000000071".into(),
            created_at: now.clone(),
            updated_at: now,
            row_version: 4,
            metadata: serde_json::Map::new(),
            adapter_id: "test.runtime".into(),
            version: "1.0.0".into(),
            family: "runtime".into(),
            package_hash: package_hash.clone(),
            publisher: "Fixture".into(),
            trust: AdapterTrust::Untrusted,
            manifest: manifest.clone(),
            state: AdapterState::Validated,
        };
        let ctx = MutationContext {
            actor_id: "operator-fixture".into(),
            correlation_id: "0192f000-0000-7000-8000-000000000072".into(),
        };
        installations.insert(&installation, &ctx).await.unwrap();
        let service = PackageExportService::new(db.clone(), installations, root.clone()).unwrap();
        let destination = temp.path().join("adapter-export.zip");
        let request = larm_domain::adapter_packages::AdapterExport {
            adapter_installation_id: installation.id.clone(),
            destination_path: destination.display().to_string(),
        };
        let receipt = service.export_package(&request, &ctx).await.unwrap();
        assert_eq!(receipt.resource_id, installation.id);
        assert_eq!(receipt.row_version, installation.row_version);
        assert_eq!(receipt.event_id, "2");

        let archive_hash = hex::encode(Sha256::digest(std::fs::read(&destination).unwrap()));
        let plan = larm_adapter_host::package::ArchivePlan::inspect(
            &destination,
            &archive_hash,
            &larm_adapter_host::package::PackageControl::default(),
        )
        .unwrap();
        assert_eq!(plan.manifest.package_sha256, package_hash);
        let mut archive = zip::ZipArchive::new(std::fs::File::open(&destination).unwrap()).unwrap();
        assert_eq!(archive.len(), 2);
        let names: Vec<String> = (0..archive.len())
            .map(|index| {
                archive
                    .by_index(index)
                    .unwrap()
                    .name()
                    .unwrap()
                    .into_owned()
            })
            .collect();
        assert_eq!(names, ["bin/worker.exe", "manifest.json"]);

        let event: (String, String, String) =
            sqlx::query_as("SELECT event_type,actor_id,payload FROM event WHERE resource_id=? AND event_type='adapter.exported'")
                .bind(&installation.id)
                .fetch_one(&db.pool)
                .await
                .unwrap();
        assert_eq!(event.0, "adapter.exported");
        assert_eq!(event.1, ctx.actor_id);
        assert!(!event.2.contains(&destination.display().to_string()));
        let outbox: i64 = sqlx::query_scalar("SELECT count(*) FROM event_outbox WHERE event_id=?")
            .bind(receipt.event_id.parse::<i64>().unwrap())
            .fetch_one(&db.pool)
            .await
            .unwrap();
        assert_eq!(outbox, 1);
    }

    #[tokio::test]
    async fn espacio_usa_identidad_observada_y_normaliza_prefijo_windows() {
        let (_dir, db) = larm_db::testing::temp_migrated().await.unwrap();
        let space = NativePackageSpace {
            prober: std::sync::Arc::new(Probe(observation())),
            locations: std::sync::Arc::new(larm_db::locations::SqlStorageLocations {
                pool: db.pool.clone(),
            }),
            minimum_free_bytes: 2 << 30,
        };
        let (demand, _) = space
            .measure_package_space(r"\\?\C:\LARM\adapters", 1 << 30)
            .await
            .unwrap();
        assert_eq!(demand.volume_key, "0192aaaa-0000-7000-8000-000000000077");
        assert_eq!(demand.os_free, 20 << 30);
        assert_eq!(demand.policy_minimum, 2 << 30);
        assert_eq!(demand.required_bytes, 1 << 30);
    }
    #[tokio::test]
    async fn espacio_desconocido_offline_o_readonly_no_se_convierte_en_reserva() {
        let (_dir, db) = larm_db::testing::temp_migrated().await.unwrap();
        for case in 0..8 {
            let mut obs = observation();
            match case {
                0 => obs.volumes[0].online = false,
                1 => obs.volumes[0].read_only = true,
                2 => obs.volumes[0].free_bytes = None,
                3 => obs.volumes[0].identity.volume_guid = None,
                4 => obs.volumes[0].identity.volume_key = Some("unknown:fixture".into()),
                5 => obs.volumes[0].identity.volume_key = Some(String::new()),
                6 => obs.observed_at = "not-a-timestamp".into(),
                _ => {
                    obs.volumes[0].identity.volume_key =
                        Some("0192aaaa-0000-7000-8000-000000000088".into())
                }
            }
            let space = NativePackageSpace {
                prober: std::sync::Arc::new(Probe(obs)),
                locations: std::sync::Arc::new(larm_db::locations::SqlStorageLocations {
                    pool: db.pool.clone(),
                }),
                minimum_free_bytes: 0,
            };
            let error = space
                .measure_package_space(r"C:\LARM\adapters", 1 << 30)
                .await
                .unwrap_err();
            assert_eq!(
                error.code,
                if case == 6 {
                    ErrorCode::IntegrityFailed
                } else {
                    ErrorCode::ResourceUnavailable
                }
            );
        }
        let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM volume_reservation")
            .fetch_one(&db.pool)
            .await
            .unwrap();
        assert_eq!(rows, 0);
    }

    #[test]
    fn confianza_http_respeta_pins_configurados_y_no_reactiva_claves_antiguas() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("security")).unwrap();
        let file = dir.path().join("security/adapter-trust.json");
        let key = serde_json::json!({"key_id":"publisher-fixture","publisher":"Fixture publisher","public_key":"a".repeat(64),"official":false,"revoked":false});
        std::fs::write(
            &file,
            serde_json::json!({"keys":[key],"revoked_packages":["b".repeat(64)]}).to_string(),
        )
        .unwrap();
        let mut settings = larm_core::config::Settings::compose(None, None, None).unwrap();
        let empty = trust_store(&settings, dir.path(), None).unwrap();
        assert!(empty.keys.is_empty());
        assert_eq!(empty.revoked_packages.len(), 1);
        settings
            .security
            .publisher_keys
            .insert("publisher-fixture".into(), "a".repeat(64));
        assert_eq!(
            trust_store(&settings, dir.path(), None).unwrap().keys.len(),
            1
        );
        settings.security.publisher_keys.clear();
        assert!(trust_store(&settings, dir.path(), None)
            .unwrap()
            .keys
            .is_empty());
        assert_eq!(
            trust_store(&settings, dir.path(), Some(&file))
                .unwrap()
                .keys
                .len(),
            1,
            "pin explícito del operador CLI"
        );
        settings
            .security
            .publisher_keys
            .insert("publisher-fixture".into(), "c".repeat(64));
        assert_eq!(
            trust_store(&settings, dir.path(), None)
                .unwrap_err()
                .details
                .reason,
            "publisher_key_binding_missing"
        );
    }
    #[test]
    fn trust_store_ambiguo_o_con_valores_fuera_del_schema_falla_cerrado() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("trust.json");
        let settings = larm_core::config::Settings::compose(None, None, None).unwrap();
        let key = serde_json::json!({"key_id":"same","publisher":"Fixture","public_key":"a".repeat(64),"official":false,"revoked":false});
        for value in [
            serde_json::json!({"keys":[key.clone(),key],"revoked_packages":[]}),
            serde_json::json!({"keys":[],"revoked_packages":["BAD"]}),
            serde_json::json!({"keys":[],"revoked_packages":[],"trust":"VERIFIED"}),
        ] {
            std::fs::write(&file, value.to_string()).unwrap();
            assert_eq!(
                trust_store(&settings, dir.path(), Some(&file))
                    .unwrap_err()
                    .details
                    .reason,
                "adapter_trust_store_invalid"
            );
        }
    }
}
