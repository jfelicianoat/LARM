//! Resolución y persistencia atómica de referencias P033/P036 para P100.

use async_trait::async_trait;
use larm_domain::{
    bridge::{timestamp, ApiError},
    instances::{InstanceState, RuntimeInstance},
    launch_profile::LaunchProfileRepository,
    ports::{MutationContext, VariantRepository},
    protocol::{EndpointQualification, InferenceEndpoint, ProtocolProfile, ProtocolRegistry},
    runtime::{RuntimeOwnership, RuntimeVersionLifecycle},
    runtime_ports::RuntimeInstanceRepository,
    runtime_ports::RuntimeRepository,
};
use larm_runtime::{
    endpoints::{EndpointProbe, EndpointProbeObservation, EndpointQualificationService},
    external_adoption::{
        ExternalAdoptionQualification, QualifiedAdoptionReferenceStore,
        QualifiedAdoptionReferences, ResolvedRuntimeVersion, RuntimeVersionResolver,
    },
};
use serde_json::{json, Map, Value};
use sqlx::SqlitePool;
use std::{collections::BTreeSet, sync::Arc, time::Duration};

/// Resuelve el pin P032 de la pareja `(variant_id, launch_profile_id)` para
/// leases del puente adoptado y exige una instalación de ese runtime/host.
pub struct SqlPinnedRuntimeVersionResolver {
    pool: SqlitePool,
}

impl SqlPinnedRuntimeVersionResolver {
    #[must_use]
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl RuntimeVersionResolver for SqlPinnedRuntimeVersionResolver {
    async fn resolve(
        &self,
        variant_id: &str,
        launch_profile_id: &str,
        configured_runtime_version_id: &str,
        host_id: &str,
    ) -> Result<ResolvedRuntimeVersion, ApiError> {
        if !larm_domain::bridge::is_uuid_v7(variant_id)
            || !larm_domain::bridge::is_uuid_v7(launch_profile_id)
            || !larm_domain::bridge::is_uuid_v7(configured_runtime_version_id)
            || !larm_domain::bridge::is_uuid_v7(host_id)
        {
            return Err(ApiError::new(
                "VALIDATION_FAILED",
                "La selección de versión recibió IDs no válidos",
                "runtime_pin_identity_invalid",
            ));
        }
        let runtimes = larm_db::runtimes::SqlRuntimes {
            pool: self.pool.clone(),
        };
        let configured = runtimes
            .version(configured_runtime_version_id)
            .await
            .map_err(|error| unavailable(error.details.reason.clone(), &error.message))?
            .ok_or_else(|| {
                unavailable(
                    "configured_runtime_version_missing",
                    "La versión configurada no existe en el catálogo",
                )
            })?;
        let runtime = runtimes
            .runtime(&configured.runtime_id)
            .await
            .map_err(|error| unavailable(error.details.reason.clone(), &error.message))?
            .ok_or_else(|| {
                unavailable(
                    "configured_runtime_missing",
                    "El runtime asociado a la versión configurada no existe",
                )
            })?;
        let pin = runtimes
            .pin_for(variant_id, launch_profile_id)
            .await
            .map_err(|error| unavailable(error.details.reason.clone(), &error.message))?;
        let pinned = pin.is_some();
        let selected_id = match pin {
            Some(value) => value.runtime_version_id,
            None => runtime.default_version_id.ok_or_else(|| {
                unavailable(
                    "runtime_default_missing",
                    "El runtime no tiene una versión predeterminada para nuevas leases",
                )
            })?,
        };
        let selected = if selected_id == configured.id {
            configured.clone()
        } else {
            runtimes
                .version(&selected_id)
                .await
                .map_err(|error| unavailable(error.details.reason.clone(), &error.message))?
                .ok_or_else(|| {
                    unavailable(
                        "pinned_runtime_version_missing",
                        "La versión fijada ya no existe en el catálogo",
                    )
                })?
        };
        if selected.runtime_id != configured.runtime_id {
            return Err(unavailable(
                "pinned_runtime_family_mismatch",
                "La versión fijada pertenece a otro runtime que el host adoptado",
            ));
        }
        if !matches!(
            selected.lifecycle,
            RuntimeVersionLifecycle::Validated
                | RuntimeVersionLifecycle::Default
                | RuntimeVersionLifecycle::Retained
        ) {
            return Err(unavailable(
                "runtime_version_unavailable",
                "La versión seleccionada dejó de estar validada o disponible",
            ));
        }
        let installations: Vec<String> = sqlx::query_scalar(
            "SELECT id FROM runtime_installation WHERE runtime_version_id=? \
             AND external_host_id=? AND strategy='EXTERNAL_ADOPTION' AND ownership='EXTERNAL' \
             ORDER BY id LIMIT 2",
        )
        .bind(&selected.id)
        .bind(host_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|error| unavailable("runtime_installation_query_failed", &error.to_string()))?;
        match installations.as_slice() {
            [_] => {}
            [] => {
                return Err(unavailable(
                    "pinned_installation_missing",
                    "El host adoptado no tiene instalada la versión seleccionada",
                ));
            }
            _ => {
                return Err(unavailable(
                    "pinned_installation_ambiguous",
                    "El host tiene instalaciones ambiguas para la versión seleccionada",
                ));
            }
        }
        Ok(ResolvedRuntimeVersion {
            runtime_version_id: selected.id,
            version_string: selected.version_string,
            pinned,
        })
    }
}

pub struct SqlQualifiedAdoptionReferenceStore {
    pool: SqlitePool,
    http: reqwest::Client,
}

impl SqlQualifiedAdoptionReferenceStore {
    pub fn new(pool: SqlitePool) -> Result<Self, String> {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .no_proxy()
            .build()
            .map_err(|error| format!("cliente de prueba P036 para runtime adoptado: {error}"))?;
        Ok(Self { pool, http })
    }

    async fn resolve_installation(
        &self,
        request: &ExternalAdoptionQualification,
    ) -> Result<String, ApiError> {
        let host_url: Option<String> = sqlx::query_scalar(
            "SELECT base_url FROM external_host WHERE id=? AND ownership='EXTERNAL'",
        )
        .bind(&request.host_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| unavailable("external_host_query_failed", &error.to_string()))?;
        let host_url = host_url.ok_or_else(|| {
            unavailable(
                "external_host_not_registered",
                "El host externo no está registrado como EXTERNAL",
            )
        })?;
        let endpoint_base = request.endpoint.base_url.trim_end_matches("/v1");
        if host_url.trim_end_matches('/') != endpoint_base.trim_end_matches('/') {
            return Err(unavailable(
                "external_host_url_mismatch",
                "La URL del endpoint no coincide con el host externo registrado",
            ));
        }

        let installations: Vec<String> = sqlx::query_scalar(
            "SELECT id FROM runtime_installation WHERE runtime_version_id=? \
             AND external_host_id=? AND strategy='EXTERNAL_ADOPTION' AND ownership='EXTERNAL' \
             ORDER BY id LIMIT 2",
        )
        .bind(&request.runtime_version_id)
        .bind(&request.host_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|error| unavailable("runtime_installation_query_failed", &error.to_string()))?;
        match installations.as_slice() {
            [installation] => Ok(installation.clone()),
            [] => Err(unavailable(
                "external_installation_not_registered",
                "No existe una instalación adoptada para el host y versión configurados",
            )),
            _ => Err(unavailable(
                "external_installation_ambiguous",
                "Hay más de una instalación adoptada para el mismo host y versión",
            )),
        }
    }

    async fn resolve_backend(
        &self,
        installation_id: &str,
        backend_type: &str,
    ) -> Result<String, ApiError> {
        let backends: Vec<String> = sqlx::query_scalar(
            "SELECT id FROM runtime_backend WHERE runtime_installation_id=? \
             AND backend_type=? AND status='AVAILABLE' ORDER BY id LIMIT 2",
        )
        .bind(installation_id)
        .bind(backend_type)
        .fetch_all(&self.pool)
        .await
        .map_err(|error| unavailable("runtime_backend_query_failed", &error.to_string()))?;
        match backends.as_slice() {
            [backend] => Ok(backend.clone()),
            [] => Err(unavailable(
                "observed_backend_not_registered",
                "El backend observado no está registrado como disponible",
            )),
            _ => Err(unavailable(
                "observed_backend_ambiguous",
                "Hay varios backends disponibles del tipo observado",
            )),
        }
    }

    async fn qualify_endpoint(
        &self,
        candidate: &larm_domain::protocol::InferenceEndpointCreate,
    ) -> Result<InferenceEndpoint, ApiError> {
        let embedding = candidate
            .capabilities
            .iter()
            .all(|capability| capability == "embedding.create");
        let (request_schema_id, response_schema_id) = if embedding {
            (
                "openai.embeddings.request.v1",
                "openai.embeddings.response.v1",
            )
        } else {
            (
                "openai.chat_completions.request.v1",
                "openai.chat_completions.response.v1",
            )
        };
        let mut protocols = ProtocolRegistry::default();
        protocols
            .register(ProtocolProfile {
                protocol: candidate.protocol.clone(),
                profile_hash: candidate.protocol_profile_hash.clone(),
                capabilities: candidate.capabilities.clone(),
                streaming: false,
                tools: false,
                request_schema_id: request_schema_id.into(),
                response_schema_id: response_schema_id.into(),
            })
            .map_err(|error| unavailable(error.details.reason.clone(), &error.message))?;
        let qualification = EndpointQualificationService::new(
            Arc::new(OllamaEndpointProbe {
                http: self.http.clone(),
            }),
            Arc::new(protocols),
            Duration::from_secs(5),
        )
        .map_err(|error| unavailable(error.details.reason.clone(), &error.message))?;
        let endpoint = qualification
            .qualify(candidate)
            .await
            .map_err(|error| unavailable(error.details.reason.clone(), &error.message))?;
        if endpoint.qualification != EndpointQualification::Qualified {
            return Err(ApiError::new(
                "RESOURCE_UNAVAILABLE",
                "El handshake de protocolo no cualificó el endpoint adoptado",
                "lease_endpoint_not_qualified",
            )
            .with_evidence(format!("probe_refs={:?}", endpoint.evidence_refs))
            .with_resource(candidate.instance_id.clone()));
        }
        Ok(endpoint)
    }
}

#[async_trait]
impl QualifiedAdoptionReferenceStore for SqlQualifiedAdoptionReferenceStore {
    async fn qualify_and_register(
        &self,
        request: &ExternalAdoptionQualification,
    ) -> Result<QualifiedAdoptionReferences, ApiError> {
        if request.endpoint.instance_id != request.instance_id
            || request.generation == 0
            || request.execution_fingerprint.len() != 64
            || !request
                .execution_fingerprint
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        {
            return Err(ApiError::new(
                "VALIDATION_FAILED",
                "La solicitud de cualificación no es coherente",
                "external_adoption_qualification_invalid",
            ));
        }

        let variant_repo = larm_db::variants::SqlVariants {
            pool: self.pool.clone(),
        };
        let variant = variant_repo
            .get_variant(&request.variant_id)
            .await?
            .ok_or_else(|| {
                unavailable(
                    "model_variant_not_registered",
                    "La variante adoptada no existe en el catálogo local",
                )
            })?;
        let requested: BTreeSet<_> = request.endpoint.capabilities.iter().collect();
        let configured: BTreeSet<_> = variant.capabilities.iter().collect();
        if !requested.is_subset(&configured) {
            return Err(unavailable(
                "model_variant_capabilities_mismatch",
                "Las capacidades del endpoint exceden las de la variante local",
            ));
        }

        let profile_repo = larm_db::launch_profiles::SqlLaunchProfiles {
            pool: self.pool.clone(),
        };
        let profile = profile_repo
            .launch_profile(&request.launch_profile_id)
            .await
            .map_err(|error| unavailable(error.details.reason.clone(), &error.message))?
            .ok_or_else(|| {
                unavailable(
                    "launch_profile_not_registered",
                    "El perfil de lanzamiento no existe en el catálogo local",
                )
            })?;
        if profile.name.as_str() != request.launch_profile_name
            || request
                .launch_profile_parameters
                .as_ref()
                .is_some_and(|parameters| {
                    serde_json::to_value(&profile.parameters).ok().as_ref() != Some(parameters)
                })
        {
            return Err(unavailable(
                "launch_profile_binding_mismatch",
                "El perfil configurado no coincide con el perfil local persistido",
            ));
        }

        let installation_id = self.resolve_installation(request).await?;
        let backend_id = self
            .resolve_backend(&installation_id, &request.backend_type)
            .await?;
        if profile
            .backend_id
            .as_deref()
            .is_some_and(|profile_backend| profile_backend != backend_id)
        {
            return Err(unavailable(
                "launch_profile_backend_mismatch",
                "El perfil está fijado a otro backend que el observado",
            ));
        }

        let endpoint = self.qualify_endpoint(&request.endpoint).await?;
        let now = timestamp(chrono::Utc::now());
        let mut metadata = Map::new();
        metadata.insert(
            "external_adoption".into(),
            json!({"host_id": request.host_id, "runtime_version_id": request.runtime_version_id}),
        );
        let instance = RuntimeInstance {
            id: request.instance_id.clone(),
            created_at: now.clone(),
            updated_at: now,
            row_version: 1,
            metadata,
            runtime_installation_id: installation_id,
            variant_id: request.variant_id.clone(),
            backend_id,
            launch_profile_id: request.launch_profile_id.clone(),
            state: InstanceState::Ready,
            pid: None,
            process_started_at: None,
            executable_hash: None,
            generation: request.generation,
            execution_fingerprint: Some(request.execution_fingerprint.clone()),
            last_used_at: None,
            ownership: RuntimeOwnership::External,
        };
        let context = MutationContext {
            actor_id: request.actor_id.clone(),
            correlation_id: uuid::Uuid::now_v7().to_string(),
        };
        larm_db::instances::SqlRuntimeInstances {
            pool: self.pool.clone(),
        }
        .register_external_qualified_instance(&instance, &endpoint, &context)
        .await
        .map_err(|error| unavailable(error.details.reason.clone(), &error.message))?;

        Ok(QualifiedAdoptionReferences {
            instance_id: instance.id,
            endpoint_id: endpoint.id,
        })
    }
}

struct OllamaEndpointProbe {
    http: reqwest::Client,
}

#[async_trait]
impl EndpointProbe for OllamaEndpointProbe {
    async fn probe(
        &self,
        endpoint: &larm_domain::protocol::InferenceEndpointCreate,
    ) -> Result<EndpointProbeObservation, larm_domain::error::LarmError> {
        let root = endpoint.base_url.trim_end_matches("/v1");
        let health_response = self.http.get(format!("{root}/api/version")).send().await;
        let (health_ok, health_status) = match health_response {
            Ok(response) => {
                let status = response.status().as_u16();
                let body = response.json::<Value>().await.unwrap_or(Value::Null);
                (
                    status == 200 && body.get("version").and_then(Value::as_str).is_some(),
                    Some(status),
                )
            }
            Err(_) => (false, None),
        };
        let models_response = self
            .http
            .get(format!(
                "{}/models",
                endpoint.base_url.trim_end_matches('/')
            ))
            .send()
            .await;
        let models_ok = match models_response {
            Ok(response) if response.status().as_u16() == 200 => response
                .json::<Value>()
                .await
                .is_ok_and(|body| body.get("data").and_then(Value::as_array).is_some()),
            _ => false,
        };
        Ok(EndpointProbeObservation {
            health_ok,
            health_status,
            protocol: models_ok.then(|| endpoint.protocol.clone()),
            protocol_profile_hash: models_ok.then(|| endpoint.protocol_profile_hash.clone()),
            capabilities: if models_ok {
                endpoint.capabilities.clone()
            } else {
                Vec::new()
            },
            evidence_refs: vec![
                match health_status {
                    Some(status) => format!("ollama:api-version:{status}"),
                    None => "ollama:api-version:connect_failed".into(),
                },
                if models_ok {
                    "openai:models:200".into()
                } else {
                    "openai:models:failed".into()
                },
            ],
            failure_reason: (!health_ok || !models_ok)
                .then(|| "external_adoption_endpoint_probe_failed".into()),
        })
    }
}

fn unavailable(reason: impl Into<String>, detail: &str) -> ApiError {
    ApiError::new(
        "RESOURCE_UNAVAILABLE",
        "No se pudieron cualificar las referencias del runtime adoptado",
        reason,
    )
    .retryable(true)
    .with_original("larm_daemon", "EXTERNAL_ADOPTION_QUALIFICATION", detail)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

    use super::*;
    use axum::{routing::get, Json, Router};
    use larm_domain::{
        launch::LaunchParameters,
        launch_profile::{LaunchProfileCreate, LaunchProfileKind, LaunchProfileRepository},
    };
    use tokio::sync::oneshot;

    fn id() -> String {
        uuid::Uuid::now_v7().to_string()
    }

    async fn seed_catalog(pool: &SqlitePool, base_url: &str) -> (String, String, String, String) {
        let host_id = id();
        let version_id = id();
        let installation_id = id();
        let backend_id = id();
        let variant_id = id();
        let runtime_id = id();
        let runtime_package_id = id();
        let runtime_adapter_id = id();
        let stamp = timestamp(chrono::Utc::now());
        for (statement, bindings) in [
            (
                "INSERT INTO external_host(registration_mode,id,created_at,updated_at,row_version,metadata,name,base_url,host_type,state,ownership) VALUES('ADOPTED',?,?,?,1,'{}','local test',?,'ollama','UNKNOWN','EXTERNAL')",
                vec![host_id.as_str(), stamp.as_str(), stamp.as_str(), base_url],
            ),
            (
                "INSERT INTO adapter_installation(id,created_at,updated_at,row_version,metadata,adapter_id,version,family,package_hash,publisher,trust,manifest,state) VALUES(?,?,?,1,'{}','runtime-adapter','1.0','runtime','hash','test','UNTRUSTED','{}','INSTALLED')",
                vec![runtime_package_id.as_str(), stamp.as_str(), stamp.as_str()],
            ),
            (
                "INSERT INTO runtime_adapter(id,created_at,updated_at,row_version,metadata,adapter_installation_id,runtime_family,capabilities) VALUES(?,?,?,1,'{}',?,'ollama','[]')",
                vec![runtime_adapter_id.as_str(), stamp.as_str(), stamp.as_str(), runtime_package_id.as_str()],
            ),
            (
                "INSERT INTO runtime(id,created_at,updated_at,row_version,metadata,channel_policy,name,adapter_id,capabilities) VALUES(?,?,?,1,'{}','{\"allowed_channels\":[\"stable\"]}','Ollama',?,'[]')",
                vec![runtime_id.as_str(), stamp.as_str(), stamp.as_str(), runtime_adapter_id.as_str()],
            ),
            (
                "INSERT INTO runtime_version(id,created_at,updated_at,row_version,metadata,runtime_id,version_string,channel,promotion) VALUES(?,?,?,1,'{}',?,'0.1.0','stable','NONE')",
                vec![version_id.as_str(), stamp.as_str(), stamp.as_str(), runtime_id.as_str()],
            ),
            (
                "INSERT INTO runtime_installation(registration_mode,stage,id,created_at,updated_at,row_version,metadata,runtime_version_id,strategy,external_host_id,state,ownership) VALUES('ADOPTED','DISCOVERED',?,?,?,1,'{}',?,'EXTERNAL_ADOPTION',?,'DISCOVERED','EXTERNAL')",
                vec![installation_id.as_str(), stamp.as_str(), stamp.as_str(), version_id.as_str(), host_id.as_str()],
            ),
            (
                "INSERT INTO runtime_backend(limits,id,created_at,updated_at,row_version,metadata,runtime_installation_id,backend_type,status,evidence_refs) VALUES('{\"max_instances\":1,\"multi_model_support\":true,\"parallel_requests\":1,\"gpu_sharing\":false}',?,?,?,1,'{}',?,'GPU','AVAILABLE','[\"ollama:/api/ps\"]')",
                vec![backend_id.as_str(), stamp.as_str(), stamp.as_str(), installation_id.as_str()],
            ),
            (
                "INSERT INTO adapter_installation(id,created_at,updated_at,row_version,metadata,adapter_id,version,family,package_hash,publisher,trust,manifest,state) VALUES('source-adapter-package',?,?,1,'{}','source-adapter','1.0','source','hash','test','UNTRUSTED','{}','INSTALLED')",
                vec![stamp.as_str(), stamp.as_str()],
            ),
            (
                "INSERT INTO model_source_adapter(id,created_at,updated_at,row_version,metadata,adapter_installation_id,source_type,capabilities) VALUES('source-adapter',?,?,1,'{}','source-adapter-package','HTTP','{}')",
                vec![stamp.as_str(), stamp.as_str()],
            ),
            (
                "INSERT INTO canonical_model(id,created_at,updated_at,row_version,metadata,name,capabilities,possible_duplicates) VALUES('canonical',?,?,1,'{}','Test model','[]','[]')",
                vec![stamp.as_str(), stamp.as_str()],
            ),
            (
                "INSERT INTO model_source(id,created_at,updated_at,row_version,metadata,name,adapter_id,base_url,priority,enabled,auth_mode,policy) VALUES('source',?,?,1,'{}','Test source','source-adapter','https://example.test',0,0,'NONE','{}')",
                vec![stamp.as_str(), stamp.as_str()],
            ),
            (
                "INSERT INTO model_distribution(id,created_at,updated_at,row_version,metadata,canonical_model_id,source_id,upstream_namespace,upstream_model_id) VALUES('distribution',?,?,1,'{}','canonical','source','test','model')",
                vec![stamp.as_str(), stamp.as_str()],
            ),
            (
                "INSERT INTO model_revision(id,created_at,updated_at,row_version,metadata,distribution_id,revision_key,manifest_hash,state) VALUES('revision',?,?,1,'{}','distribution','main','hash','CURRENT')",
                vec![stamp.as_str(), stamp.as_str()],
            ),
        ] {
            let mut query = sqlx::query(statement);
            for binding in bindings {
                query = query.bind(binding);
            }
            query
                .execute(pool)
                .await
                .unwrap_or_else(|error| panic!("seed runtime registry: {error}"));
        }
        sqlx::query(
            "INSERT INTO model_variant(id,created_at,updated_at,row_version,metadata,revision_id,status,lifecycle,availability,ownership,promotion,system_role,capabilities,model_limits,update_available) VALUES(?,?,?,1,'{}','revision','DISCOVERED','DISCOVERED','UNKNOWN','LARM_MANAGED','NONE','GENERAL','[\"text.generate\"]','{}',0)",
        )
        .bind(&variant_id)
        .bind(&stamp)
        .bind(&stamp)
        .execute(pool)
        .await
        .unwrap_or_else(|error| panic!("seed model variant: {error}"));
        (
            host_id,
            version_id,
            installation_id,
            format!("{backend_id}|{variant_id}"),
        )
    }

    #[tokio::test]
    async fn pinned_runtime_resolver_uses_pair_pin_and_fails_closed_when_unavailable() {
        let (_directory, db) = larm_db::testing::temp_migrated()
            .await
            .unwrap_or_else(|error| panic!("db: {error}"));
        let base_url = "http://127.0.0.1:12345";
        let (host_id, configured_version_id, _installation_id, bindings) =
            seed_catalog(&db.pool, base_url).await;
        let (_, variant_id) = bindings
            .split_once('|')
            .unwrap_or_else(|| panic!("seeded backend and variant"));
        let profile_id = id();
        let stamp = timestamp(chrono::Utc::now());
        sqlx::query(
            "INSERT INTO launch_profile(id,created_at,updated_at,row_version,metadata,name,parameters,profile_hash,backend_id) VALUES(?,?,?,1,'{}','BALANCED','{}','profile-hash',NULL)",
        )
        .bind(&profile_id)
        .bind(&stamp)
        .bind(&stamp)
        .execute(&db.pool)
        .await
        .unwrap_or_else(|error| panic!("seed launch profile: {error}"));

        let runtime_id: String =
            sqlx::query_scalar("SELECT runtime_id FROM runtime_version WHERE id=?")
                .bind(&configured_version_id)
                .fetch_one(&db.pool)
                .await
                .unwrap_or_else(|error| panic!("read runtime family id: {error}"));
        let default_version_id = id();
        let default_installation_id = id();
        sqlx::query(
            "INSERT INTO runtime_version(id,created_at,updated_at,row_version,metadata,runtime_id,version_string,channel,promotion,lifecycle) VALUES(?,?,?,1,'{}',?,'0.2.0','stable','DEFAULT','DEFAULT')",
        )
        .bind(&default_version_id)
        .bind(&stamp)
        .bind(&stamp)
        .bind(&runtime_id)
        .execute(&db.pool)
        .await
        .unwrap_or_else(|error| panic!("seed default version: {error}"));
        sqlx::query(
            "INSERT INTO runtime_installation(registration_mode,stage,id,created_at,updated_at,row_version,metadata,runtime_version_id,strategy,external_host_id,state,ownership) VALUES('ADOPTED','VALIDATED',?,?,?,1,'{}',?,'EXTERNAL_ADOPTION',?,'VALIDATED','EXTERNAL')",
        )
        .bind(&default_installation_id)
        .bind(&stamp)
        .bind(&stamp)
        .bind(&default_version_id)
        .bind(&host_id)
        .execute(&db.pool)
        .await
        .unwrap_or_else(|error| panic!("seed default installation: {error}"));
        sqlx::query("UPDATE runtime SET default_version_id=? WHERE id=?")
            .bind(&default_version_id)
            .bind(&runtime_id)
            .execute(&db.pool)
            .await
            .unwrap_or_else(|error| panic!("set runtime default: {error}"));

        let resolver = SqlPinnedRuntimeVersionResolver::new(db.pool.clone());
        let resolved_default = resolver
            .resolve(variant_id, &profile_id, &configured_version_id, &host_id)
            .await
            .unwrap_or_else(|error| {
                panic!(
                    "resolve unpinned default ({}): {error}",
                    error.details.reason
                )
            });
        assert_eq!(resolved_default.runtime_version_id, default_version_id);
        assert_eq!(resolved_default.version_string, "0.2.0");
        assert!(!resolved_default.pinned);

        let pinned_version_id = id();
        let pinned_installation_id = id();
        sqlx::query(
            "INSERT INTO runtime_version(id,created_at,updated_at,row_version,metadata,runtime_id,version_string,channel,promotion,lifecycle) VALUES(?,?,?,1,'{}',?,'0.3.0','stable','NONE','VALIDATED')",
        )
        .bind(&pinned_version_id)
        .bind(&stamp)
        .bind(&stamp)
        .bind(&runtime_id)
        .execute(&db.pool)
        .await
        .unwrap_or_else(|error| panic!("seed pinned version: {error}"));
        sqlx::query(
            "INSERT INTO runtime_installation(registration_mode,stage,id,created_at,updated_at,row_version,metadata,runtime_version_id,strategy,external_host_id,state,ownership) VALUES('ADOPTED','VALIDATED',?,?,?,1,'{}',?,'EXTERNAL_ADOPTION',?,'VALIDATED','EXTERNAL')",
        )
        .bind(&pinned_installation_id)
        .bind(&stamp)
        .bind(&stamp)
        .bind(&pinned_version_id)
        .bind(&host_id)
        .execute(&db.pool)
        .await
        .unwrap_or_else(|error| panic!("seed pinned installation: {error}"));
        sqlx::query(
            "INSERT INTO runtime_version_pin(id,created_at,updated_at,row_version,metadata,variant_id,launch_profile_id,runtime_version_id) VALUES(?,?,?,1,'{}',?,?,?)",
        )
        .bind(id())
        .bind(&stamp)
        .bind(&stamp)
        .bind(variant_id)
        .bind(&profile_id)
        .bind(&pinned_version_id)
        .execute(&db.pool)
        .await
        .unwrap_or_else(|error| panic!("seed runtime pin: {error}"));

        let resolved = resolver
            .resolve(variant_id, &profile_id, &configured_version_id, &host_id)
            .await
            .unwrap_or_else(|error| panic!("resolve pinned version: {error}"));
        assert_eq!(resolved.runtime_version_id, pinned_version_id);
        assert_eq!(resolved.version_string, "0.3.0");
        assert!(resolved.pinned);

        sqlx::query("DELETE FROM runtime_installation WHERE id=?")
            .bind(&pinned_installation_id)
            .execute(&db.pool)
            .await
            .unwrap_or_else(|error| panic!("remove pinned installation: {error}"));
        let error = resolver
            .resolve(variant_id, &profile_id, &configured_version_id, &host_id)
            .await
            .err()
            .unwrap_or_else(|| panic!("pin without a matching host installation must fail"));
        assert_eq!(error.details.reason, "pinned_installation_missing");

        sqlx::query("DELETE FROM runtime_version_pin WHERE variant_id=? AND launch_profile_id=?")
            .bind(variant_id)
            .bind(&profile_id)
            .execute(&db.pool)
            .await
            .unwrap_or_else(|error| panic!("remove pair pin: {error}"));
        let unpinned = resolver
            .resolve(variant_id, &profile_id, &configured_version_id, &host_id)
            .await
            .unwrap_or_else(|error| panic!("resolve default after unpin: {error}"));
        assert_eq!(unpinned.runtime_version_id, default_version_id);
        assert!(!unpinned.pinned);

        sqlx::query("UPDATE runtime SET default_version_id=NULL WHERE id=?")
            .bind(&runtime_id)
            .execute(&db.pool)
            .await
            .unwrap_or_else(|error| panic!("remove runtime default: {error}"));
        let error = resolver
            .resolve(variant_id, &profile_id, &configured_version_id, &host_id)
            .await
            .err()
            .unwrap_or_else(|| {
                panic!("missing default must not fall back to the configured version")
            });
        assert_eq!(error.details.reason, "runtime_default_missing");
    }

    #[tokio::test]
    async fn reference_store_requires_catalog_bindings_and_http_qualification() {
        let (_directory, db) = larm_db::testing::temp_migrated()
            .await
            .unwrap_or_else(|error| panic!("db: {error}"));
        let app = Router::new()
            .route(
                "/api/version",
                get(|| async { Json(json!({"version": "0.1.0"})) }),
            )
            .route(
                "/v1/models",
                get(|| async { Json(json!({"data": [{"id": "test-model"}]})) }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .unwrap_or_else(|error| panic!("bind loopback: {error}"));
        let address = listener
            .local_addr()
            .unwrap_or_else(|error| panic!("loopback address: {error}"));
        let base_url = format!("http://{address}");
        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        let server = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = shutdown_rx.await;
                })
                .await
                .unwrap_or_else(|error| panic!("loopback server: {error}"));
        });

        let (host_id, runtime_version_id, installation_id, bindings) =
            seed_catalog(&db.pool, &base_url).await;
        let (backend_id, variant_id) = bindings
            .split_once('|')
            .unwrap_or_else(|| panic!("seeded backend and variant"));
        let profile_repo = larm_db::launch_profiles::SqlLaunchProfiles {
            pool: db.pool.clone(),
        };
        let parameters = LaunchParameters::builtin(backend_id, 4, Some(4096), 128);
        let profile = LaunchProfileCreate {
            name: LaunchProfileKind::Balanced,
            parameters: parameters.clone(),
            backend_id: None,
        };
        profile_repo
            .create_launch_profile(
                &profile,
                &"a".repeat(64),
                "adoption-reference-profile",
                &MutationContext {
                    actor_id: id(),
                    correlation_id: id(),
                },
            )
            .await
            .unwrap_or_else(|error| panic!("create launch profile: {error}"));
        let stored_profile_id: String = sqlx::query_scalar("SELECT id FROM launch_profile LIMIT 1")
            .fetch_one(&db.pool)
            .await
            .unwrap_or_else(|error| panic!("read launch profile id: {error}"));
        let generation = 7;
        let instance_id = id();
        let endpoint_candidate = larm_domain::protocol::InferenceEndpointCreate {
            instance_id: instance_id.clone(),
            protocol: "openai".into(),
            protocol_profile_hash: "a".repeat(64),
            base_url: format!("{base_url}/v1"),
            credential_reference_id: None,
            capabilities: vec!["text.generate".into()],
            qualification: EndpointQualification::Untested,
            evidence_refs: Vec::new(),
        };
        let request = ExternalAdoptionQualification {
            instance_id: instance_id.clone(),
            host_id,
            runtime_version_id,
            variant_id: variant_id.into(),
            launch_profile_id: stored_profile_id,
            launch_profile_name: "BALANCED".into(),
            launch_profile_parameters: Some(serde_json::to_value(parameters).unwrap()),
            backend_type: "GPU".into(),
            generation,
            execution_fingerprint: "b".repeat(64),
            endpoint: endpoint_candidate,
            actor_id: id(),
        };
        assert!(
            sqlx::query_scalar::<_, String>("SELECT id FROM runtime_installation WHERE id=?")
                .bind(&installation_id)
                .fetch_optional(&db.pool)
                .await
                .unwrap()
                .is_some()
        );

        let store = SqlQualifiedAdoptionReferenceStore::new(db.pool.clone())
            .unwrap_or_else(|error| panic!("reference store: {error}"));
        let mut wrong_backend = request.clone();
        wrong_backend.backend_type = "CPU".into();
        let rejected = store
            .qualify_and_register(&wrong_backend)
            .await
            .unwrap_err();
        assert_eq!(rejected.details.reason, "observed_backend_not_registered");
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM runtime_instance")
                .fetch_one(&db.pool)
                .await
                .unwrap(),
            0
        );

        let refs = store
            .qualify_and_register(&request)
            .await
            .unwrap_or_else(|error| panic!("qualify references: {error:?}"));
        assert_eq!(refs.instance_id, request.instance_id);
        let instance = larm_db::instances::SqlRuntimeInstances {
            pool: db.pool.clone(),
        }
        .instance(&refs.instance_id)
        .await
        .unwrap_or_else(|error| panic!("read persisted instance: {error}"))
        .unwrap_or_else(|| panic!("persisted instance missing"));
        assert_eq!(instance.state, InstanceState::Ready);
        assert_eq!(instance.ownership, RuntimeOwnership::External);
        assert_eq!(instance.generation, generation);
        assert_eq!(instance.backend_id, backend_id);
        let endpoint: InferenceEndpoint = serde_json::from_str(
            &sqlx::query_scalar::<_, String>("SELECT json_object('id',id,'created_at',created_at,'updated_at',updated_at,'row_version',row_version,'metadata',json(metadata),'instance_id',instance_id,'protocol',protocol,'protocol_profile_hash',protocol_profile_hash,'base_url',base_url,'credential_reference_id',credential_reference_id,'capabilities',json(capabilities),'qualification',qualification,'evidence_refs',json(evidence_refs)) FROM inference_endpoint WHERE id=?")
                .bind(&refs.endpoint_id)
                .fetch_one(&db.pool)
                .await
                .unwrap_or_else(|error| panic!("read persisted endpoint: {error}")),
        )
        .unwrap_or_else(|error| panic!("decode persisted endpoint: {error}"));
        assert_eq!(endpoint.qualification, EndpointQualification::Qualified);
        assert!(endpoint.evidence_refs.contains(&"openai:models:200".into()));
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM event_outbox")
                .fetch_one(&db.pool)
                .await
                .unwrap(),
            3
        );

        let _ = shutdown_tx.send(());
        server.await.unwrap();
    }
}
