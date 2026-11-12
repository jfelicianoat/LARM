//! Análisis, limpieza y borrado de modelos (P018; spec §38, §43; contratos
//! 02, 03 y 07).
//!
//! - **Análisis**: `GET /storage-locations/{id}/analysis` (solo lectura).
//! - **Limpieza**: el plan (`cleanup-plan`) lista lo recuperable y siempre
//!   exige aprobación ligada a su hash; la ejecución (`cleanup`) es un job
//!   que vuelve a comprobar cada elemento antes de borrarlo (sigue sin
//!   registrar, mismo tamaño, su job sigue terminado) y salta lo que cambió.
//! - **Borrar un modelo**: el plan (`delete-plan`) decide por fichero:
//!   `DELETE_FILE` solo si es `LARM_MANAGED` y nadie más lo usa;
//!   `DEREGISTER` para `EXTERNAL`/`HOST_MANAGED` (el fichero se queda);
//!   `KEEP_SHARED` para blobs compartidos. `DELETE /models/{id}` exige
//!   `If-Match`, el `plan_hash` y la aprobación; toma los locks exclusivos
//!   (una lease activa es `ACTIVE_LEASE`), borra el registro en una
//!   transacción y solo después los ficheros.

use std::collections::BTreeSet;
use std::sync::Arc;

use async_trait::async_trait;
use larm_domain::artifacts::{join, resolve_root, Ownership, Plan, PlanStep};
use larm_domain::auth::{ApiScope, Principal};
use larm_domain::bridge::{timestamp, JobPhase, JobReceipt, JobState};
use larm_domain::error::{ErrorCode, LarmError};
use larm_domain::estimate::{Confidence, StorageEstimate};
use larm_domain::guard::{JournalStep, LockMode, Resource, ResourceKind};
use larm_domain::locations::StorageLocation;
use larm_domain::ports::{
    ArtifactLocationRepository, JobSource, LiveJobs, MoveJournal, MutationContext, ResourceGuard,
    StorageLocationRepository, StorageProber, VariantRepository,
};
use larm_domain::units::DecimalU64;
use serde::{Deserialize, Serialize};

use crate::analyzer::{analyze, list_root, Reclaimable, StorageAnalysis};

pub const CLEANUP_KIND: &str = "storage.cleanup";
pub const DELETE_KIND: &str = "model.delete";

/// Puertos del servicio.
#[derive(Clone)]
pub struct StoragePorts {
    pub locations: Arc<dyn StorageLocationRepository>,
    pub artifacts: Arc<dyn ArtifactLocationRepository>,
    pub variants: Arc<dyn VariantRepository>,
    pub prober: Arc<dyn StorageProber>,
    pub live: Arc<dyn LiveJobs>,
    pub guard: Arc<dyn ResourceGuard>,
    pub cleanup_journal: Arc<dyn MoveJournal>,
    pub delete_journal: Arc<dyn MoveJournal>,
}

pub struct StorageService {
    pub ports: StoragePorts,
    pub plan_ttl: chrono::Duration,
}

fn not_found(what: &str, id: &str) -> LarmError {
    LarmError::new(
        ErrorCode::NotFound,
        format!("{what} desconocido"),
        "not_found",
    )
    .with_resource(id.to_string())
}

fn io(what: &str, e: &std::io::Error) -> LarmError {
    LarmError::new(
        ErrorCode::ResourceUnavailable,
        "No se pudo acceder al almacenamiento",
        "io_failed",
    )
    .with_technical(format!("{what}: {e}"))
    .retryable(true)
}

fn internal(reason: &str, detail: impl Into<String>) -> LarmError {
    LarmError::new(ErrorCode::Internal, "Error interno", reason).with_technical(detail.into())
}

fn empty_estimate(reason: &str) -> StorageEstimate {
    StorageEstimate {
        download_bytes: None,
        final_artifact_bytes: None,
        installed_bytes: None,
        derived_cache_bytes: None,
        temporary_peak_bytes: None,
        existing_bytes: None,
        peak_extra_bytes: Some(DecimalU64::default()),
        required_free_bytes: Some(DecimalU64::default()),
        upper_bound_bytes: Some(DecimalU64::default()),
        confidence: Confidence::Exact,
        reason: reason.into(),
        evidence_refs: vec![],
        per_volume: vec![],
    }
}

/// Borra un fichero aunque sea de solo lectura (blobs del CAS). Ausente = ok.
fn remove(path: &str) -> std::io::Result<()> {
    match std::fs::metadata(path) {
        Ok(meta) => {
            let mut perms = meta.permissions();
            if perms.readonly() {
                #[allow(clippy::permissions_set_readonly_false)]
                perms.set_readonly(false);
                std::fs::set_permissions(path, perms)?;
            }
            std::fs::remove_file(path)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// Payload del plan de limpieza.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct CleanupPayload {
    actor_id: String,
    resource_id: String,
    expires_at: String,
    items: Vec<Reclaimable>,
    plan: Option<Plan>,
    #[serde(default)]
    result: Option<serde_json::Value>,
    #[serde(default)]
    error: Option<LarmError>,
}

/// Decisión del plan de borrado para una fila.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeleteItem {
    pub artifact_location_id: String,
    pub artifact_id: String,
    pub row_version: i64,
    pub storage_location_id: String,
    pub relative_path: String,
    /// `DELETE_FILE`, `DEREGISTER` o `KEEP_SHARED`.
    pub action: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct DeletePayload {
    actor_id: String,
    resource_id: String,
    variant_row_version: i64,
    expires_at: String,
    items: Vec<DeleteItem>,
    plan: Option<Plan>,
}

impl StorageService {
    async fn location(&self, id: &str) -> Result<StorageLocation, LarmError> {
        self.ports
            .locations
            .get(id)
            .await?
            .ok_or_else(|| not_found("Ubicación", id))
    }

    async fn root(&self, location: &StorageLocation) -> Result<String, LarmError> {
        let unc: Vec<String> = location.unc_path.iter().cloned().collect();
        let observation = self.ports.prober.probe(&unc).await?;
        resolve_root(location, &observation.volumes).map_err(|_| {
            LarmError::new(
                ErrorCode::ResourceUnavailable,
                "El volumen de la ubicación no está conectado",
                "volume_offline",
            )
            .with_resource(location.id.clone())
            .retryable(true)
        })
    }

    /// Análisis y elementos recuperables de una ubicación.
    ///
    /// # Errors
    /// Ubicación desconocida, volumen desconectado o E/S.
    pub async fn analysis(
        &self,
        id: &str,
    ) -> Result<(StorageAnalysis, Vec<Reclaimable>), LarmError> {
        let location = self.location(id).await?;
        let root = self.root(&location).await?;
        let rows = self.ports.artifacts.by_location(id).await?;
        let live = self.ports.live.live_jobs().await?;
        let files = tokio::task::spawn_blocking(move || list_root(std::path::Path::new(&root)))
            .await
            .map_err(|e| internal("join", e.to_string()))?
            .map_err(|e| io("recorriendo la raíz", &e))?;
        let (usage, items) = analyze(&files, &rows, &live);
        Ok((
            StorageAnalysis {
                storage_location_id: location.id.clone(),
                volume_key: location.volume_key.clone(),
                free_bytes: Some(location.free_bytes),
                reserved_bytes: location.reserved_bytes,
                categories: usage.into_values().collect(),
                snapshot_at: timestamp(chrono::Utc::now()),
            },
            items,
        ))
    }

    fn hash(value: &serde_json::Value) -> Result<String, LarmError> {
        larm_domain::canonical::sha256_hex(value).map_err(|e| internal("canonical", e))
    }

    /// Plan de limpieza: todo lo recuperable, con aprobación obligatoria.
    ///
    /// # Errors
    /// Los del análisis; `INVALID_STATE_TRANSITION` si no hay nada que limpiar.
    pub async fn cleanup_plan(&self, id: &str, ctx: &MutationContext) -> Result<Plan, LarmError> {
        let (_, items) = self.analysis(id).await?;
        if items.is_empty() {
            return Err(LarmError::new(
                ErrorCode::InvalidStateTransition,
                "No hay nada recuperable en esta ubicación",
                "nothing_to_clean",
            ));
        }
        let plan_hash = Self::hash(&serde_json::json!({
            "operation": "storage.cleanup", "storage_location_id": id, "items": items,
        }))?;
        let plan_id = uuid::Uuid::now_v7().to_string();
        let expires_at = timestamp(chrono::Utc::now() + self.plan_ttl);
        let freed: u64 = items.iter().map(|i| i.size_bytes).sum();
        let plan = Plan {
            plan_id: plan_id.clone(),
            plan_hash: plan_hash.clone(),
            operation: "storage.cleanup".into(),
            resource_ids: vec![format!("storage_location:{id}")],
            storage_estimate: empty_estimate(&format!("libera {freed} bytes")),
            steps: items
                .iter()
                .map(|i| {
                    let mut request = serde_json::Map::new();
                    request.insert(
                        "category".into(),
                        serde_json::to_value(i.category).unwrap_or_default(),
                    );
                    request.insert("relative_path".into(), i.relative_path.clone().into());
                    request.insert("size_bytes".into(), i.size_bytes.to_string().into());
                    request.insert("reason".into(), i.reason.clone().into());
                    PlanStep {
                        phase: "DELETE".into(),
                        adapter_id: None,
                        request,
                    }
                })
                .collect(),
            expires_at: expires_at.clone(),
            approval_required: true,
        };
        let payload = CleanupPayload {
            actor_id: ctx.actor_id.clone(),
            resource_id: id.into(),
            expires_at,
            items,
            plan: Some(plan.clone()),
            ..CleanupPayload::default()
        };
        self.ports
            .cleanup_journal
            .write(&step(
                &plan_id,
                CLEANUP_KIND,
                "PLANNED",
                &plan_hash,
                &plan.resource_ids,
                &payload,
                &[],
            ))
            .await?;
        Ok(plan)
    }

    /// Plan guardado por id y hash (para validar la aprobación antes).
    ///
    /// # Errors
    /// `STALE_REVISION` si no existe, ya se usó, caducó o es de otra ubicación.
    pub async fn planned_cleanup(
        &self,
        id: &str,
        plan_id: &str,
        plan_hash: &str,
    ) -> Result<Plan, LarmError> {
        let stale = |reason: &str| {
            LarmError::new(
                ErrorCode::StaleRevision,
                "Plan desconocido, usado o caducado: vuelve a planificar",
                reason,
            )
        };
        let record = self
            .ports
            .cleanup_journal
            .get(plan_id)
            .await?
            .filter(|r| r.phase == "PLANNED" && r.plan_hash == plan_hash)
            .ok_or_else(|| stale("plan_unknown"))?;
        let payload: CleanupPayload = serde_json::from_value(record.payload)
            .map_err(|e| internal("payload", e.to_string()))?;
        if payload.resource_id != id {
            return Err(stale("plan_mismatch"));
        }
        if chrono::DateTime::parse_from_rfc3339(&payload.expires_at).map_or(true, |e| {
            e.with_timezone(&chrono::Utc) <= chrono::Utc::now()
        }) {
            return Err(stale("plan_expired"));
        }
        payload
            .plan
            .ok_or_else(|| internal("payload", "plan ausente"))
    }

    /// Ejecuta un plan de limpieza ya aprobado: job asíncrono.
    ///
    /// # Errors
    /// Plan desconocido o usado.
    pub async fn cleanup(
        self: &Arc<Self>,
        id: &str,
        plan_id: &str,
        plan_hash: &str,
        ctx: &MutationContext,
    ) -> Result<(JobReceipt, larm_domain::ports::BackgroundTask), LarmError> {
        let plan = self.planned_cleanup(id, plan_id, plan_hash).await?;
        let record = self
            .ports
            .cleanup_journal
            .get(plan_id)
            .await?
            .ok_or_else(|| internal("journal", "plan desaparecido"))?;
        let payload: CleanupPayload = serde_json::from_value(record.payload)
            .map_err(|e| internal("payload", e.to_string()))?;
        // Un solo uso: PLANNED → RUNNING.
        self.ports
            .cleanup_journal
            .write(&step(
                plan_id,
                CLEANUP_KIND,
                "RUNNING",
                plan_hash,
                &plan.resource_ids,
                &payload,
                &["PLANNED"],
            ))
            .await?;
        let service = Arc::clone(self);
        let (id, plan_id, plan_hash, ctx) = (
            id.to_string(),
            plan_id.to_string(),
            plan_hash.to_string(),
            ctx.clone(),
        );
        let receipt = JobReceipt {
            job_id: plan_id.clone(),
            status_url: format!("/api/v1/jobs/{plan_id}"),
            state: JobPhase::Running,
        };
        Ok((
            receipt,
            Box::pin(async move {
                let result = service.run_cleanup(&id, &payload).await;
                let mut done = payload.clone();
                let phase = match result {
                    Ok(r) => {
                        done.result = Some(r);
                        "COMPLETED"
                    }
                    Err(e) => {
                        done.error = Some(e);
                        "FAILED"
                    }
                };
                let _ = service
                    .ports
                    .cleanup_journal
                    .write(&step(
                        &plan_id,
                        CLEANUP_KIND,
                        phase,
                        &plan_hash,
                        &plan.resource_ids,
                        &done,
                        &["RUNNING"],
                    ))
                    .await;
                let _ = ctx;
            }),
        ))
    }

    async fn run_cleanup(
        &self,
        id: &str,
        payload: &CleanupPayload,
    ) -> Result<serde_json::Value, LarmError> {
        let location = self.location(id).await?;
        let root = self.root(&location).await?;
        // Se recalcula: solo se borra lo que sigue siendo recuperable igual.
        let (_, now_items) = self.analysis(id).await?;
        let current: BTreeSet<(String, u64)> = now_items
            .iter()
            .map(|i| (i.relative_path.clone(), i.size_bytes))
            .collect();
        let (mut deleted, mut skipped, mut freed) = (Vec::new(), Vec::new(), 0u64);
        for item in &payload.items {
            if !current.contains(&(item.relative_path.clone(), item.size_bytes)) {
                skipped.push(
                    serde_json::json!({"relative_path": item.relative_path, "reason": "changed"}),
                );
                continue;
            }
            // Justo antes de borrar: nadie lo ha registrado mientras tanto.
            if !self
                .ports
                .artifacts
                .at_path(id, &item.relative_path)
                .await?
                .is_empty()
            {
                skipped.push(serde_json::json!({"relative_path": item.relative_path, "reason": "registered"}));
                continue;
            }
            let path = join(&root, &item.relative_path);
            match tokio::task::spawn_blocking(move || remove(&path))
                .await
                .map_err(|e| internal("join", e.to_string()))?
            {
                Ok(()) => {
                    freed += item.size_bytes;
                    deleted.push(item.relative_path.clone());
                }
                Err(e) => skipped.push(serde_json::json!({"relative_path": item.relative_path, "reason": e.kind().to_string()})),
            }
        }
        Ok(
            serde_json::json!({"deleted": deleted, "skipped": skipped, "freed_bytes": freed.to_string()}),
        )
    }

    async fn delete_items(&self, variant_id: &str) -> Result<(i64, Vec<DeleteItem>), LarmError> {
        let variant = self
            .ports
            .variants
            .get_variant(variant_id)
            .await?
            .ok_or_else(|| not_found("Variante", variant_id))?;
        let rows = self
            .ports
            .artifacts
            .for_variant(variant_id)
            .await?
            .unwrap_or_default();
        let mut items = Vec::new();
        for r in &rows {
            let l = &r.location;
            let others = self
                .ports
                .artifacts
                .at_path(&l.storage_location_id, &l.relative_path)
                .await?
                .into_iter()
                .any(|o| o.artifact_id != l.artifact_id);
            let action = match l.ownership {
                Ownership::External | Ownership::HostManaged => "DEREGISTER",
                Ownership::Shared => "KEEP_SHARED",
                Ownership::LarmManaged if r.referenced_by > 1 || others => "KEEP_SHARED",
                Ownership::LarmManaged => "DELETE_FILE",
            };
            items.push(DeleteItem {
                artifact_location_id: l.id.clone(),
                artifact_id: l.artifact_id.clone(),
                row_version: l.row_version,
                storage_location_id: l.storage_location_id.clone(),
                relative_path: l.relative_path.clone(),
                action: action.into(),
            });
        }
        Ok((variant.row_version, items))
    }

    /// Plan de borrado de una variante (aprobación obligatoria).
    ///
    /// # Errors
    /// Variante desconocida o fallo de la base.
    pub async fn delete_plan(
        &self,
        variant_id: &str,
        ctx: &MutationContext,
    ) -> Result<Plan, LarmError> {
        let (version, items) = self.delete_items(variant_id).await?;
        let plan_hash = Self::hash(&serde_json::json!({
            "operation": "model.delete", "variant_id": variant_id, "row_version": version, "items": items,
        }))?;
        let plan_id = uuid::Uuid::now_v7().to_string();
        let expires_at = timestamp(chrono::Utc::now() + self.plan_ttl);
        let mut resource_ids = vec![format!("variant:{variant_id}")];
        resource_ids.extend(items.iter().map(|i| format!("artifact:{}", i.artifact_id)));
        resource_ids.sort();
        resource_ids.dedup();
        let plan = Plan {
            plan_id: plan_id.clone(),
            plan_hash: plan_hash.clone(),
            operation: "model.delete".into(),
            resource_ids: resource_ids.clone(),
            storage_estimate: empty_estimate("borrar no ocupa espacio nuevo"),
            steps: items
                .iter()
                .map(|i| {
                    let mut request = serde_json::Map::new();
                    request.insert(
                        "artifact_location_id".into(),
                        i.artifact_location_id.clone().into(),
                    );
                    request.insert(
                        "storage_location_id".into(),
                        i.storage_location_id.clone().into(),
                    );
                    request.insert("relative_path".into(), i.relative_path.clone().into());
                    PlanStep {
                        phase: i.action.clone(),
                        adapter_id: None,
                        request,
                    }
                })
                .collect(),
            expires_at: expires_at.clone(),
            approval_required: true,
        };
        let payload = DeletePayload {
            actor_id: ctx.actor_id.clone(),
            resource_id: variant_id.into(),
            variant_row_version: version,
            expires_at,
            items,
            plan: Some(plan.clone()),
        };
        self.ports
            .delete_journal
            .write(&step(
                &plan_id,
                DELETE_KIND,
                "PLANNED",
                &plan_hash,
                &resource_ids,
                &payload,
                &[],
            ))
            .await?;
        Ok(plan)
    }

    /// Plan de borrado vigente con ese hash para esa variante.
    ///
    /// # Errors
    /// `STALE_REVISION` si no existe, caducó o es de otra variante.
    pub async fn planned_delete(
        &self,
        variant_id: &str,
        plan_hash: &str,
    ) -> Result<Plan, LarmError> {
        let stale = |reason: &str| {
            LarmError::new(
                ErrorCode::StaleRevision,
                "Plan desconocido, usado o caducado: vuelve a planificar",
                reason,
            )
        };
        let record = self
            .ports
            .delete_journal
            .find_planned(plan_hash)
            .await?
            .ok_or_else(|| stale("plan_unknown"))?;
        let payload: DeletePayload = serde_json::from_value(record.payload)
            .map_err(|e| internal("payload", e.to_string()))?;
        if payload.resource_id != variant_id {
            return Err(stale("plan_mismatch"));
        }
        if chrono::DateTime::parse_from_rfc3339(&payload.expires_at).map_or(true, |e| {
            e.with_timezone(&chrono::Utc) <= chrono::Utc::now()
        }) {
            return Err(stale("plan_expired"));
        }
        payload
            .plan
            .ok_or_else(|| internal("payload", "plan ausente"))
    }

    /// Borra la variante según su plan aprobado (la aprobación la consume la API).
    ///
    /// # Errors
    /// `STALE_REVISION` (If-Match o plan obsoleto), `ACTIVE_LEASE`,
    /// `CONFLICT` (`variant_in_use`, locks) o E/S.
    pub async fn delete(
        &self,
        variant_id: &str,
        if_match: i64,
        plan_hash: &str,
        ctx: &MutationContext,
    ) -> Result<serde_json::Value, LarmError> {
        let plan = self.planned_delete(variant_id, plan_hash).await?;
        let (version, items) = self.delete_items(variant_id).await?;
        if version != if_match {
            return Err(LarmError::new(
                ErrorCode::StaleRevision,
                "La variante cambió desde que la leíste",
                "stale_revision",
            )
            .with_resource(variant_id.to_string()));
        }
        let now_hash = Self::hash(&serde_json::json!({
            "operation": "model.delete", "variant_id": variant_id, "row_version": version, "items": items,
        }))?;
        if now_hash != plan_hash {
            return Err(LarmError::new(
                ErrorCode::StaleRevision,
                "El modelo cambió desde el plan",
                "plan_changed",
            ));
        }
        let mut locks = vec![(
            Resource::new(ResourceKind::Variant, variant_id),
            LockMode::Exclusive,
        )];
        locks.extend(items.iter().map(|i| {
            (
                Resource::new(ResourceKind::Artifact, i.artifact_id.clone()),
                LockMode::Exclusive,
            )
        }));
        let payload = DeletePayload {
            actor_id: ctx.actor_id.clone(),
            resource_id: variant_id.into(),
            variant_row_version: version,
            expires_at: String::new(),
            items: items.clone(),
            plan: Some(plan.clone()),
        };
        // Locks + lease bajo el mismo lock; el plan solo se usa una vez.
        self.ports
            .guard
            .acquire(
                &plan.plan_id,
                1,
                &locks,
                Some(&step(
                    &plan.plan_id,
                    DELETE_KIND,
                    "LOCKED",
                    plan_hash,
                    &plan.resource_ids,
                    &payload,
                    &["PLANNED"],
                )),
            )
            .await?;
        let finish = |phase: &'static str| {
            step(
                &plan.plan_id,
                DELETE_KIND,
                phase,
                plan_hash,
                &plan.resource_ids,
                &payload,
                &["LOCKED"],
            )
        };
        // Primero el registro (una transacción); después los ficheros.
        if let Err(e) = self
            .ports
            .variants
            .delete_variant(variant_id, version, ctx)
            .await
        {
            let _ = self
                .ports
                .guard
                .release(&plan.plan_id, 1, Some(&finish("FAILED")))
                .await;
            return Err(e);
        }
        let mut deleted = Vec::new();
        let mut left = Vec::new();
        for item in items.iter().filter(|i| i.action == "DELETE_FILE") {
            let path = match self.location(&item.storage_location_id).await {
                Ok(l) => self.root(&l).await.map(|r| join(&r, &item.relative_path)),
                Err(e) => Err(e),
            };
            let outcome = match path {
                Ok(p) => tokio::task::spawn_blocking(move || remove(&p))
                    .await
                    .map_err(|e| internal("join", e.to_string()))?
                    .map_err(|e| e.kind().to_string()),
                Err(e) => Err(e.details.reason),
            };
            match outcome {
                Ok(()) => deleted.push(item.relative_path.clone()),
                // El registro ya no existe: lo que quede es un huérfano que
                // el analizador mostrará como recuperable.
                Err(reason) => left.push(
                    serde_json::json!({"relative_path": item.relative_path, "reason": reason}),
                ),
            }
        }
        // Su manifiesto junto al CAS ya no describe nada: se retira para que
        // una reconstrucción no la resucite como incompleta.
        let mut roots: Vec<&str> = items
            .iter()
            .map(|i| i.storage_location_id.as_str())
            .collect();
        roots.sort_unstable();
        roots.dedup();
        for location_id in roots {
            if let Ok(location) = self.location(location_id).await {
                if let Ok(root) = self.root(&location).await {
                    let path = join(&root, &format!("manifests/{variant_id}.json"));
                    let _ = tokio::task::spawn_blocking(move || remove(&path)).await;
                }
            }
        }
        self.ports
            .guard
            .release(&plan.plan_id, 1, Some(&finish("COMPLETED")))
            .await?;
        Ok(serde_json::json!({
            "deleted_files": deleted,
            "left_as_orphans": left,
            "deregistered": items.iter().filter(|i| i.action != "DELETE_FILE").count(),
        }))
    }
}

fn step<T: Serialize>(
    id: &str,
    kind: &str,
    phase: &str,
    plan_hash: &str,
    resource_ids: &[String],
    payload: &T,
    from: &[&str],
) -> JournalStep {
    JournalStep {
        id: id.into(),
        kind: kind.into(),
        phase: phase.into(),
        plan_hash: plan_hash.into(),
        resource_ids: resource_ids.to_vec(),
        payload: serde_json::to_value(payload).unwrap_or_default(),
        from_phases: from.iter().map(|f| (*f).to_string()).collect(),
    }
}

#[async_trait]
impl JobSource for StorageService {
    /// Jobs de limpieza: visibles para quien los pidió y para un administrador.
    async fn job(&self, principal: &Principal, id: &str) -> Result<Option<JobState>, LarmError> {
        let Some(record) = self.ports.cleanup_journal.get(id).await? else {
            return Ok(None);
        };
        let payload: CleanupPayload = match serde_json::from_value(record.payload) {
            Ok(p) => p,
            Err(_) => return Ok(None),
        };
        if payload.actor_id != principal.id && !principal.has_api_scope(ApiScope::Admin) {
            return Ok(None);
        }
        let state = match record.phase.as_str() {
            "PLANNED" => JobPhase::Queued,
            "COMPLETED" => JobPhase::Completed,
            "FAILED" | "CANCELLED" => JobPhase::Failed,
            _ => JobPhase::Running,
        };
        let mut progress = serde_json::Map::new();
        progress.insert("phase".into(), record.phase.clone().into());
        progress.insert("items_total".into(), payload.items.len().into());
        Ok(Some(JobState {
            job_id: record.id,
            state,
            resource_type: "storage_location".into(),
            resource_id: Some(payload.resource_id),
            progress,
            result: payload.result,
            error: payload.error,
            evidence_refs: vec![],
        }))
    }
}

/// Acceso al servicio desde la API (puerto `StorageMaintenance`).
#[derive(Clone)]
pub struct StorageHandle(pub Arc<StorageService>);

#[async_trait]
impl larm_domain::ports::StorageMaintenance for StorageHandle {
    async fn analysis(&self, id: &str) -> Result<serde_json::Value, LarmError> {
        let (analysis, _) = self.0.analysis(id).await?;
        serde_json::to_value(analysis).map_err(|e| internal("serialize", e.to_string()))
    }
    async fn cleanup_plan(&self, id: &str, ctx: &MutationContext) -> Result<Plan, LarmError> {
        self.0.cleanup_plan(id, ctx).await
    }
    async fn planned_cleanup(
        &self,
        id: &str,
        plan_id: &str,
        plan_hash: &str,
    ) -> Result<Plan, LarmError> {
        self.0.planned_cleanup(id, plan_id, plan_hash).await
    }
    async fn cleanup(
        &self,
        id: &str,
        plan_id: &str,
        plan_hash: &str,
        ctx: &MutationContext,
    ) -> Result<(JobReceipt, larm_domain::ports::BackgroundTask), LarmError> {
        self.0.cleanup(id, plan_id, plan_hash, ctx).await
    }
    async fn delete_plan(
        &self,
        variant_id: &str,
        ctx: &MutationContext,
    ) -> Result<Plan, LarmError> {
        self.0.delete_plan(variant_id, ctx).await
    }
    async fn planned_delete(&self, variant_id: &str, plan_hash: &str) -> Result<Plan, LarmError> {
        self.0.planned_delete(variant_id, plan_hash).await
    }
    async fn delete(
        &self,
        variant_id: &str,
        if_match: i64,
        plan_hash: &str,
        ctx: &MutationContext,
    ) -> Result<serde_json::Value, LarmError> {
        self.0.delete(variant_id, if_match, plan_hash, ctx).await
    }
}
