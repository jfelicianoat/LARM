//! Composición P045 del probe durable con el worker source cualificado.

use async_trait::async_trait;
use larm_adapter_host::{conformance::NativeConformanceRunner, source::SourceWorker};
use larm_adapter_sdk::{
    permissions::Access,
    source::{AuthProbe, SearchSupport, SourceReply, SourceRequest},
};
use larm_domain::{
    credential::{CredentialReference, SecretType, BACKEND},
    error::{ErrorCode, LarmError},
    ports::{
        CredentialRepository, MutationContext, SourceAccountRepository, SourceAuthProbeExecutor,
    },
    source_probes::{SourceAuthProbeResult, SourceProbeFailure, SourceProbeJob},
    sources::{AuthMode, ModelSource},
};
use std::{collections::HashMap, path::PathBuf, sync::Arc, time::Duration};

const SOURCE_RPC_TIMEOUT: Duration = Duration::from_secs(60);

fn unavailable(reason: &str) -> LarmError {
    LarmError::new(
        ErrorCode::ResourceUnavailable,
        "No se pudo preparar el sondeo de autenticación de la fuente",
        reason,
    )
}

fn origin(url: &url::Url) -> Option<String> {
    let scheme = url.scheme();
    if !matches!(scheme, "http" | "https") {
        return None;
    }
    Some(format!(
        "{scheme}://{}:{}",
        url.host_str()?.to_ascii_lowercase(),
        url.port_or_known_default()?
    ))
}

fn access_for_probe(
    source: &ModelSource,
    source_type: &str,
    installation: &larm_domain::adapters::AdapterInstallation,
    credential: Option<&CredentialReference>,
) -> Result<Access, LarmError> {
    if installation
        .metadata
        .get("execution")
        .and_then(serde_json::Value::as_str)
        == Some("none")
    {
        return Err(unavailable("source_probe_execution_unavailable"));
    }
    let manifest =
        larm_adapter_sdk::manifest::AdapterManifest::parse(&installation.manifest.to_string())?;
    if !manifest
        .methods
        .iter()
        .any(|method| method == "source.auth_probe")
    {
        return Err(LarmError::new(
            ErrorCode::UnsupportedCapability,
            "El adaptador no declara source.auth_probe",
            "source_probe_method_unavailable",
        ));
    }
    let secret_service = format!("larm/source/{}", source.id);
    let secrets = match &source.auth_mode {
        AuthMode::None => {
            if credential.is_some() {
                return Err(unavailable("source_probe_credential_unexpected"));
            }
            Vec::new()
        }
        AuthMode::Basic => {
            let account_id = source.account_id.as_deref().ok_or_else(|| {
                LarmError::new(
                    ErrorCode::AuthRequired,
                    "La fuente Basic no tiene una cuenta seleccionada",
                    "source_account_required",
                )
            })?;
            let reference = credential.ok_or_else(|| {
                LarmError::new(
                    ErrorCode::AuthRequired,
                    "La cuenta Basic no tiene una credencial vinculada",
                    "source_credential_reference_missing",
                )
            })?;
            if reference.service != secret_service
                || reference.username != account_id
                || reference.backend != BACKEND
                || reference.secret_type != SecretType::Password
                || !reference.scopes.is_empty()
                || reference.expires_at.is_some()
            {
                return Err(LarmError::new(
                    ErrorCode::Forbidden,
                    "La credencial no corresponde a la cuenta Basic seleccionada",
                    "source_credential_scope_mismatch",
                )
                .with_resource(&reference.id));
            }
            if !manifest.permissions.secret_refs.contains(&secret_service) {
                return Err(LarmError::new(
                    ErrorCode::Forbidden,
                    "El adaptador no declara la referencia de credencial de esta fuente",
                    "source_probe_secret_not_declared",
                )
                .with_resource(&reference.id));
            }
            vec![secret_service.clone()]
        }
        AuthMode::WindowsIntegrated
            if source_type == "UNC_PRIVATE"
                && source.account_id.is_none()
                && credential.is_none() =>
        {
            Vec::new()
        }
        _ => {
            return Err(LarmError::new(
                ErrorCode::UnsupportedCapability,
                "El adaptador aún no implementa este mecanismo de autenticación",
                "unsupported_auth",
            ));
        }
    };
    match source_type {
        "LOCAL" => {
            if !secrets.is_empty() {
                return Err(unavailable("source_probe_local_auth_unsupported"));
            }
            let path = if source.base_url.starts_with("file:") {
                url::Url::parse(&source.base_url)
                    .ok()
                    .and_then(|url| url.to_file_path().ok())
                    .ok_or_else(|| unavailable("source_probe_path_invalid"))?
            } else {
                PathBuf::from(&source.base_url)
            };
            if !path.is_absolute() || !path.is_dir() {
                return Err(unavailable("source_probe_path_unavailable"));
            }
            Ok(Access {
                reads: vec![path],
                ..Access::default()
            })
        }
        "UNC_PRIVATE" => {
            let path = PathBuf::from(&source.base_url);
            if source.auth_mode != AuthMode::WindowsIntegrated
                || credential.is_some()
                || !path.is_absolute()
                || !path.is_dir()
            {
                return Err(unavailable("source_probe_path_unavailable"));
            }
            Ok(Access {
                reads: vec![path],
                ..Access::default()
            })
        }
        "GIT_LFS" => Err(LarmError::new(
            ErrorCode::UnsupportedCapability,
            "Git LFS requiere autenticación específica",
            "unsupported_auth",
        )),
        _ => {
            let url = url::Url::parse(&source.base_url)
                .map_err(|_| unavailable("source_probe_url_invalid"))?;
            if !matches!(url.scheme(), "http" | "https")
                || url.host_str().is_none()
                || !url.username().is_empty()
                || url.password().is_some()
                || url.fragment().is_some()
            {
                return Err(unavailable("source_probe_url_invalid"));
            }
            if source.auth_mode == AuthMode::Basic && url.scheme() != "https" {
                return Err(LarmError::new(
                    ErrorCode::UnsupportedCapability,
                    "La autenticación Basic requiere HTTPS",
                    "basic_requires_https",
                ));
            }
            let source_origin =
                origin(&url).ok_or_else(|| unavailable("source_probe_url_invalid"))?;
            let declared = manifest
                .permissions
                .network_origins
                .iter()
                .filter_map(|candidate| url::Url::parse(candidate).ok())
                .collect::<Vec<_>>();
            if !declared
                .iter()
                .any(|candidate| origin(candidate).as_deref() == Some(source_origin.as_str()))
            {
                return Err(LarmError::new(
                    ErrorCode::Forbidden,
                    "El origen configurado no está declarado por el adaptador",
                    "source_probe_origin_not_declared",
                ));
            }
            let mut urls = declared
                .into_iter()
                .filter_map(|candidate| origin(&candidate).map(|_| candidate.to_string()))
                .collect::<Vec<_>>();
            urls.sort();
            urls.dedup();
            Ok(Access {
                urls,
                secrets,
                ..Access::default()
            })
        }
    }
}

pub struct QualifiedSourceAuthProbeExecutor {
    pub sources: larm_db::sources::SqlSources,
    pub accounts: larm_db::source_accounts::SqlSourceAccounts,
    pub credentials: larm_db::security::SqlCredentials,
    pub conformance: larm_db::conformance::SqlAdapterConformance,
    pub runner: Arc<NativeConformanceRunner>,
    active: tokio::sync::Mutex<HashMap<String, SourceWorker>>,
}

impl QualifiedSourceAuthProbeExecutor {
    pub fn new(
        sources: larm_db::sources::SqlSources,
        accounts: larm_db::source_accounts::SqlSourceAccounts,
        credentials: larm_db::security::SqlCredentials,
        conformance: larm_db::conformance::SqlAdapterConformance,
        runner: Arc<NativeConformanceRunner>,
    ) -> Self {
        Self {
            sources,
            accounts,
            credentials,
            conformance,
            runner,
            active: tokio::sync::Mutex::new(HashMap::new()),
        }
    }
}

#[async_trait]
impl SourceAuthProbeExecutor for QualifiedSourceAuthProbeExecutor {
    async fn execute_source_probe(
        &self,
        job: &SourceProbeJob,
    ) -> Result<SourceAuthProbeResult, SourceProbeFailure> {
        let binding = self
            .sources
            .execution_binding(&job.target)
            .await
            .map_err(SourceProbeFailure::before)?;
        if binding.source.row_version != job.source_row_version {
            return Err(SourceProbeFailure::before(LarmError::new(
                ErrorCode::StaleRevision,
                "La fuente cambió desde que se encoló el sondeo",
                "source_probe_snapshot_stale",
            )));
        }
        let current = self
            .conformance
            .current_for_consumer(&binding.installation.id)
            .await
            .map_err(SourceProbeFailure::before)?
            .ok_or_else(|| {
                SourceProbeFailure::before(unavailable("source_probe_qualification_missing"))
            })?;
        if current.0 != binding.installation {
            return Err(SourceProbeFailure::before(LarmError::new(
                ErrorCode::StaleRevision,
                "La instalación cambió desde su cualificación",
                "source_probe_qualification_stale",
            )));
        }
        let support: SearchSupport = binding
            .adapter
            .metadata
            .get("source_protocol_v1")
            .cloned()
            .and_then(|value| serde_json::from_value(value).ok())
            .ok_or_else(|| {
                SourceProbeFailure::before(unavailable("source_probe_protocol_binding_missing"))
            })?;
        let (account_id, credential_reference_id, credential) =
            if binding.source.auth_mode == AuthMode::Basic {
                let account_id = binding.source.account_id.as_deref().ok_or_else(|| {
                    SourceProbeFailure::before(LarmError::new(
                        ErrorCode::AuthRequired,
                        "La fuente Basic no tiene una cuenta seleccionada",
                        "source_account_required",
                    ))
                })?;
                let account = self
                    .accounts
                    .source_account(account_id)
                    .await
                    .map_err(SourceProbeFailure::before)?
                    .ok_or_else(|| {
                        SourceProbeFailure::before(LarmError::new(
                            ErrorCode::NotFound,
                            "La cuenta seleccionada ya no existe",
                            "source_account_missing",
                        ))
                    })?;
                if account.source_id != binding.source.id {
                    return Err(SourceProbeFailure::before(
                        LarmError::new(
                            ErrorCode::Forbidden,
                            "La cuenta seleccionada pertenece a otra fuente",
                            "source_account_mismatch",
                        )
                        .with_resource(account_id),
                    ));
                }
                let reference_id = account.credential_reference_id.as_deref().ok_or_else(|| {
                    SourceProbeFailure::before(LarmError::new(
                        ErrorCode::AuthRequired,
                        "La cuenta seleccionada no tiene una credencial vinculada",
                        "source_credential_reference_missing",
                    ))
                })?;
                let credential = self
                    .credentials
                    .get(reference_id)
                    .await
                    .map_err(SourceProbeFailure::before)?
                    .ok_or_else(|| {
                        SourceProbeFailure::before(LarmError::new(
                            ErrorCode::AuthBackendUnavailable,
                            "La referencia de credencial ya no existe",
                            "source_credential_reference_missing",
                        ))
                    })?;
                (
                    Some(account_id.to_string()),
                    Some(reference_id.to_string()),
                    Some(credential),
                )
            } else {
                (None, None, None)
            };
        let access = access_for_probe(
            &binding.source,
            &binding.adapter.source_type,
            &binding.installation,
            credential.as_ref(),
        )
        .map_err(SourceProbeFailure::before)?;
        let context = MutationContext {
            actor_id: job.actor_id.clone(),
            correlation_id: job.correlation_id.clone(),
        };
        let worker = self
            .runner
            .source_worker(&current.0, &current.1, &job.target, support, &context)
            .await
            .map_err(SourceProbeFailure::before)?;
        {
            let mut active = self.active.lock().await;
            if active.contains_key(&job.target) {
                return Err(SourceProbeFailure::before(LarmError::new(
                    ErrorCode::Conflict,
                    "Ya existe un worker de sondeo para esta fuente",
                    "source_probe_worker_active",
                )));
            }
            active.insert(job.target.clone(), worker.clone());
        }
        let response = worker
            .call(
                SourceRequest::AuthProbe(AuthProbe {
                    source_id: job.target.clone(),
                    auth_mode: binding.source.auth_mode,
                    account_id,
                    credential_reference_id,
                }),
                &access,
                Some(SOURCE_RPC_TIMEOUT),
            )
            .await;
        if let Err(error) = worker.close().await {
            return Err(SourceProbeFailure::unconfirmed(error));
        }
        self.active.lock().await.remove(&job.target);
        let SourceReply::AuthProbe(result) = response.map_err(SourceProbeFailure::before)? else {
            return Err(SourceProbeFailure::before(LarmError::new(
                ErrorCode::ProtocolError,
                "El adaptador respondió con otro método al sondeo de autenticación",
                "source_probe_reply_mismatch",
            )));
        };
        let reason = if result.authenticated {
            "authenticated"
        } else if result.license_required {
            "license_required"
        } else if result.auth_required {
            "authentication_required"
        } else {
            "probe_completed"
        };
        Ok(SourceAuthProbeResult {
            authenticated: result.authenticated,
            auth_required: result.auth_required,
            license_required: result.license_required,
            reason: reason.into(),
        })
    }

    async fn wait_source_probe_io(&self, source_id: &str) -> Result<(), LarmError> {
        let worker = self.active.lock().await.get(source_id).cloned();
        if let Some(worker) = worker {
            worker.close().await?;
            self.active.lock().await.remove(source_id);
        }
        Ok(())
    }
}

pub fn service(
    db: &larm_db::pool::Db,
    runner: Arc<NativeConformanceRunner>,
) -> Arc<larm_adapter_host::source_probe_service::SourceProbeService> {
    let sources = larm_db::sources::SqlSources {
        pool: db.pool.clone(),
        qualification: Some(runner.clone()),
    };
    let executor = Arc::new(QualifiedSourceAuthProbeExecutor::new(
        sources,
        larm_db::source_accounts::SqlSourceAccounts {
            pool: db.pool.clone(),
        },
        larm_db::security::SqlCredentials {
            pool: db.pool.clone(),
        },
        larm_db::conformance::SqlAdapterConformance {
            pool: db.pool.clone(),
        },
        runner,
    ));
    larm_adapter_host::source_probe_service::SourceProbeService::new(
        Arc::new(larm_db::source_probes::SqlSourceProbes {
            pool: db.pool.clone(),
        }),
        executor,
    )
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use larm_adapter_sdk::manifest::{AdapterManifest, Family, MethodTable};
    use larm_domain::{
        adapters::{AdapterInstallation, AdapterState, AdapterTrust},
        credential::{CredentialReference, SecretType, BACKEND},
        sources::AuthMode,
    };
    use serde_json::{json, Map, Value};

    fn source(auth_mode: AuthMode, base_url: &str) -> ModelSource {
        ModelSource {
            id: "0192f000-0000-7000-8000-00000000f201".into(),
            created_at: "2026-10-24T00:00:00Z".into(),
            updated_at: "2026-10-24T00:00:00Z".into(),
            row_version: 1,
            metadata: Map::new(),
            name: "fixture".into(),
            adapter_id: "0192f000-0000-7000-8000-00000000f202".into(),
            base_url: base_url.into(),
            priority: 0,
            enabled: true,
            auth_mode,
            account_id: None,
            policy: Map::new(),
            operational: Default::default(),
        }
    }

    fn basic_credential(source_id: &str, account_id: &str) -> CredentialReference {
        CredentialReference {
            id: "0192f000-0000-7000-8000-00000000f204".into(),
            created_at: "2026-10-24T00:00:00Z".into(),
            updated_at: "2026-10-24T00:00:00Z".into(),
            row_version: 1,
            metadata: Map::new(),
            service: format!("larm/source/{source_id}"),
            username: account_id.into(),
            backend: BACKEND.into(),
            secret_type: SecretType::Password,
            scopes: Vec::new(),
            expires_at: None,
        }
    }

    fn fixture_installation(network_origins: &[&str]) -> AdapterInstallation {
        let table = MethodTable::contract();
        let methods = table
            .mandatory_common
            .iter()
            .cloned()
            .chain(
                table
                    .required_for(Family::Source)
                    .into_iter()
                    .map(str::to_string),
            )
            .collect::<Vec<_>>();
        let manifest = AdapterManifest::parse(
            &json!({
                "id":"fixture.source",
                "version":"1.0.0",
                "family":"source",
                "protocol_version":"1.0",
                "publisher":"LARM test",
                "executable":{"relative_path":"bin/source.exe","args":[]},
                "platforms":["windows"],
                "architectures":["x86_64"],
                "methods":methods,
                "permissions":{
                    "read_roots":[],"write_roots":[],
                    "network_origins":network_origins,
                    "process_allowlist":[],"secret_refs":[]
                },
                "capabilities":["source.search"],
                "package_sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "signature":null,
                "installation_strategies":[]
            })
            .to_string(),
        )
        .unwrap();
        AdapterInstallation {
            id: "0192f000-0000-7000-8000-00000000f203".into(),
            created_at: "2026-10-24T00:00:00Z".into(),
            updated_at: "2026-10-24T00:00:00Z".into(),
            row_version: 1,
            metadata: Map::new(),
            adapter_id: "0192f000-0000-7000-8000-00000000f202".into(),
            version: "1.0.0".into(),
            family: "source".into(),
            package_hash: "a".repeat(64),
            publisher: "LARM test".into(),
            trust: AdapterTrust::Verified,
            manifest: serde_json::to_value(manifest).unwrap(),
            state: AdapterState::Installed,
        }
    }

    #[test]
    fn public_probe_is_limited_to_the_declared_http_origin() {
        let source = source(AuthMode::None, "https://catalog.example/api");
        let installed = fixture_installation(&["https://catalog.example"]);
        let access = access_for_probe(&source, "HTTP", &installed, None).unwrap();
        assert_eq!(access.urls, vec!["https://catalog.example/"]);

        let error = access_for_probe(
            &source,
            "HTTP",
            &fixture_installation(&["https://other.example"]),
            None,
        )
        .unwrap_err();
        assert_eq!(error.details.reason, "source_probe_origin_not_declared");
    }

    #[test]
    fn private_auth_and_missing_probe_method_fail_closed() {
        let private = source(AuthMode::Bearer, "https://catalog.example/api");
        let error = access_for_probe(
            &private,
            "HTTP",
            &fixture_installation(&["https://catalog.example"]),
            None,
        )
        .unwrap_err();
        assert_eq!(error.details.reason, "unsupported_auth");

        let public = source(AuthMode::None, "https://catalog.example/api");
        let mut invalid_installation = fixture_installation(&["https://catalog.example"]);
        invalid_installation.manifest["methods"]
            .as_array_mut()
            .unwrap()
            .retain(|method| method != "source.auth_probe");
        let error = access_for_probe(&public, "HTTP", &invalid_installation, None).unwrap_err();
        assert_eq!(error.details.reason, "method_required_missing");
    }

    #[test]
    fn basic_probe_requires_bound_password_https_and_exact_manifest_permission() {
        let account_id = "0192f000-0000-7000-8000-00000000f205";
        let mut private = source(AuthMode::Basic, "https://catalog.example/private");
        private.account_id = Some(account_id.into());
        let mut installed = fixture_installation(&["https://catalog.example"]);
        installed.manifest["permissions"]["secret_refs"] =
            json!([format!("larm/source/{}", private.id)]);
        let credential = basic_credential(&private.id, account_id);
        let access = access_for_probe(&private, "HTTP", &installed, Some(&credential)).unwrap();
        assert_eq!(access.urls, vec!["https://catalog.example/"]);
        assert_eq!(access.secrets, vec![format!("larm/source/{}", private.id)]);

        let mut other_account = credential.clone();
        other_account.username = "0192f000-0000-7000-8000-00000000f206".into();
        assert_eq!(
            access_for_probe(&private, "HTTP", &installed, Some(&other_account))
                .unwrap_err()
                .details
                .reason,
            "source_credential_scope_mismatch"
        );

        let mut wrong_type = credential.clone();
        wrong_type.secret_type = SecretType::Token;
        assert_eq!(
            access_for_probe(&private, "HTTP", &installed, Some(&wrong_type))
                .unwrap_err()
                .details
                .reason,
            "source_credential_scope_mismatch"
        );

        let mut no_permission = installed.clone();
        no_permission.manifest["permissions"]["secret_refs"] = json!([]);
        assert_eq!(
            access_for_probe(&private, "HTTP", &no_permission, Some(&credential))
                .unwrap_err()
                .details
                .reason,
            "source_probe_secret_not_declared"
        );

        private.base_url = "http://catalog.example/private".into();
        assert_eq!(
            access_for_probe(&private, "HTTP", &installed, Some(&credential))
                .unwrap_err()
                .details
                .reason,
            "basic_requires_https"
        );
    }

    #[test]
    fn disabled_execution_and_invalid_urls_are_rejected() {
        let public_source = source(AuthMode::None, "https://catalog.example/api");
        let mut installed = fixture_installation(&["https://catalog.example"]);
        installed
            .metadata
            .insert("execution".into(), Value::String("none".into()));
        assert_eq!(
            access_for_probe(&public_source, "HTTP", &installed, None)
                .unwrap_err()
                .details
                .reason,
            "source_probe_execution_unavailable"
        );

        let malformed = source(AuthMode::None, "https://user:pass@catalog.example/api");
        assert_eq!(
            access_for_probe(
                &malformed,
                "HTTP",
                &fixture_installation(&["https://catalog.example"]),
                None,
            )
            .unwrap_err()
            .details
            .reason,
            "source_probe_url_invalid"
        );
    }
}
