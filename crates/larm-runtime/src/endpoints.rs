//! Registration and revalidation of probed instance endpoints (P036).

use std::{collections::BTreeSet, sync::Arc, time::Duration};

use async_trait::async_trait;
use chrono::Utc;
use larm_domain::{
    bridge::timestamp,
    error::{ErrorCode, LarmError},
    ports::MutationContext,
    protocol::{
        EndpointQualification, InferenceEndpoint, InferenceEndpointCreate, ProtocolRegistry,
        ProtocolResolution,
    },
    runtime_ports::InferenceEndpointRepository,
};
use serde_json::{json, Map};

/// Evidence returned by an adapter-specific health and protocol handshake.
/// I/O failures are observations (`health_ok=false` or no protocol), while an
/// `Err` is reserved for a broken probe implementation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndpointProbeObservation {
    pub health_ok: bool,
    pub health_status: Option<u16>,
    pub protocol: Option<String>,
    pub protocol_profile_hash: Option<String>,
    pub capabilities: Vec<String>,
    pub evidence_refs: Vec<String>,
    pub failure_reason: Option<String>,
}

impl EndpointProbeObservation {
    fn validate(&self) -> Result<(), LarmError> {
        if self.evidence_refs.is_empty()
            || self.evidence_refs.iter().any(|reference| {
                reference.trim().is_empty() || reference.chars().any(char::is_control)
            })
            || self.capabilities.iter().any(|capability| {
                capability.trim().is_empty() || capability.chars().any(char::is_control)
            })
            || self
                .protocol
                .as_deref()
                .is_some_and(|value| value.trim().is_empty())
            || self
                .protocol_profile_hash
                .as_deref()
                .is_some_and(|value| value.trim().is_empty())
        {
            return Err(invalid("endpoint_probe_observation_invalid"));
        }
        Ok(())
    }
}

/// Runtime-family adapter that performs the actual health and protocol probe.
#[async_trait]
pub trait EndpointProbe: Send + Sync {
    async fn probe(
        &self,
        endpoint: &InferenceEndpointCreate,
    ) -> Result<EndpointProbeObservation, LarmError>;
}

/// P036 qualification without persistence, used when another P033 repository
/// must commit the instance and endpoint together in one transaction.
pub struct EndpointQualificationService {
    probe: Arc<dyn EndpointProbe>,
    protocols: Arc<ProtocolRegistry>,
    timeout: Duration,
}

impl EndpointQualificationService {
    pub fn new(
        probe: Arc<dyn EndpointProbe>,
        protocols: Arc<ProtocolRegistry>,
        timeout: Duration,
    ) -> Result<Self, LarmError> {
        if timeout.is_zero() {
            return Err(invalid("endpoint_probe_timeout_invalid"));
        }
        Ok(Self {
            probe,
            protocols,
            timeout,
        })
    }

    /// Returns the complete probe-derived endpoint record without storing it.
    pub async fn qualify(
        &self,
        candidate: &InferenceEndpointCreate,
    ) -> Result<InferenceEndpoint, LarmError> {
        candidate.validate()?;
        let observation = self.probe_with_timeout(candidate).await?;
        observation.validate()?;
        let endpoint = self.record(candidate, observation);
        endpoint.validate_record()?;
        Ok(endpoint)
    }

    async fn probe_with_timeout(
        &self,
        candidate: &InferenceEndpointCreate,
    ) -> Result<EndpointProbeObservation, LarmError> {
        match tokio::time::timeout(self.timeout, self.probe.probe(candidate)).await {
            Ok(result) => result,
            Err(_) => Ok(EndpointProbeObservation {
                health_ok: false,
                health_status: None,
                protocol: None,
                protocol_profile_hash: None,
                capabilities: Vec::new(),
                evidence_refs: vec!["endpoint_probe:timeout".into()],
                failure_reason: Some("probe_timeout".into()),
            }),
        }
    }

    fn record(
        &self,
        candidate: &InferenceEndpointCreate,
        observation: EndpointProbeObservation,
    ) -> InferenceEndpoint {
        let qualified = self.qualifies(candidate, &observation);
        let now = timestamp(Utc::now());
        InferenceEndpoint {
            id: uuid::Uuid::now_v7().to_string(),
            created_at: now.clone(),
            updated_at: now,
            row_version: 1,
            metadata: probe_metadata(&observation),
            instance_id: candidate.instance_id.clone(),
            protocol: candidate.protocol.clone(),
            protocol_profile_hash: candidate.protocol_profile_hash.clone(),
            base_url: candidate.base_url.clone(),
            credential_reference_id: candidate.credential_reference_id.clone(),
            capabilities: observation.capabilities,
            qualification: if qualified {
                EndpointQualification::Qualified
            } else {
                EndpointQualification::Failed
            },
            evidence_refs: observation.evidence_refs,
        }
    }

    fn qualifies(
        &self,
        candidate: &InferenceEndpointCreate,
        observation: &EndpointProbeObservation,
    ) -> bool {
        let ProtocolResolution::Known(profile) = self
            .protocols
            .resolve(&candidate.protocol, &candidate.protocol_profile_hash)
        else {
            return false;
        };
        let requested = candidate.capabilities.iter().collect::<BTreeSet<_>>();
        let observed = observation.capabilities.iter().collect::<BTreeSet<_>>();
        let registered = profile.capabilities.iter().collect::<BTreeSet<_>>();
        observation.health_ok
            && observation.protocol.as_deref() == Some(candidate.protocol.as_str())
            && observation.protocol_profile_hash.as_deref()
                == Some(candidate.protocol_profile_hash.as_str())
            && observed == requested
            && requested.is_subset(&registered)
    }
}

/// Owns endpoint qualification policy. Unknown protocols and profile versions
/// are retained as FAILED records with their original identifier and evidence.
pub struct EndpointRegistryService {
    repo: Arc<dyn InferenceEndpointRepository>,
    qualification: EndpointQualificationService,
}

impl EndpointRegistryService {
    pub fn new(
        repo: Arc<dyn InferenceEndpointRepository>,
        probe: Arc<dyn EndpointProbe>,
        protocols: Arc<ProtocolRegistry>,
        timeout: Duration,
    ) -> Result<Self, LarmError> {
        Ok(Self {
            repo,
            qualification: EndpointQualificationService::new(probe, protocols, timeout)?,
        })
    }

    /// Persists a record only after the adapter returned health/protocol probe
    /// evidence. The request's qualification and evidence are never trusted.
    pub async fn register(
        &self,
        candidate: &InferenceEndpointCreate,
        ctx: &MutationContext,
    ) -> Result<InferenceEndpoint, LarmError> {
        let endpoint = self.qualification.qualify(candidate).await?;
        self.repo.register_endpoint(&endpoint, ctx).await?;
        Ok(endpoint)
    }

    /// Rechecks a qualified endpoint. A dead endpoint becomes FAILED and is
    /// removed from the Broker-facing qualified view; restart creates a new ID.
    pub async fn revalidate(
        &self,
        id: &str,
        ctx: &MutationContext,
    ) -> Result<InferenceEndpoint, LarmError> {
        let endpoint = self.repo.endpoint(id).await?.ok_or_else(|| {
            LarmError::new(
                ErrorCode::NotFound,
                "Endpoint desconocido",
                "inference_endpoint_not_found",
            )
            .with_resource(id.to_owned())
        })?;
        if endpoint.qualification == EndpointQualification::Failed {
            return Ok(endpoint);
        }
        let candidate = InferenceEndpointCreate {
            instance_id: endpoint.instance_id.clone(),
            protocol: endpoint.protocol.clone(),
            protocol_profile_hash: endpoint.protocol_profile_hash.clone(),
            base_url: endpoint.base_url.clone(),
            credential_reference_id: endpoint.credential_reference_id.clone(),
            capabilities: endpoint.capabilities.clone(),
            qualification: EndpointQualification::Untested,
            evidence_refs: Vec::new(),
        };
        let observation = self.qualification.probe_with_timeout(&candidate).await?;
        observation.validate()?;
        if self.qualification.qualifies(&candidate, &observation) {
            return Ok(endpoint);
        }
        self.repo
            .fail_endpoint(id, endpoint.row_version, &observation.evidence_refs, ctx)
            .await
    }

    /// Read-only endpoints that may be returned to a Broker for an active instance.
    pub async fn qualified_for_instance(
        &self,
        instance_id: &str,
    ) -> Result<Vec<InferenceEndpoint>, LarmError> {
        self.repo
            .qualified_endpoints_for_instance(instance_id)
            .await
    }
}

fn probe_metadata(observation: &EndpointProbeObservation) -> Map<String, serde_json::Value> {
    let mut metadata = Map::new();
    metadata.insert(
        "probe".into(),
        json!({
            "health_ok": observation.health_ok,
            "health_status": observation.health_status,
            "protocol": observation.protocol,
            "protocol_profile_hash": observation.protocol_profile_hash,
            "failure_reason": observation.failure_reason,
        }),
    );
    metadata
}

fn invalid(reason: &'static str) -> LarmError {
    LarmError::new(
        ErrorCode::ValidationFailed,
        "Sonda de endpoint no válida",
        reason,
    )
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

    use std::{collections::VecDeque, sync::Mutex};

    use super::*;
    use larm_domain::protocol::ProtocolProfile;

    #[derive(Default)]
    struct MemoryEndpoints(Mutex<Vec<InferenceEndpoint>>);

    #[async_trait]
    impl InferenceEndpointRepository for MemoryEndpoints {
        async fn register_endpoint(
            &self,
            endpoint: &InferenceEndpoint,
            _ctx: &MutationContext,
        ) -> Result<(), LarmError> {
            endpoint.validate_record()?;
            self.0.lock().unwrap().push(endpoint.clone());
            Ok(())
        }

        async fn endpoint(&self, id: &str) -> Result<Option<InferenceEndpoint>, LarmError> {
            Ok(self
                .0
                .lock()
                .unwrap()
                .iter()
                .find(|row| row.id == id)
                .cloned())
        }

        async fn endpoints_for_instance(
            &self,
            instance_id: &str,
        ) -> Result<Vec<InferenceEndpoint>, LarmError> {
            Ok(self
                .0
                .lock()
                .unwrap()
                .iter()
                .filter(|row| row.instance_id == instance_id)
                .cloned()
                .collect())
        }

        async fn fail_endpoint(
            &self,
            id: &str,
            expected_row_version: i64,
            evidence_refs: &[String],
            _ctx: &MutationContext,
        ) -> Result<InferenceEndpoint, LarmError> {
            let mut rows = self.0.lock().unwrap();
            let row = rows.iter_mut().find(|row| row.id == id).ok_or_else(|| {
                LarmError::new(
                    ErrorCode::NotFound,
                    "Endpoint desconocido",
                    "inference_endpoint_not_found",
                )
            })?;
            if row.row_version != expected_row_version {
                return Err(LarmError::new(
                    ErrorCode::StaleRevision,
                    "El endpoint cambió desde que se leyó",
                    "stale_revision",
                ));
            }
            row.qualification = EndpointQualification::Failed;
            row.row_version += 1;
            row.updated_at = timestamp(Utc::now());
            row.evidence_refs.extend_from_slice(evidence_refs);
            row.evidence_refs.sort();
            row.evidence_refs.dedup();
            Ok(row.clone())
        }
    }

    struct QueueProbe(Mutex<VecDeque<Result<EndpointProbeObservation, LarmError>>>);

    #[async_trait]
    impl EndpointProbe for QueueProbe {
        async fn probe(
            &self,
            _endpoint: &InferenceEndpointCreate,
        ) -> Result<EndpointProbeObservation, LarmError> {
            self.0.lock().unwrap().pop_front().expect("probe result")
        }
    }

    struct SlowProbe;

    #[async_trait]
    impl EndpointProbe for SlowProbe {
        async fn probe(
            &self,
            _endpoint: &InferenceEndpointCreate,
        ) -> Result<EndpointProbeObservation, LarmError> {
            tokio::time::sleep(Duration::from_secs(1)).await;
            Ok(EndpointProbeObservation {
                health_ok: true,
                health_status: Some(200),
                protocol: Some("openai".into()),
                protocol_profile_hash: Some("a".repeat(64)),
                capabilities: vec!["text.generate".into()],
                evidence_refs: vec!["probe:slow".into()],
                failure_reason: None,
            })
        }
    }

    fn profile(protocol: &str, hash: &str) -> ProtocolProfile {
        ProtocolProfile {
            protocol: protocol.into(),
            profile_hash: hash.into(),
            capabilities: vec!["text.generate".into()],
            streaming: true,
            tools: false,
            request_schema_id: "request.v1".into(),
            response_schema_id: "response.v1".into(),
        }
    }

    fn candidate(instance: u16, protocol: &str, hash: &str) -> InferenceEndpointCreate {
        InferenceEndpointCreate {
            instance_id: format!("0192aaaa-0000-7000-8000-{instance:012x}"),
            protocol: protocol.into(),
            protocol_profile_hash: hash.into(),
            base_url: "http://127.0.0.1:18080/v1".into(),
            credential_reference_id: None,
            capabilities: vec!["text.generate".into()],
            qualification: EndpointQualification::Untested,
            evidence_refs: Vec::new(),
        }
    }

    fn observation(protocol: &str, hash: &str, health_ok: bool) -> EndpointProbeObservation {
        EndpointProbeObservation {
            health_ok,
            health_status: Some(if health_ok { 200 } else { 503 }),
            protocol: Some(protocol.into()),
            protocol_profile_hash: Some(hash.into()),
            capabilities: vec!["text.generate".into()],
            evidence_refs: vec!["probe:fixture".into()],
            failure_reason: (!health_ok).then(|| "health_status_mismatch".into()),
        }
    }

    fn service(
        repo: Arc<MemoryEndpoints>,
        probe: Arc<QueueProbe>,
        registry: ProtocolRegistry,
        timeout: Duration,
    ) -> EndpointRegistryService {
        EndpointRegistryService::new(repo, probe, Arc::new(registry), timeout).unwrap()
    }

    fn ctx() -> MutationContext {
        MutationContext {
            actor_id: "p036-test".into(),
            correlation_id: "0192aaaa-0000-7000-8000-000000000099".into(),
        }
    }

    #[tokio::test]
    async fn registra_varios_protocolos_y_preserva_unknown_sin_publicarlo_al_broker() {
        let hash_a = "a".repeat(64);
        let hash_b = "b".repeat(64);
        let mut registry = ProtocolRegistry::default();
        registry.register(profile("openai", &hash_a)).unwrap();
        registry.register(profile("ollama", &hash_b)).unwrap();
        let repo = Arc::new(MemoryEndpoints::default());
        let probe = Arc::new(QueueProbe(Mutex::new(VecDeque::from([
            Ok(observation("openai", &hash_a, true)),
            Ok(observation("ollama", &hash_b, true)),
            Ok(observation("vendor.custom/v7", &"c".repeat(64), true)),
        ]))));
        let service = service(repo.clone(), probe, registry, Duration::from_secs(1));
        let cases = [
            ("openai".to_owned(), hash_a),
            ("ollama".to_owned(), hash_b),
            ("vendor.custom/v7".to_owned(), "c".repeat(64)),
        ];
        for (protocol, hash) in &cases {
            service
                .register(&candidate(1, protocol, hash), &ctx())
                .await
                .unwrap();
        }
        let rows = repo
            .endpoints_for_instance("0192aaaa-0000-7000-8000-000000000001")
            .await
            .unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[2].protocol, "vendor.custom/v7");
        assert_eq!(rows[2].qualification, EndpointQualification::Failed);
        assert_eq!(
            service
                .qualified_for_instance("0192aaaa-0000-7000-8000-000000000001")
                .await
                .unwrap()
                .len(),
            2
        );
    }

    #[tokio::test]
    async fn endpoint_dead_se_degrada_y_un_nuevo_probe_puede_recuperar_el_servicio() {
        let hash = "a".repeat(64);
        let mut registry = ProtocolRegistry::default();
        registry.register(profile("openai", &hash)).unwrap();
        let repo = Arc::new(MemoryEndpoints::default());
        let probe = Arc::new(QueueProbe(Mutex::new(VecDeque::from([
            Ok(observation("openai", &hash, true)),
            Ok(observation("openai", &hash, false)),
            Ok(observation("openai", &hash, true)),
        ]))));
        let service = service(repo.clone(), probe, registry, Duration::from_secs(1));
        let first = service
            .register(&candidate(1, "openai", &hash), &ctx())
            .await
            .unwrap();
        let failed = service.revalidate(&first.id, &ctx()).await.unwrap();
        assert_eq!(failed.qualification, EndpointQualification::Failed);
        assert!(service
            .qualified_for_instance(&first.instance_id)
            .await
            .unwrap()
            .is_empty());
        let recovered = service
            .register(&candidate(1, "openai", &hash), &ctx())
            .await
            .unwrap();
        assert_ne!(recovered.id, first.id);
        assert_eq!(recovered.qualification, EndpointQualification::Qualified);
        assert_eq!(
            service
                .qualified_for_instance(&first.instance_id)
                .await
                .unwrap()
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn timeout_queda_failed_y_error_interno_no_crea_endpoint_parcial() {
        let hash = "a".repeat(64);
        let mut registry = ProtocolRegistry::default();
        registry.register(profile("openai", &hash)).unwrap();
        let repo = Arc::new(MemoryEndpoints::default());
        let timeout_service = EndpointRegistryService::new(
            repo.clone(),
            Arc::new(SlowProbe),
            Arc::new(registry.clone()),
            Duration::from_millis(1),
        )
        .unwrap();
        let timed_out = timeout_service
            .register(&candidate(1, "openai", &hash), &ctx())
            .await
            .unwrap();
        assert_eq!(timed_out.qualification, EndpointQualification::Failed);
        assert!(timed_out
            .evidence_refs
            .contains(&"endpoint_probe:timeout".into()));

        let error_probe = Arc::new(QueueProbe(Mutex::new(VecDeque::from([Err(
            LarmError::new(
                ErrorCode::Internal,
                "Falló la implementación de la sonda",
                "probe_implementation_failed",
            ),
        )]))));
        let error_service = service(repo.clone(), error_probe, registry, Duration::from_secs(1));
        assert!(error_service
            .register(&candidate(1, "openai", &hash), &ctx())
            .await
            .is_err());
        assert_eq!(
            repo.endpoints_for_instance("0192aaaa-0000-7000-8000-000000000001")
                .await
                .unwrap()
                .len(),
            1
        );
    }

    #[cfg(windows)]
    struct WindowsLoopbackProbe {
        client: reqwest::Client,
    }

    #[cfg(windows)]
    #[async_trait]
    impl EndpointProbe for WindowsLoopbackProbe {
        async fn probe(
            &self,
            endpoint: &InferenceEndpointCreate,
        ) -> Result<EndpointProbeObservation, LarmError> {
            let root = endpoint.base_url.trim_end_matches("/v1");
            let health = self.client.get(format!("{root}/health")).send().await;
            let (health_ok, health_status) = match health {
                Ok(response) => {
                    let status = response.status().as_u16();
                    let body = response
                        .json::<serde_json::Value>()
                        .await
                        .unwrap_or_default();
                    (status == 200 && body["status"] == "ok", Some(status))
                }
                Err(_) => (false, None),
            };
            let protocol = self
                .client
                .get(format!(
                    "{}/models",
                    endpoint.base_url.trim_end_matches('/')
                ))
                .send()
                .await;
            let protocol_ok = match protocol {
                Ok(response) if response.status().as_u16() == 200 => response
                    .json::<serde_json::Value>()
                    .await
                    .is_ok_and(|body| body["data"].is_array()),
                _ => false,
            };
            let mut evidence_refs = vec![match health_status {
                Some(status) => format!("loopback:health:{status}"),
                None => "loopback:health:connect_failed".into(),
            }];
            evidence_refs.push(if protocol_ok {
                "loopback:openai-models:200".into()
            } else {
                "loopback:openai-models:failed".into()
            });
            Ok(EndpointProbeObservation {
                health_ok,
                health_status,
                protocol: protocol_ok.then(|| endpoint.protocol.clone()),
                protocol_profile_hash: protocol_ok.then(|| endpoint.protocol_profile_hash.clone()),
                capabilities: if protocol_ok {
                    endpoint.capabilities.clone()
                } else {
                    Vec::new()
                },
                evidence_refs,
                failure_reason: (!health_ok || !protocol_ok)
                    .then(|| "loopback_health_or_protocol_failed".into()),
            })
        }
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn windows_real_loopback_health_protocol_and_sqlite_registration_then_dead_endpoint() {
        use axum::{routing::get, Json, Router};
        use larm_db::inference_endpoints::SqlInferenceEndpoints;
        use sqlx::sqlite::SqlitePoolOptions;
        use tokio::sync::oneshot;

        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::query("PRAGMA foreign_keys=ON")
            .execute(&pool)
            .await
            .unwrap();
        for schema in [
            "CREATE TABLE runtime_instance(id TEXT PRIMARY KEY)",
            "CREATE TABLE inference_endpoint(id TEXT PRIMARY KEY,created_at TEXT NOT NULL,updated_at TEXT NOT NULL,row_version INTEGER NOT NULL,metadata TEXT NOT NULL,instance_id TEXT NOT NULL REFERENCES runtime_instance(id),protocol TEXT NOT NULL,protocol_profile_hash TEXT NOT NULL,base_url TEXT NOT NULL,credential_reference_id TEXT,capabilities TEXT NOT NULL,qualification TEXT NOT NULL,evidence_refs TEXT NOT NULL)",
            "CREATE TABLE event_sequence_alloc(id INTEGER PRIMARY KEY AUTOINCREMENT)",
            "CREATE TABLE event(id TEXT PRIMARY KEY,created_at TEXT NOT NULL,updated_at TEXT NOT NULL,row_version INTEGER NOT NULL,metadata TEXT NOT NULL,event_id INTEGER NOT NULL UNIQUE,event_type TEXT NOT NULL,actor_id TEXT NOT NULL,resource_type TEXT NOT NULL,resource_id TEXT NOT NULL,resource_version INTEGER NOT NULL,correlation_id TEXT NOT NULL,payload TEXT NOT NULL,occurred_at TEXT NOT NULL)",
            "CREATE TABLE event_outbox(event_id INTEGER PRIMARY KEY,event_record_id TEXT NOT NULL UNIQUE,delivered_at TEXT)",
        ] {
            sqlx::query(schema).execute(&pool).await.unwrap();
        }
        let instance_id = "0192aaaa-0000-7000-8000-000000000001";
        sqlx::query("INSERT INTO runtime_instance(id) VALUES(?)")
            .bind(instance_id)
            .execute(&pool)
            .await
            .unwrap();
        let repo = Arc::new(SqlInferenceEndpoints { pool });

        let app = Router::new()
            .route(
                "/health",
                get(|| async { axum::Json(serde_json::json!({"status":"ok"})) }),
            )
            .route(
                "/v1/models",
                get(|| async { Json(serde_json::json!({"data": [{"id":"fixture"}]})) }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        let server = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = shutdown_rx.await;
                })
                .await
                .unwrap();
        });

        let hash = "a".repeat(64);
        let mut protocols = ProtocolRegistry::default();
        protocols.register(profile("openai", &hash)).unwrap();
        let probe = Arc::new(WindowsLoopbackProbe {
            client: reqwest::Client::builder().no_proxy().build().unwrap(),
        });
        let service = EndpointRegistryService::new(
            repo.clone(),
            probe,
            Arc::new(protocols),
            Duration::from_secs(3),
        )
        .unwrap();
        let mut request = candidate(1, "openai", &hash);
        request.instance_id = instance_id.into();
        request.base_url = format!("http://{address}/v1");
        let endpoint = service.register(&request, &ctx()).await.unwrap();
        assert_eq!(
            endpoint.qualification,
            EndpointQualification::Qualified,
            "loopback probe did not qualify: metadata={:?}, evidence={:?}",
            endpoint.metadata,
            endpoint.evidence_refs
        );
        assert!(endpoint
            .evidence_refs
            .contains(&"loopback:openai-models:200".into()));
        assert_eq!(
            service
                .qualified_for_instance(instance_id)
                .await
                .unwrap()
                .len(),
            1
        );

        let _ = shutdown_tx.send(());
        server.await.unwrap();
        let dead = service.revalidate(&endpoint.id, &ctx()).await.unwrap();
        assert_eq!(dead.qualification, EndpointQualification::Failed);
        assert!(service
            .qualified_for_instance(instance_id)
            .await
            .unwrap()
            .is_empty());
        let events: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM event_outbox")
            .fetch_one(&repo.pool)
            .await
            .unwrap();
        assert_eq!(events, 2);
    }
}
