use super::*;
use larm_domain::{
    estimate::{estimate, Confidence, HardBudget, ItemKind, Phase, PhaseItem, Size},
    transformations::TransformationInputGuard,
    units::DecimalU64,
};
use larm_transform::environment::PlanningStorage;

const WORKER_CONTROL: u64 = 3 << 20;
const PROVENANCE_MAX: u64 = 16 << 10;
const CORE_CONTROL: u64 = 1 << 20;
fn bytes(n: u64) -> Result<DecimalU64, LarmError> {
    DecimalU64::new(n)
        .ok_or_else(|| error(ErrorCode::ValidationFailed, "transformation_size_overflow"))
}
fn mapped_plan(
    request: &TransformationRequest,
    input: &dyn TransformationInputGuard,
    worker: &TransformationPlanResponse,
    binding: TransformationExecutionBinding,
    storage: &PlanningStorage,
    planner_id: &str,
) -> Result<SealedTransformationPlan, LarmError> {
    let input_bytes = input.artifacts()[0].size_bytes.get();
    if input_bytes > 64 << 20 {
        return Err(error(
            ErrorCode::UnsupportedCapability,
            "reference_transform_input_limit",
        ));
    }
    // El tamaño final es un invariante de la RECETA: la Base64 de referencia
    // expande 4/3; un conversor de producto (GGUF) declara el tamaño del
    // contenedor y el ancla es su re-preflight en execute (fail-closed ante
    // cambios del input) más la verificación por hash de la salida.
    let (data, phase_name, data_item) = match planner_id {
        "product.gguf.v1" | "product.onnx.v1" | "product.onnxq.v1" => {
            let declared = worker
                .storage_estimate
                .final_artifact_bytes
                .ok_or_else(|| {
                    error(
                        ErrorCode::ProtocolError,
                        "transformation_reference_estimate_changed",
                    )
                })?
                .get();
            let labels = match planner_id {
                "product.onnxq.v1" => ("product_onnxq_conversion", "product_onnxq_data"),
                "product.onnx.v1" => ("product_onnx_conversion", "product_onnx_data"),
                _ => ("product_gguf_conversion", "product_gguf_data"),
            };
            (declared, labels.0, labels.1)
        }
        _ => {
            let data = input_bytes.div_ceil(3).checked_mul(4).ok_or_else(|| {
                error(ErrorCode::ValidationFailed, "transformation_size_overflow")
            })?;
            if worker.storage_estimate.final_artifact_bytes != Some(bytes(data)?)
                || worker.storage_estimate.temporary_peak_bytes != Some(bytes(WORKER_CONTROL)?)
                || worker.storage_estimate.upper_bound_bytes != Some(bytes(data + WORKER_CONTROL)?)
            {
                return Err(error(
                    ErrorCode::ProtocolError,
                    "transformation_reference_estimate_changed",
                ));
            }
            (data, "reference_base64_conversion", "reference_data")
        }
    };
    let control = worker
        .storage_estimate
        .temporary_peak_bytes
        .ok_or_else(|| {
            error(
                ErrorCode::ProtocolError,
                "transformation_reference_estimate_changed",
            )
        })?
        .get();
    let upper = worker
        .storage_estimate
        .upper_bound_bytes
        .ok_or_else(|| {
            error(
                ErrorCode::ProtocolError,
                "transformation_reference_estimate_changed",
            )
        })?
        .get();
    if upper != control.saturating_add(data) {
        return Err(error(
            ErrorCode::ProtocolError,
            "transformation_reference_estimate_changed",
        ));
    }
    let key = storage
        .workspaces
        .first()
        .ok_or_else(|| {
            error(
                ErrorCode::ResourceUnavailable,
                "transformation_workspace_missing",
            )
        })?
        .volume_key
        .clone();
    if storage.workspaces.iter().any(|r| r.volume_key != key) {
        return Err(error(
            ErrorCode::Incompatible,
            "reference_transform_same_volume_required",
        ));
    }
    let policy = TransformationOutputPolicy {
        worker_volume_key: key.clone(),
        cas_volume_key: storage.destination.volume_key.clone(),
        max_total_bytes: bytes(data + PROVENANCE_MAX)?,
        max_file_bytes: bytes(data.max(PROVENANCE_MAX))?,
        max_artifacts: 2,
        worker_control_bytes: bytes(WORKER_CONTROL)?,
        control_bytes: bytes(CORE_CONTROL)?,
    };
    // Fixed-size data is algorithmic. The other items are enforced quotas,
    // represented as bounded UNKNOWN until their actual bytes are measured.
    let conversion = Phase {
        name: phase_name.into(),
        items: vec![
            PhaseItem {
                name: data_item.into(),
                kind: ItemKind::Temporary,
                volume_key: key.clone(),
                size: Size::exact(data),
                existing: false,
            },
            PhaseItem {
                name: "reference_control".into(),
                kind: ItemKind::Temporary,
                volume_key: key.clone(),
                size: Size::estimated(0, WORKER_CONTROL, Confidence::Unknown),
                existing: false,
            },
            PhaseItem {
                name: "reference_provenance".into(),
                kind: ItemKind::Temporary,
                volume_key: key,
                size: Size::estimated(0, PROVENANCE_MAX, Confidence::Unknown),
                existing: false,
            },
        ],
    };
    let mut phases = vec![
        conversion,
        policy
            .registration_phase()
            .map_err(|_| error(ErrorCode::ValidationFailed, "transformation_policy_invalid"))?,
    ];
    for phase in &mut phases {
        for (artifact, location) in input.artifacts().iter().zip(input.locations()) {
            phase.items.push(PhaseItem {
                name: format!("existing_input:{}", artifact.artifact_id),
                kind: ItemKind::FinalArtifact,
                volume_key: location.volume_key.clone(),
                size: Size::exact(artifact.size_bytes.get()),
                existing: true,
            });
        }
    }
    let mut computed = estimate(
        &phases,
        &storage.volumes,
        storage.policy.margin,
        request.hard_budget_bytes.map(|n| HardBudget(n.get())),
    )
    .map_err(|e| {
        error(
            ErrorCode::ValidationFailed,
            "transformation_estimate_invalid",
        )
        .with_technical(format!("{e:?}"))
    })?;
    if computed.per_volume.iter().any(|v| !v.eligible)
        || request
            .hard_budget_bytes
            .is_some_and(|budget| computed.upper_bound_bytes.is_none_or(|n| n > budget))
    {
        return Err(error(
            ErrorCode::StorageInsufficient,
            "transformation_global_budget_insufficient",
        ));
    }
    computed.evidence_refs.extend([
        format!("conformance:{}", binding.conformance_run_id),
        "reference:base64-streaming-and-bounded-sidecars".into(),
        "core:managed-publication-enforced-control-quota".into(),
    ]);
    if storage.destination.filesystem.eq_ignore_ascii_case("FAT32")
        && policy.max_file_bytes.get() > u32::MAX as u64
    {
        return Err(error(
            ErrorCode::Incompatible,
            "transformation_file_bound_exceeds_fat32",
        ));
    }
    let mut steps = worker.steps.clone();
    steps.push(PlanStep{phase:"register_managed_output".into(),adapter_id:None,request:serde_json::json!({"planner":planner_id,"storage_location_id":storage.destination.id,"destination_row_version":storage.destination.row_version,"profile_sha256":binding.profile_sha256})});
    seal_publication_plan(
        id(),
        request.clone(),
        input.artifacts().to_vec(),
        PlanDraft {
            operation: worker.operation.clone(),
            resource_ids: worker.resource_ids.clone(),
            storage_estimate: computed,
            steps,
            expires_at: worker.expires_at.clone(),
            approval_required: worker.approval_required,
        },
        binding,
        policy,
    )
    .map_err(|_| {
        error(
            ErrorCode::IntegrityFailed,
            "transformation_plan_seal_failed",
        )
    })
}
impl Service {
    pub(super) async fn plan(
        &self,
        request: &TransformationRequest,
        ctx: &MutationContext,
    ) -> Result<TransformationPlanResponse, LarmError> {
        let backend = backend::Backend::select(self, request, ctx).await?;
        let input = self
            .inner
            .env
            .input(&request.input_variant_id, None)
            .await?;
        backend.check_input(request, &input)?;
        let worker = self
            .inner
            .runner
            .transformation_worker_expected(
                &backend.installation,
                &backend.run,
                &backend.backend_context,
                Some(&backend.identity),
            )
            .await?;
        let prepared = async {
            let description = worker
                .describe(&id(), &backend.access, Some(Duration::from_secs(30)))
                .await?;
            if description.tool_version != request.tool_version
                || !description.formats_output.contains(&request.target_format)
            {
                return Err(error(
                    ErrorCode::UnsupportedCapability,
                    "transformation_tool_or_format_not_supported",
                ));
            }
            // Los conversores de producto reales (cuantización K en Python
            // stdlib) superan con holgura el presupuesto del worker trivial:
            // el timeout es un detector de cuelgue, no un límite de rendimiento.
            let preview = worker
                .plan(
                    &id(),
                    request,
                    &backend.access,
                    Some(Duration::from_secs(600)),
                )
                .await?;
            let storage = self
                .inner
                .env
                .planning_storage(
                    &request.storage_location_id,
                    &[
                        backend.local.work_dir.join("state"),
                        backend.local.work_dir.join("output"),
                    ],
                    &larm_db::reservations::SqlVolumeReservations {
                        pool: self.inner.db.pool.clone(),
                    },
                )
                .await?;
            let plan = mapped_plan(
                request,
                &input,
                &preview,
                worker.execution_binding(&preview)?,
                &storage,
                backend.planner_id,
            )?;
            input.recheck().await?;
            worker.recheck().await?;
            Ok(plan)
        }
        .await;
        // El preview cierra antes de persistir: store_plan revalida la
        // cualificación y toma la exclusión breve de la instalación, que el
        // worker retiene hasta soltarlo (se cierra antes de responder).
        let closed = worker.close().await;
        drop(worker);
        let plan = match prepared {
            Err(original) => {
                if let Err(e) = closed {
                    tracing::error!(reason=%e.details.reason,"no terminó el cierre del preview transformation");
                }
                return Err(original);
            }
            Ok(plan) => {
                closed?;
                plan
            }
        };
        self.active()?;
        self.plans().store_plan(&plan, ctx).await?;
        Ok(plan.response)
    }
}
