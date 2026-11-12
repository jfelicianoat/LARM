//! Política de ejecución de adaptadores (P022; contrato 03 §9 y §13).
//!
//! Antes de lanzar un worker:
//!
//! 1. LARM no puede estar elevado: un worker nunca hereda privilegios de
//!    administrador.
//! 2. El paquete en disco tiene que dar el `package_sha256` del manifiesto.
//! 3. Firma y confianza: `REVOKED` bloquea; `VERIFIED` (clave fijada por el
//!    operador) se ejecuta; `UNTRUSTED` (sin firma o `CUSTOM`) sólo con una
//!    concesión TRUSTED_ONLY ligada al hash del paquete, al publicador, a los
//!    permisos exactos y con caducidad.
//!
//! AppContainer y las ACL por SID de paquete no existen todavía (P130): toda
//! ejecución es TRUSTED_ONLY y la frontera es la comprobación del host en
//! cada operación ([`GuardedWorker`]), no el sistema operativo.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use larm_adapter_sdk::manifest::{AdapterManifest, Permissions};
use larm_adapter_sdk::permissions::{
    adapter_resource, elevation_digest, execution_digest, package_hash, Access, Assessment, Grants,
    PermissionDelta, RootMap, Trust, TrustStore,
};
use larm_domain::approval::{ApprovalRecord, ApprovalUse};
use larm_domain::error::{ErrorCode, LarmError};
use serde::Serialize;
use serde_json::Value;

use crate::worker::{Containment, HostConfig, Worker, WorkerSpec};

/// Nombre del manifiesto dentro del paquete (excluido del hash).
pub const MANIFEST_FILE: &str = "manifest.json";

fn forbidden(reason: &str, message: &str) -> LarmError {
    LarmError::new(ErrorCode::Forbidden, message, reason)
}

fn approval_required(reason: &str, message: &str, resource: &str) -> LarmError {
    LarmError::new(ErrorCode::ApprovalRequired, message, reason).with_resource(resource.to_string())
}

/// ¿Corre LARM elevado?
pub trait ElevationProbe: Send + Sync {
    /// # Errors
    /// No se pudo determinar (se trata como elevado).
    fn elevated(&self) -> Result<bool, LarmError>;
}

/// Nivel de integridad del token con `whoami /groups`: alto (`S-1-16-12288`)
/// o de sistema (`S-1-16-16384`) es elevado.
#[derive(Debug, Clone, Copy, Default)]
pub struct WhoAmI;

impl ElevationProbe for WhoAmI {
    fn elevated(&self) -> Result<bool, LarmError> {
        let root = std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into());
        let exe = Path::new(&root).join("System32").join("whoami.exe");
        let out = std::process::Command::new(exe)
            .args(["/groups", "/fo", "csv", "/nh"])
            .output()
            .map_err(|e| {
                forbidden(
                    "elevation_unknown",
                    "No se pudo comprobar si LARM está elevado",
                )
                .with_technical(e.to_string())
            })?;
        if !out.status.success() {
            return Err(forbidden(
                "elevation_unknown",
                "No se pudo comprobar si LARM está elevado",
            ));
        }
        let text = String::from_utf8_lossy(&out.stdout);
        if !text.contains("S-1-16-") {
            return Err(forbidden(
                "elevation_unknown",
                "whoami no informó del nivel de integridad",
            ));
        }
        Ok(text.contains("S-1-16-12288") || text.contains("S-1-16-16384"))
    }
}

/// Concesión TRUSTED_ONLY para ejecutar un adaptador sin firma verificada.
///
/// Sale de una aprobación (`POST /approvals`) con:
/// - `operation_digest` = [`execution_digest`] del manifiesto;
/// - `resource_ids` = [`adapter_resource`];
/// - `code_hash` = `package_sha256`;
/// - `permissions` = `{publisher, permissions, expires_at, secret_refs}`,
///   donde `permissions` es copia exacta de la del manifiesto y
///   `secret_refs` lo que se concede de verdad (por defecto, nada).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TrustedOnlyGrant {
    pub approval_id: String,
    pub actor_id: String,
    pub resource: String,
    pub execution_digest: String,
    pub package_sha256: String,
    pub publisher: String,
    pub secret_refs: Vec<String>,
    pub expires_at: String,
}

fn parse_time(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

impl TrustedOnlyGrant {
    /// Valida la aprobación contra el manifiesto. Quien llama consume
    /// después la aprobación (un solo uso) y guarda la concesión.
    ///
    /// # Errors
    /// `APPROVAL_REQUIRED` con el motivo si no cubre exactamente este
    /// paquete, publicador y permisos, o si no lleva caducidad futura.
    pub fn from_approval(
        record: &ApprovalRecord,
        manifest: &AdapterManifest,
        actor_id: &str,
        now: DateTime<Utc>,
    ) -> Result<Self, LarmError> {
        let resource = adapter_resource(manifest);
        let digest = execution_digest(manifest)?;
        record.check(
            &ApprovalUse {
                actor_id,
                operation_digest: &digest,
                resource_ids: std::slice::from_ref(&resource),
                code_hash: Some(&manifest.package_sha256),
            },
            now,
        )?;
        let p = &record.permissions;
        if p.get("publisher").and_then(Value::as_str) != Some(manifest.publisher.as_str()) {
            return Err(approval_required(
                "approval_publisher_mismatch",
                "La aprobación es para otro publicador",
                &resource,
            ));
        }
        let declared = serde_json::to_value(&manifest.permissions).unwrap_or(Value::Null);
        if p.get("permissions") != Some(&declared) {
            return Err(approval_required(
                "approval_permissions_mismatch",
                "La aprobación no cubre estos permisos",
                &resource,
            ));
        }
        let expires_at = p
            .get("expires_at")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if !parse_time(&expires_at).is_some_and(|t| t > now) {
            return Err(approval_required(
                "approval_expires_at_invalid",
                "La concesión necesita una caducidad futura",
                &resource,
            ));
        }
        let secret_refs: Vec<String> = match p.get("secret_refs") {
            None => vec![],
            Some(v) => serde_json::from_value(v.clone()).map_err(|_| {
                approval_required(
                    "approval_secret_refs_invalid",
                    "secret_refs de la aprobación mal formado",
                    &resource,
                )
            })?,
        };
        if secret_refs
            .iter()
            .any(|s| !manifest.permissions.secret_refs.contains(s))
        {
            return Err(approval_required(
                "approval_secret_not_declared",
                "Se concede un secreto que el manifiesto no declara",
                &resource,
            ));
        }
        Ok(Self {
            approval_id: record.id.clone(),
            actor_id: actor_id.into(),
            resource,
            execution_digest: digest,
            package_sha256: manifest.package_sha256.clone(),
            publisher: manifest.publisher.clone(),
            secret_refs,
            expires_at,
        })
    }
}

/// Modo de ejecución. Sólo existe TRUSTED_ONLY hasta P130 (AppContainer).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ExecutionMode {
    TrustedOnly,
}

/// Autorización para lanzar un worker concreto.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchAuthorization {
    pub assessment: Assessment,
    pub mode: ExecutionMode,
    pub grants: Grants,
    pub execution_digest: String,
    /// Concesión usada (sólo `UNTRUSTED`).
    pub approval_id: Option<String>,
}

/// Decisión al actualizar un adaptador ya aceptado.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum UpgradeDecision {
    /// Mismos permisos o menos: se acepta sin más.
    Accepted { approval_id: Option<String> },
    /// Pide más: hace falta una aprobación con este digest.
    ApprovalRequired {
        delta: PermissionDelta,
        operation_digest: String,
        resource_id: String,
    },
}

/// Política del host.
pub struct Policy {
    pub trust: TrustStore,
    pub roots: RootMap,
    pub elevation: Arc<dyn ElevationProbe>,
}

impl Policy {
    /// Decide si se puede lanzar el adaptador.
    ///
    /// # Errors
    /// - `FORBIDDEN`: `elevated_process`, `elevation_unknown`,
    ///   `package_hash_mismatch`, `revoked`, `signature_invalid`;
    /// - `APPROVAL_REQUIRED`: `untrusted_requires_approval` o la concesión no
    ///   corresponde (`grant_*`).
    pub fn authorize(
        &self,
        package_dir: &Path,
        manifest: &AdapterManifest,
        custom: bool,
        grant: Option<&TrustedOnlyGrant>,
        now: DateTime<Utc>,
    ) -> Result<LaunchAuthorization, LarmError> {
        if self.elevation.elevated()? {
            return Err(forbidden(
                "elevated_process",
                "LARM se está ejecutando como administrador: no lanza adaptadores con esos privilegios",
            ));
        }
        if package_hash(package_dir, &[MANIFEST_FILE])? != manifest.package_sha256 {
            return Err(forbidden(
                "package_hash_mismatch",
                "El paquete en disco no corresponde al manifiesto",
            ));
        }
        let assessment = self.trust.assess(manifest, custom)?;
        let digest = execution_digest(manifest)?;
        let mut grants = Grants::resolve(&manifest.permissions, &self.roots);
        let approval_id = match assessment.trust {
            Trust::Revoked => {
                return Err(forbidden(
                    "revoked",
                    "Adaptador revocado: no se permiten nuevas ejecuciones",
                ));
            }
            Trust::Verified => None,
            Trust::Untrusted => {
                let resource = adapter_resource(manifest);
                let Some(g) = grant else {
                    return Err(approval_required(
                        "untrusted_requires_approval",
                        "Adaptador sin firma verificada: necesita una aprobación TRUSTED_ONLY",
                        &resource,
                    ));
                };
                if g.resource != resource || g.execution_digest != digest {
                    return Err(approval_required(
                        "grant_mismatch",
                        "La concesión es de otro adaptador o permisos",
                        &resource,
                    ));
                }
                if g.package_sha256 != manifest.package_sha256 {
                    return Err(approval_required(
                        "grant_code_hash_mismatch",
                        "La concesión es de otro paquete",
                        &resource,
                    ));
                }
                if g.publisher != manifest.publisher {
                    return Err(approval_required(
                        "grant_publisher_mismatch",
                        "La concesión es de otro publicador",
                        &resource,
                    ));
                }
                if !parse_time(&g.expires_at).is_some_and(|t| t > now) {
                    return Err(approval_required(
                        "grant_expired",
                        "La concesión TRUSTED_ONLY ha caducado",
                        &resource,
                    ));
                }
                // UNTRUSTED no recibe secretos salvo concesión explícita.
                grants.secret_refs = g.secret_refs.iter().cloned().collect();
                Some(g.approval_id.clone())
            }
        };
        Ok(LaunchAuthorization {
            assessment,
            mode: ExecutionMode::TrustedOnly,
            grants,
            execution_digest: digest,
            approval_id,
        })
    }

    /// Revisa los permisos de una versión nueva frente a los aceptados.
    ///
    /// # Errors
    /// `APPROVAL_REQUIRED` si la aprobación presentada no cubre esta
    /// elevación.
    pub fn review_upgrade(
        accepted: &Permissions,
        manifest: &AdapterManifest,
        approval: Option<(&ApprovalRecord, &str)>,
        now: DateTime<Utc>,
    ) -> Result<UpgradeDecision, LarmError> {
        let delta = PermissionDelta::between(accepted, &manifest.permissions);
        if !delta.is_elevation() {
            return Ok(UpgradeDecision::Accepted { approval_id: None });
        }
        let digest = elevation_digest(manifest, &delta)?;
        let resource = adapter_resource(manifest);
        let Some((record, actor_id)) = approval else {
            return Ok(UpgradeDecision::ApprovalRequired {
                delta,
                operation_digest: digest,
                resource_id: resource,
            });
        };
        record.check(
            &ApprovalUse {
                actor_id,
                operation_digest: &digest,
                resource_ids: std::slice::from_ref(&resource),
                code_hash: Some(&manifest.package_sha256),
            },
            now,
        )?;
        Ok(UpgradeDecision::Accepted {
            approval_id: Some(record.id.clone()),
        })
    }
}

/// Worker con sus permisos: cada operación sensible se comprueba en el host
/// antes de enviarla.
pub struct GuardedWorker {
    worker: Arc<Worker>,
    grants: Grants,
}

impl GuardedWorker {
    /// Lanza el worker autorizado.
    ///
    /// # Errors
    /// `FORBIDDEN` `authorization_mismatch` si el manifiesto no es el
    /// autorizado; los de [`Worker::start`].
    pub async fn start(
        spec: WorkerSpec,
        authorization: &LaunchAuthorization,
        config: HostConfig,
        generation: u64,
        containment: Arc<dyn Containment>,
    ) -> Result<Self, LarmError> {
        Self::start_owned(spec, authorization, config, generation, containment, None).await
    }

    pub(crate) async fn start_owned(
        spec: WorkerSpec,
        authorization: &LaunchAuthorization,
        config: HostConfig,
        generation: u64,
        containment: Arc<dyn Containment>,
        owner: Option<Arc<dyn Send + Sync>>,
    ) -> Result<Self, LarmError> {
        if execution_digest(&spec.manifest)? != authorization.execution_digest {
            return Err(forbidden(
                "authorization_mismatch",
                "El manifiesto no es el autorizado",
            ));
        }
        let worker = Worker::start_owned(spec, config, generation, containment, owner).await?;
        Ok(Self {
            worker,
            grants: authorization.grants.clone(),
        })
    }

    /// Llama al worker declarando lo que la operación toca.
    ///
    /// # Errors
    /// `FORBIDDEN` si `access` excede los permisos (sin enviar nada); los de
    /// [`Worker::call`].
    pub async fn call(
        &self,
        method: &str,
        params: Value,
        access: &Access,
        timeout: Option<Duration>,
        resource_id: Option<&str>,
    ) -> Result<Value, LarmError> {
        self.grants.check(access)?;
        self.worker.call(method, params, timeout, resource_id).await
    }

    /// Como call, conservando un id del host para cancelación explícita.
    pub async fn call_request(
        &self,
        request: larm_adapter_sdk::protocol::Request,
        access: &Access,
        timeout: Option<Duration>,
        resource_id: Option<&str>,
    ) -> Result<Value, LarmError> {
        self.grants.check(access)?;
        self.worker
            .call_request(request, timeout, resource_id)
            .await
    }

    pub async fn cancel(&self, request_id: &str) -> Result<bool, LarmError> {
        self.worker.cancel(request_id).await
    }

    pub fn manifest(&self) -> &AdapterManifest {
        &self.worker.manifest
    }

    /// Estado del proceso. No se expone el [`Worker`] para que nadie llame
    /// sin pasar por la comprobación.
    #[must_use]
    pub fn state(&self) -> crate::worker::WorkerState {
        self.worker.state()
    }

    pub async fn stop(&self) {
        self.worker.stop().await;
    }

    pub async fn stop_and_wait(&self) -> Result<(), LarmError> {
        self.worker.stop_and_wait().await
    }

    pub(crate) fn terminate(&self) {
        self.worker.terminate();
    }

    #[must_use]
    pub fn io_drained(&self) -> bool {
        self.worker.io_drained()
    }

    #[must_use]
    pub fn pid(&self) -> u32 {
        self.worker.pid
    }

    pub fn subscribe_progress(
        &self,
    ) -> tokio::sync::broadcast::Receiver<larm_adapter_sdk::protocol::Progress> {
        self.worker.subscribe_progress()
    }

    #[must_use]
    pub fn grants(&self) -> &Grants {
        &self.grants
    }
}
