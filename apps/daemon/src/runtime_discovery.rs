//! Entrada local para descubrir versiones con la sesión cualificada P021/P022/P026.
use async_trait::async_trait;
use larm_adapter_host::runtime::discovery::{
    DiscoveryAccess, DiscoveryAccessResolver, NativeDiscoveryHost,
};
use larm_adapter_sdk::permissions::Access;
use larm_domain::{
    error::{ErrorCode, LarmError},
    ports::MutationContext,
    runtime_execution::RuntimeExecutionContext,
};
use larm_runtime::discovery_service::{DiscoveryResult, DiscoveryService};
use std::{num::NonZeroU32, sync::Arc, time::Duration};

const PAGE_BUDGET: u32 = 100;
const RPC_TIMEOUT: Duration = Duration::from_secs(30);

fn invalid(reason: &str) -> LarmError {
    LarmError::new(
        ErrorCode::ValidationFailed,
        "No se pudo preparar el descubrimiento del runtime",
        reason,
    )
}

/// La operación sólo puede usar los orígenes de red del manifiesto que el
/// perfil P022 ya aprobó. No concede rutas, procesos ni secretos adicionales.
pub struct ManifestDiscoveryAccess;

#[async_trait]
impl DiscoveryAccessResolver for ManifestDiscoveryAccess {
    async fn resolve(
        &self,
        context: &RuntimeExecutionContext,
        ctx: &MutationContext,
    ) -> Result<DiscoveryAccess, LarmError> {
        if ctx.actor_id != context.actor_id {
            return Err(LarmError::new(
                ErrorCode::Forbidden,
                "El actor no coincide con el contexto cualificado",
                "runtime_discovery_actor_mismatch",
            ));
        }
        let encoded = serde_json::to_string(&context.installation.manifest)
            .map_err(|_| invalid("runtime_discovery_manifest_invalid"))?;
        let manifest = larm_adapter_sdk::manifest::AdapterManifest::parse(&encoded)
            .map_err(|_| invalid("runtime_discovery_manifest_invalid"))?;
        let mut urls = manifest.permissions.network_origins;
        urls.sort();
        urls.dedup();
        Ok(DiscoveryAccess {
            access: Access {
                urls,
                ..Access::default()
            },
            timeout: RPC_TIMEOUT,
        })
    }
}

/// Ejecuta una sola sesión local, drena el runner y devuelve únicamente el
/// resultado persistido. El perfil P022 determina qué actor puede consumirlo.
pub async fn discover(
    db: &larm_db::pool::Db,
    runtime_id: &str,
    actor_id: &str,
) -> Result<DiscoveryResult, LarmError> {
    if actor_id.is_empty() || actor_id.len() > 256 || actor_id.chars().any(char::is_control) {
        return Err(invalid("runtime_execution_actor_invalid"));
    }
    let page_budget = NonZeroU32::new(PAGE_BUDGET)
        .ok_or_else(|| invalid("runtime_discovery_page_budget_invalid"))?;
    let repo = Arc::new(larm_db::runtimes::SqlRuntimes {
        pool: db.pool.clone(),
    });
    let runner = crate::probes::runner(db);
    let host = Arc::new(NativeDiscoveryHost {
        runner: runner.clone(),
        access: Arc::new(ManifestDiscoveryAccess),
    });
    let service = DiscoveryService::new(repo, host, page_budget);
    let context = MutationContext {
        actor_id: actor_id.into(),
        correlation_id: uuid::Uuid::now_v7().to_string(),
    };
    let result = service.discover(runtime_id, &context).await;
    let drained = runner.shutdown().await;
    match result {
        Err(error) => Err(error),
        Ok(value) => {
            drained?;
            Ok(value)
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;
    use larm_domain::{
        adapters::{AdapterInstallation, AdapterState, AdapterTrust},
        conformance::{BindingHashes, Run},
        runtime::{Runtime, RuntimeAdapter, RuntimeChannelPolicy},
    };
    use serde_json::json;

    fn context(actor_id: &str, permissions: serde_json::Value) -> RuntimeExecutionContext {
        let methods = larm_adapter_sdk::manifest::MethodTable::contract()
            .methods
            .into_iter()
            .filter(|method| method.family == "all" || method.family == "runtime")
            .map(|method| method.method)
            .collect::<Vec<_>>();
        let installation = AdapterInstallation {
            id: "0192aaaa-0000-7000-8000-000000000001".into(),
            created_at: "2026-10-20T00:00:00.000Z".into(),
            updated_at: "2026-10-20T00:00:00.000Z".into(),
            row_version: 1,
            metadata: Default::default(),
            adapter_id: "fixture.runtime".into(),
            version: "1".into(),
            family: "runtime".into(),
            package_hash: "a".repeat(64),
            publisher: "fixture".into(),
            trust: AdapterTrust::Untrusted,
            state: AdapterState::Installed,
            manifest: json!({
                "id":"fixture.runtime", "version":"1", "family":"runtime",
                "protocol_version":"1.0", "publisher":"fixture",
                "executable":{"relative_path":"worker.exe","args":[]},
                "platforms":["windows"], "architectures":["x86_64"],
                "methods":methods, "installation_strategies":[],
                "permissions":permissions, "capabilities":[], "package_sha256":"a".repeat(64),
                "signature":null
            }),
        };
        RuntimeExecutionContext {
            operation_id: "0192aaaa-0000-7000-8000-000000000002".into(),
            runtime: Runtime {
                id: "0192aaaa-0000-7000-8000-000000000003".into(),
                created_at: "2026-10-20T00:00:00.000Z".into(),
                updated_at: "2026-10-20T00:00:00.000Z".into(),
                row_version: 1,
                metadata: Default::default(),
                name: "fixture".into(),
                adapter_id: "0192aaaa-0000-7000-8000-000000000004".into(),
                capabilities: vec![],
                default_version_id: None,
                channel_policy: RuntimeChannelPolicy::default(),
                retention_policy: Default::default(),
            },
            adapter: RuntimeAdapter {
                id: "0192aaaa-0000-7000-8000-000000000004".into(),
                created_at: "2026-10-20T00:00:00.000Z".into(),
                updated_at: "2026-10-20T00:00:00.000Z".into(),
                row_version: 1,
                metadata: Default::default(),
                adapter_installation_id: installation.id.clone(),
                runtime_family: "fixture".into(),
                capabilities: vec![],
            },
            installation: installation.clone(),
            run: Run {
                id: "0192aaaa-0000-7000-8000-000000000005".into(),
                actor_id: actor_id.into(),
                installation_id: installation.id.clone(),
                installation_revision: "1".into(),
                installation_sha256: "b".repeat(64),
                manifest_sha256: "c".repeat(64),
                permissions_sha256: "d".repeat(64),
                package_sha256: "a".repeat(64),
                family: "runtime".into(),
                capabilities: vec![],
                bindings: BindingHashes {
                    suite_sha256: "e".repeat(64),
                    capability_profile_sha256: "f".repeat(64),
                    execution_context_sha256: "0".repeat(64),
                },
                started_at: "2026-10-20T00:00:00.000Z".into(),
            },
            actor_id: actor_id.into(),
        }
    }

    #[tokio::test]
    async fn access_usa_solo_origenes_aprobados_y_no_concede_otros_recursos() {
        let context = context(
            "operator",
            json!({
                "read_roots":["$package"], "write_roots":["$work"],
                "network_origins":["https://example.test", "https://example.test"],
                "process_allowlist":["python.exe"], "secret_refs":["larm/test"]
            }),
        );
        let ctx = MutationContext {
            actor_id: "operator".into(),
            correlation_id: uuid::Uuid::now_v7().to_string(),
        };
        let access = ManifestDiscoveryAccess
            .resolve(&context, &ctx)
            .await
            .unwrap();
        assert_eq!(access.access.urls, ["https://example.test"]);
        assert!(access.access.reads.is_empty());
        assert!(access.access.writes.is_empty());
        assert!(access.access.processes.is_empty());
        assert!(access.access.secrets.is_empty());
        assert_eq!(access.timeout, RPC_TIMEOUT);
    }

    #[tokio::test]
    async fn access_rechaza_otro_actor_y_manifiesto_invalido() {
        let current = context("operator", json!({"network_origins":[]}));
        let other = MutationContext {
            actor_id: "other".into(),
            correlation_id: uuid::Uuid::now_v7().to_string(),
        };
        match ManifestDiscoveryAccess.resolve(&current, &other).await {
            Err(error) => assert_eq!(error.details.reason, "runtime_discovery_actor_mismatch"),
            Ok(_) => panic!("se aceptó otro actor"),
        }
        let broken = context("operator", json!({"unexpected":true}));
        let actor = MutationContext {
            actor_id: "operator".into(),
            correlation_id: uuid::Uuid::now_v7().to_string(),
        };
        match ManifestDiscoveryAccess.resolve(&broken, &actor).await {
            Err(error) => assert_eq!(error.details.reason, "runtime_discovery_manifest_invalid"),
            Ok(_) => panic!("se aceptó un manifiesto incompleto"),
        }
    }
}
