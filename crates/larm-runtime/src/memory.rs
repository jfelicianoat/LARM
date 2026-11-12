//! Estimación por pool y preflight de memoria (P035; contrato 04).
//!
//! Las fórmulas genéricas solo se aplican a transformers con metadata completa
//! y asignación conocida a un pool. Un runtime puede aportar un estimate más
//! preciso; los modelos multimedia requieren siempre el estimador del adapter.

use std::collections::{BTreeMap, BTreeSet};

use larm_domain::{
    error::{ErrorCode, LarmError},
    estimate::{Confidence, RuntimeMemoryEstimate, RuntimePoolMemoryEstimate},
    guard::{normalize, LockMode, Resource, ResourceKind},
    hardware::{DeviceInfo, DeviceKind},
    ports::{PoolReservationRequest, PoolReservations, ResourceGuard},
    units::{DecimalU64, DECIMAL_U64_MAX},
};

const MIB: u64 = 1024 * 1024;
const GIB: u64 = 1024 * 1024 * 1024;
pub const RUNTIME_OVERHEAD_MIN_BYTES: u64 = 256 * MIB;
pub const SAFETY_MARGIN_MIN_BYTES: u64 = 512 * MIB;
pub const DEFAULT_SYSTEM_RESERVE_BYTES: u64 = 4 * GIB;
pub const DEFAULT_SYSTEM_RESERVE_BASIS_POINTS: u64 = 1_000;
pub const DEFAULT_IDLE_TIMEOUT_SECONDS: i64 = 300;
pub const LOAD_OOM_RISK: &str = "LOAD_OOM_RISK";

/// Datos estables del modelo y datos específicos del adapter necesarios para
/// el fallback genérico transformer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransformerMemoryInput {
    pub weights_bytes: Option<u64>,
    pub layers: Option<u64>,
    pub kv_heads: Option<u64>,
    pub head_dim: Option<u64>,
    pub kv_bits: Option<u64>,
    pub context_tokens: Option<u64>,
    pub batch: Option<u64>,
    pub activation_bytes: Option<u64>,
    pub workspace_bytes: Option<u64>,
    pub multimodal_bytes: Option<u64>,
    pub temporary_bytes: Option<u64>,
    /// Sólo se indica cuando todas las asignaciones usan ese mismo pool; en
    /// memoria unificada debe ser el ID compartido del pool RAM/VRAM.
    pub single_pool_key: Option<String>,
    pub confidence: Confidence,
    pub evidence_refs: Vec<String>,
    pub multimodal: bool,
}

/// Error de datos o de aritmética del cálculo de memoria.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemoryError {
    InvalidInput(&'static str),
    Overflow(&'static str),
    EstimateUnknown,
    EstimateInconsistent(&'static str),
    PoolUnknown(String),
}

impl std::fmt::Display for MemoryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidInput(reason) => write!(f, "entrada de memoria no válida: {reason}"),
            Self::Overflow(field) => write!(f, "desbordamiento al calcular {field}"),
            Self::EstimateUnknown => f.write_str("la estimación de memoria es UNKNOWN"),
            Self::EstimateInconsistent(reason) => {
                write!(f, "la estimación de memoria es inconsistente: {reason}")
            }
            Self::PoolUnknown(pool) => write!(f, "no hay estado observable para el pool {pool}"),
        }
    }
}

impl std::error::Error for MemoryError {}

/// Calcula el KV cache con redondeo hacia arriba para precisiones no byte.
pub fn estimate_kv_bytes(
    layers: u64,
    kv_heads: u64,
    head_dim: u64,
    context_tokens: u64,
    batch: u64,
    kv_bits: u64,
) -> Result<u64, MemoryError> {
    let factors = [layers, kv_heads, head_dim, context_tokens, batch, kv_bits];
    if factors.contains(&0) {
        return Err(MemoryError::InvalidInput("kv_dimensions_must_be_positive"));
    }
    let bit_count = factors.into_iter().try_fold(2_u128, |acc, value| {
        acc.checked_mul(u128::from(value))
            .ok_or(MemoryError::Overflow("kv_bytes"))
    })?;
    let bytes = bit_count.div_ceil(8);
    let bytes = u64::try_from(bytes).map_err(|_| MemoryError::Overflow("kv_bytes"))?;
    checked_size(bytes, "kv_bytes")
}

/// Fallback genérico transformer. Datos incompletos, evidencia ausente,
/// memoria multimedia o ubicación no determinada producen UNKNOWN; no se
/// inventa una asignación ni se deriva KV del nombre del modelo.
pub fn estimate_transformer(
    input: &TransformerMemoryInput,
) -> Result<RuntimeMemoryEstimate, MemoryError> {
    if input.multimodal || input.confidence == Confidence::Unknown || input.evidence_refs.is_empty()
    {
        return Ok(unknown_estimate(input.evidence_refs.clone()));
    }
    let (
        Some(weights),
        Some(layers),
        Some(kv_heads),
        Some(head_dim),
        Some(kv_bits),
        Some(context),
        Some(batch),
        Some(activation),
        Some(workspace),
        Some(multimodal),
        Some(temporary),
        Some(pool_key),
    ) = (
        input.weights_bytes,
        input.layers,
        input.kv_heads,
        input.head_dim,
        input.kv_bits,
        input.context_tokens,
        input.batch,
        input.activation_bytes,
        input.workspace_bytes,
        input.multimodal_bytes,
        input.temporary_bytes,
        input
            .single_pool_key
            .as_deref()
            .filter(|key| !key.trim().is_empty()),
    )
    else {
        return Ok(unknown_estimate(input.evidence_refs.clone()));
    };
    if weights == 0
        || layers == 0
        || kv_heads == 0
        || head_dim == 0
        || kv_bits == 0
        || context == 0
        || batch == 0
    {
        return Ok(unknown_estimate(input.evidence_refs.clone()));
    }

    let kv = estimate_kv_bytes(layers, kv_heads, head_dim, context, batch, kv_bits)?;
    let overhead = RUNTIME_OVERHEAD_MIN_BYTES.max(ceil_basis_points(weights, 1_000)?);
    let base = checked_sum(
        [
            weights, kv, activation, workspace, overhead, multimodal, temporary,
        ],
        "memory_base",
    )?;
    let safety = SAFETY_MARGIN_MIN_BYTES.max(ceil_basis_points(base, 500)?);
    let total = checked_add(base, safety, "memory_total")?;
    let estimate = RuntimeMemoryEstimate {
        weights_bytes: decimal(weights, "weights_bytes")?,
        kv_bytes: decimal(kv, "kv_bytes")?,
        activation_bytes: decimal(activation, "activation_bytes")?,
        workspace_bytes: decimal(workspace, "workspace_bytes")?,
        overhead_bytes: decimal(overhead, "overhead_bytes")?,
        total_bytes: decimal(total, "total_bytes")?,
        per_pool: vec![RuntimePoolMemoryEstimate {
            pool_key: pool_key.to_string(),
            required_bytes: decimal_value(total, "per_pool.required_bytes")?,
        }],
        confidence: input.confidence,
        evidence_refs: input.evidence_refs.clone(),
        multimodal_bytes: decimal(multimodal, "multimodal_bytes")?,
        temporary_bytes: decimal(temporary, "temporary_bytes")?,
        safety_margin_bytes: decimal(safety, "safety_margin_bytes")?,
    };
    validate_estimate(&estimate)?;
    Ok(estimate)
}

/// Forma UNKNOWN canónica para una información insuficiente.
#[must_use]
pub fn unknown_estimate(evidence_refs: Vec<String>) -> RuntimeMemoryEstimate {
    RuntimeMemoryEstimate {
        weights_bytes: None,
        kv_bytes: None,
        activation_bytes: None,
        workspace_bytes: None,
        overhead_bytes: None,
        total_bytes: None,
        per_pool: Vec::new(),
        confidence: Confidence::Unknown,
        evidence_refs,
        multimodal_bytes: None,
        temporary_bytes: None,
        safety_margin_bytes: None,
    }
}

/// Valida que un adapter entregue un desglose completo, un margen correcto y
/// asignaciones por pool que no sumen memoria unificada dos veces.
pub fn validate_estimate(estimate: &RuntimeMemoryEstimate) -> Result<(), MemoryError> {
    if estimate.confidence == Confidence::Unknown {
        return Err(MemoryError::EstimateUnknown);
    }
    let values = [
        estimate.weights_bytes,
        estimate.kv_bytes,
        estimate.activation_bytes,
        estimate.workspace_bytes,
        estimate.overhead_bytes,
        estimate.multimodal_bytes,
        estimate.temporary_bytes,
        estimate.safety_margin_bytes,
        estimate.total_bytes,
    ];
    if values.iter().any(Option::is_none) || estimate.evidence_refs.is_empty() {
        return Err(MemoryError::EstimateInconsistent(
            "missing_component_or_evidence",
        ));
    }
    let [Some(weights), Some(kv), Some(activation), Some(workspace), Some(overhead), Some(multimodal), Some(temporary), Some(safety), Some(total)] =
        values
    else {
        return Err(MemoryError::EstimateInconsistent("missing_component"));
    };
    if estimate
        .evidence_refs
        .iter()
        .any(|reference| reference.trim().is_empty())
    {
        return Err(MemoryError::EstimateInconsistent("empty_evidence_ref"));
    }
    let base = checked_sum(
        [
            weights.get(),
            kv.get(),
            activation.get(),
            workspace.get(),
            overhead.get(),
            multimodal.get(),
            temporary.get(),
        ],
        "memory_base",
    )?;
    if safety.get() != SAFETY_MARGIN_MIN_BYTES.max(ceil_basis_points(base, 500)?) {
        return Err(MemoryError::EstimateInconsistent("safety_margin_formula"));
    }
    if checked_add(base, safety.get(), "memory_total")? != total.get() {
        return Err(MemoryError::EstimateInconsistent("total_components"));
    }
    let mut seen = BTreeSet::new();
    let mut pool_total = 0;
    for pool in &estimate.per_pool {
        if pool.pool_key.trim().is_empty() || !seen.insert(&pool.pool_key) {
            return Err(MemoryError::EstimateInconsistent("duplicate_or_empty_pool"));
        }
        pool_total = checked_add(pool_total, pool.required_bytes.get(), "per_pool_total")?;
    }
    if estimate.per_pool.is_empty() || pool_total != total.get() {
        return Err(MemoryError::EstimateInconsistent(
            "pool_allocations_do_not_match_total",
        ));
    }
    Ok(())
}

fn unknown_shape_is_valid(estimate: &RuntimeMemoryEstimate) -> bool {
    estimate.confidence == Confidence::Unknown
        && estimate
            .per_pool
            .iter()
            .all(|pool| !pool.pool_key.trim().is_empty())
        && estimate
            .per_pool
            .iter()
            .map(|pool| pool.pool_key.as_str())
            .collect::<BTreeSet<_>>()
            .len()
            == estimate.per_pool.len()
}

/// Clave estable para el pool físico de un dispositivo. Unified memory usa el
/// mismo `unified_pool_id` que RAM, por lo que sus presupuestos se fusionan.
pub fn pool_key_for_device(device: &DeviceInfo) -> Result<String, MemoryError> {
    if let Some(pool_id) = device
        .unified_pool_id
        .as_deref()
        .filter(|pool_id| !pool_id.trim().is_empty())
    {
        return Ok(pool_id.to_string());
    }
    let device_id = device
        .device_id
        .as_deref()
        .filter(|device_id| !device_id.trim().is_empty())
        .ok_or(MemoryError::InvalidInput("device_pool_identity_unknown"))?;
    let prefix = match device.kind {
        DeviceKind::Gpu => "gpu",
        DeviceKind::Npu => "npu",
    };
    Ok(format!("{prefix}:{device_id}"))
}

/// Estado observado de memoria para un pool. `other_reservations_bytes` debe
/// excluir al titular que se está admitiendo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryPoolSnapshot {
    pub pool_key: String,
    pub total_bytes: Option<u64>,
    pub os_available_bytes: Option<u64>,
    pub other_reservations_bytes: u64,
}

/// Fusiona lecturas duplicadas del mismo pool sin sumar la capacidad. Si dos
/// vistas discrepan, conserva capacidad/disponibilidad menores y reserva mayor.
pub fn deduplicate_pools(
    pools: &[MemoryPoolSnapshot],
) -> Result<Vec<MemoryPoolSnapshot>, MemoryError> {
    let mut merged = BTreeMap::<String, MemoryPoolSnapshot>::new();
    for pool in pools {
        if pool.pool_key.trim().is_empty() {
            return Err(MemoryError::InvalidInput("pool_key_empty"));
        }
        checked_size(pool.other_reservations_bytes, "other_reservations_bytes")?;
        merged
            .entry(pool.pool_key.clone())
            .and_modify(|current| {
                current.total_bytes = match (current.total_bytes, pool.total_bytes) {
                    (Some(left), Some(right)) => Some(left.min(right)),
                    _ => None,
                };
                current.os_available_bytes =
                    match (current.os_available_bytes, pool.os_available_bytes) {
                        (Some(left), Some(right)) => Some(left.min(right)),
                        _ => None,
                    };
                current.other_reservations_bytes = current
                    .other_reservations_bytes
                    .max(pool.other_reservations_bytes);
            })
            .or_insert_with(|| pool.clone());
    }
    Ok(merged.into_values().collect())
}

/// Reserva del sistema fijada por la configuración del Scheduler.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SystemReservePolicy {
    pub minimum_bytes: u64,
    pub basis_points: u64,
}

impl Default for SystemReservePolicy {
    fn default() -> Self {
        Self {
            minimum_bytes: DEFAULT_SYSTEM_RESERVE_BYTES,
            basis_points: DEFAULT_SYSTEM_RESERVE_BASIS_POINTS,
        }
    }
}

/// Presupuesto admisible de un pool después de reserva de sistema y otras
/// instancias. Memoria disponible del SO limita también pools dedicados.
pub fn usable_pool_budget(
    pool: &MemoryPoolSnapshot,
    reserve: SystemReservePolicy,
) -> Result<u64, MemoryError> {
    let total = pool
        .total_bytes
        .ok_or_else(|| MemoryError::PoolUnknown(pool.pool_key.clone()))?;
    let available = pool
        .os_available_bytes
        .ok_or_else(|| MemoryError::PoolUnknown(pool.pool_key.clone()))?;
    checked_size(total, "pool_total_bytes")?;
    checked_size(available, "pool_os_available_bytes")?;
    if total == 0 || reserve.basis_points > 10_000 {
        return Err(MemoryError::InvalidInput("pool_budget_policy_invalid"));
    }
    let system_reserve = reserve
        .minimum_bytes
        .max(ceil_basis_points(total, reserve.basis_points)?);
    let capacity_budget = total
        .saturating_sub(system_reserve)
        .saturating_sub(pool.other_reservations_bytes);
    let observed_budget = available.saturating_sub(system_reserve);
    Ok(capacity_budget.min(observed_budget))
}

/// Límites efectivos: el menor límite no nulo de perfil, lease y budget duro.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AdmissionLimits<'a> {
    pub profile_memory_limit_bytes: Option<&'a str>,
    pub lease_max_memory_bytes: Option<&'a str>,
    pub hard_memory_budget_bytes: Option<&'a str>,
}

/// Contexto para persistir una admisión de memoria bajo los guards P019.
#[derive(Debug, Clone, Copy)]
pub struct MemoryAdmissionRequest<'a> {
    pub owner_id: &'a str,
    pub generation: i64,
    pub variant_id: &'a str,
    pub artifact_ids: &'a [String],
    pub limits: AdmissionLimits<'a>,
    pub mode: AdmissionMode,
    pub pools: &'a [MemoryPoolSnapshot],
    pub reserve: SystemReservePolicy,
    pub alternatives: &'a [String],
}

fn parse_positive_limit(
    value: Option<&str>,
    field: &'static str,
) -> Result<Option<u64>, MemoryError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let parsed = DecimalU64::parse(value)
        .map_err(|_| MemoryError::InvalidInput(field))?
        .get();
    if parsed == 0 {
        return Err(MemoryError::InvalidInput(field));
    }
    Ok(Some(parsed))
}

pub fn effective_memory_limit(limits: AdmissionLimits<'_>) -> Result<Option<u64>, MemoryError> {
    Ok([
        parse_positive_limit(
            limits.profile_memory_limit_bytes,
            "profile_memory_limit_zero_or_invalid",
        )?,
        parse_positive_limit(
            limits.lease_max_memory_bytes,
            "lease_memory_limit_zero_or_invalid",
        )?,
        parse_positive_limit(
            limits.hard_memory_budget_bytes,
            "hard_memory_budget_zero_or_invalid",
        )?,
    ]
    .into_iter()
    .flatten()
    .min())
}

/// UNKNOWN nunca se admite automáticamente. La excepción del contrato 04
/// requiere un hard budget explícito del operador y allocations por pool que
/// puedan verificarse contra capacidad real.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdmissionMode {
    Automatic,
    OperatorHardBudget,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdmissionFailureReason {
    UnknownEstimate,
    InvalidEstimate,
    LimitExceeded,
    PoolUnavailable,
    PoolCapacityUnknown,
    InsufficientPool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryAdmissionFailure {
    pub code: &'static str,
    pub reason: AdmissionFailureReason,
    pub pool_key: Option<String>,
    pub required_bytes: Option<u64>,
    pub available_bytes: Option<u64>,
    pub effective_limit_bytes: Option<u64>,
    pub alternatives: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryAdmissionDecision {
    pub effective_limit_bytes: Option<u64>,
    pub admitted_bytes: u64,
    pub usable_pool_budgets: Vec<(String, u64)>,
    pub used_operator_hard_budget: bool,
}

fn admission_failure(
    reason: AdmissionFailureReason,
    pool_key: Option<String>,
    required_bytes: Option<u64>,
    available_bytes: Option<u64>,
    effective_limit_bytes: Option<u64>,
    alternatives: &[String],
) -> MemoryAdmissionFailure {
    MemoryAdmissionFailure {
        code: LOAD_OOM_RISK,
        reason,
        pool_key,
        required_bytes,
        available_bytes,
        effective_limit_bytes,
        alternatives: alternatives.to_vec(),
    }
}

/// Preflight sin IO: compara el snapshot, el mínimo de límites y cada pool.
pub fn check_memory_admission(
    estimate: &RuntimeMemoryEstimate,
    limits: AdmissionLimits<'_>,
    pools: &[MemoryPoolSnapshot],
    reserve: SystemReservePolicy,
    mode: AdmissionMode,
    alternatives: &[String],
) -> Result<MemoryAdmissionDecision, MemoryAdmissionFailure> {
    let limit = effective_memory_limit(limits).map_err(|_| {
        admission_failure(
            AdmissionFailureReason::LimitExceeded,
            None,
            None,
            None,
            None,
            alternatives,
        )
    })?;
    let known_estimate = estimate.confidence != Confidence::Unknown;
    if known_estimate {
        validate_estimate(estimate).map_err(|_| {
            admission_failure(
                AdmissionFailureReason::InvalidEstimate,
                None,
                estimate.total_bytes.map(DecimalU64::get),
                None,
                limit,
                alternatives,
            )
        })?;
    } else if !unknown_shape_is_valid(estimate) {
        return Err(admission_failure(
            AdmissionFailureReason::InvalidEstimate,
            None,
            None,
            None,
            limit,
            alternatives,
        ));
    }

    let used_operator_hard_budget = !known_estimate && mode == AdmissionMode::OperatorHardBudget;
    if !known_estimate && (!used_operator_hard_budget || limits.hard_memory_budget_bytes.is_none())
    {
        return Err(admission_failure(
            AdmissionFailureReason::UnknownEstimate,
            None,
            None,
            None,
            limit,
            alternatives,
        ));
    }
    if estimate.per_pool.is_empty() {
        return Err(admission_failure(
            AdmissionFailureReason::UnknownEstimate,
            None,
            None,
            None,
            limit,
            alternatives,
        ));
    }
    let pool_required_total = estimate.per_pool.iter().try_fold(0_u64, |sum, pool| {
        sum.checked_add(pool.required_bytes.get())
            .filter(|total| *total <= DECIMAL_U64_MAX)
    });
    let Some(pool_required_total) = pool_required_total else {
        return Err(admission_failure(
            AdmissionFailureReason::InvalidEstimate,
            None,
            None,
            None,
            limit,
            alternatives,
        ));
    };
    let admitted_bytes = if known_estimate {
        estimate.total_bytes.map(DecimalU64::get).unwrap_or(0)
    } else {
        pool_required_total
    };
    if limit.is_some_and(|limit| admitted_bytes > limit) {
        return Err(admission_failure(
            AdmissionFailureReason::LimitExceeded,
            None,
            Some(admitted_bytes),
            limit,
            limit,
            alternatives,
        ));
    }

    let canonical_pools = deduplicate_pools(pools).map_err(|_| {
        admission_failure(
            AdmissionFailureReason::PoolCapacityUnknown,
            None,
            None,
            None,
            limit,
            alternatives,
        )
    })?;
    let pool_map = canonical_pools
        .iter()
        .map(|pool| (pool.pool_key.as_str(), pool))
        .collect::<BTreeMap<_, _>>();
    let mut usable_budgets = Vec::with_capacity(estimate.per_pool.len());
    for allocation in &estimate.per_pool {
        let Some(pool) = pool_map.get(allocation.pool_key.as_str()) else {
            return Err(admission_failure(
                AdmissionFailureReason::PoolUnavailable,
                Some(allocation.pool_key.clone()),
                Some(allocation.required_bytes.get()),
                None,
                limit,
                alternatives,
            ));
        };
        let available = usable_pool_budget(pool, reserve).map_err(|_| {
            admission_failure(
                AdmissionFailureReason::PoolCapacityUnknown,
                Some(allocation.pool_key.clone()),
                Some(allocation.required_bytes.get()),
                None,
                limit,
                alternatives,
            )
        })?;
        if allocation.required_bytes.get() > available {
            return Err(admission_failure(
                AdmissionFailureReason::InsufficientPool,
                Some(allocation.pool_key.clone()),
                Some(allocation.required_bytes.get()),
                Some(available),
                limit,
                alternatives,
            ));
        }
        usable_budgets.push((allocation.pool_key.clone(), available));
    }
    Ok(MemoryAdmissionDecision {
        effective_limit_bytes: limit,
        admitted_bytes,
        usable_pool_budgets: usable_budgets,
        used_operator_hard_budget,
    })
}

/// Locks P019 para inmovilizar variante/artefactos y serializar la reserva de
/// cada pool mientras se prepara el launch. La normalización aplica el orden
/// global y elimina IDs repetidos.
pub fn admission_guard_requests(
    variant_id: &str,
    artifact_ids: &[String],
    estimate: &RuntimeMemoryEstimate,
) -> Result<Vec<(Resource, LockMode)>, MemoryError> {
    if variant_id.trim().is_empty() || artifact_ids.iter().any(|id| id.trim().is_empty()) {
        return Err(MemoryError::InvalidInput("guard_resource_id_empty"));
    }
    if estimate.per_pool.is_empty()
        || estimate
            .per_pool
            .iter()
            .any(|pool| pool.pool_key.trim().is_empty())
    {
        return Err(MemoryError::EstimateInconsistent("pool_guard_missing"));
    }
    let mut requests = vec![(
        Resource::new(ResourceKind::Variant, variant_id),
        LockMode::Shared,
    )];
    requests.extend(
        artifact_ids
            .iter()
            .map(|id| (Resource::new(ResourceKind::Artifact, id), LockMode::Shared)),
    );
    requests.extend(estimate.per_pool.iter().map(|pool| {
        (
            Resource::new(ResourceKind::Pool, &pool.pool_key),
            LockMode::Exclusive,
        )
    }));
    Ok(normalize(&requests))
}

/// Reserva un estimate completo bajo los guards de P019. El caller conserva
/// las reservas hasta que el lifecycle de la instancia confirme su liberación.
pub async fn reserve_memory_admission(
    guards: &dyn ResourceGuard,
    reservations: &dyn PoolReservations,
    estimate: &RuntimeMemoryEstimate,
    request: MemoryAdmissionRequest<'_>,
) -> Result<MemoryAdmissionDecision, LarmError> {
    let decision = check_memory_admission(
        estimate,
        request.limits,
        request.pools,
        request.reserve,
        request.mode,
        request.alternatives,
    )
    .map_err(|failure| {
        let retryable = matches!(
            failure.reason,
            AdmissionFailureReason::InsufficientPool | AdmissionFailureReason::PoolUnavailable
        );
        LarmError::new(
            ErrorCode::ResourceUnavailable,
            "El preflight de memoria no admite este launch",
            "memory_admission_failed",
        )
        .retryable(retryable)
        .with_technical(format!("{}: {failure:?}", failure.code))
    })?;
    let requests = admission_guard_requests(request.variant_id, request.artifact_ids, estimate)
        .map_err(|error| {
            LarmError::new(
                ErrorCode::ValidationFailed,
                "El estimate no es válido",
                "memory_estimate_invalid",
            )
            .with_technical(error.to_string())
        })?;
    guards
        .acquire(request.owner_id, request.generation, &requests, None)
        .await?;

    let pool_budgets = decision
        .usable_pool_budgets
        .iter()
        .map(|(pool_key, budget)| (pool_key.as_str(), *budget))
        .collect::<BTreeMap<_, _>>();
    let pool_requests = estimate
        .per_pool
        .iter()
        .map(|allocation| {
            let budget = pool_budgets
                .get(allocation.pool_key.as_str())
                .copied()
                .ok_or_else(|| {
                    LarmError::new(
                        ErrorCode::ResourceUnavailable,
                        "No se puede validar el pool",
                        "memory_pool_unavailable",
                    )
                })?;
            Ok(PoolReservationRequest {
                pool_key: allocation.pool_key.clone(),
                bytes: allocation.required_bytes.get(),
                budget,
            })
        })
        .collect::<Result<Vec<_>, LarmError>>();
    let pool_requests = match pool_requests {
        Ok(requests) => requests,
        Err(error) => {
            let _ = guards
                .release(request.owner_id, request.generation, None)
                .await;
            return Err(error);
        }
    };
    if let Err(error) = reservations
        .reserve_many(request.owner_id, &pool_requests)
        .await
    {
        let release = guards
            .release(request.owner_id, request.generation, None)
            .await;
        if let Err(release_error) = release {
            return Err(error.with_technical(format!(
                "reservation_failed_and_guard_release_failed: {release_error}"
            )));
        }
        return Err(error);
    }
    if let Err(error) = guards
        .release(request.owner_id, request.generation, None)
        .await
    {
        let _ = reservations.release(request.owner_id).await;
        return Err(
            error.with_technical("memory reservation rolled back after guard release failure")
        );
    }
    Ok(decision)
}

fn ceil_basis_points(value: u64, basis_points: u64) -> Result<u64, MemoryError> {
    let scaled = u128::from(value)
        .checked_mul(u128::from(basis_points))
        .ok_or(MemoryError::Overflow("basis_points"))?;
    let result = scaled.div_ceil(10_000);
    let result = u64::try_from(result).map_err(|_| MemoryError::Overflow("basis_points"))?;
    checked_size(result, "basis_points")
}

fn checked_size(value: u64, field: &'static str) -> Result<u64, MemoryError> {
    (value <= DECIMAL_U64_MAX)
        .then_some(value)
        .ok_or(MemoryError::Overflow(field))
}

fn checked_add(left: u64, right: u64, field: &'static str) -> Result<u64, MemoryError> {
    checked_size(
        left.checked_add(right)
            .ok_or(MemoryError::Overflow(field))?,
        field,
    )
}

fn checked_sum<const N: usize>(values: [u64; N], field: &'static str) -> Result<u64, MemoryError> {
    values
        .into_iter()
        .try_fold(0, |sum, value| checked_add(sum, value, field))
}

fn decimal_value(value: u64, field: &'static str) -> Result<DecimalU64, MemoryError> {
    DecimalU64::new(value).ok_or(MemoryError::Overflow(field))
}

fn decimal(value: u64, field: &'static str) -> Result<Option<DecimalU64>, MemoryError> {
    decimal_value(value, field).map(Some)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use larm_domain::hardware::{DeviceInfo, DeviceKind};

    fn transformer(pool_key: &str) -> TransformerMemoryInput {
        TransformerMemoryInput {
            weights_bytes: Some(2 * GIB),
            layers: Some(32),
            kv_heads: Some(8),
            head_dim: Some(128),
            kv_bits: Some(4),
            context_tokens: Some(4096),
            batch: Some(1),
            activation_bytes: Some(256 * MIB),
            workspace_bytes: Some(128 * MIB),
            multimodal_bytes: Some(0),
            temporary_bytes: Some(0),
            single_pool_key: Some(pool_key.into()),
            confidence: Confidence::High,
            evidence_refs: vec!["model-metadata:fixture".into(), "adapter:fixture".into()],
            multimodal: false,
        }
    }

    fn estimate(pool_key: &str, bytes: u64) -> RuntimeMemoryEstimate {
        let overhead = RUNTIME_OVERHEAD_MIN_BYTES.max(ceil_basis_points(bytes, 1_000).unwrap());
        let base = bytes + 1 + overhead;
        let safety = SAFETY_MARGIN_MIN_BYTES.max(ceil_basis_points(base, 500).unwrap());
        let total = base + safety;
        RuntimeMemoryEstimate {
            weights_bytes: decimal(bytes, "weights").unwrap(),
            kv_bytes: decimal(1, "kv").unwrap(),
            activation_bytes: decimal(0, "activation").unwrap(),
            workspace_bytes: decimal(0, "workspace").unwrap(),
            overhead_bytes: decimal(overhead, "overhead").unwrap(),
            total_bytes: decimal(total, "total").unwrap(),
            per_pool: vec![RuntimePoolMemoryEstimate {
                pool_key: pool_key.into(),
                required_bytes: decimal_value(total, "pool").unwrap(),
            }],
            confidence: Confidence::Estimated,
            evidence_refs: vec!["fixture:known-estimate".into()],
            multimodal_bytes: decimal(0, "multimodal").unwrap(),
            temporary_bytes: decimal(0, "temporary").unwrap(),
            safety_margin_bytes: decimal(safety, "safety").unwrap(),
        }
    }

    fn no_reserve() -> SystemReservePolicy {
        SystemReservePolicy {
            minimum_bytes: 0,
            basis_points: 0,
        }
    }

    fn pool(pool_key: &str, total: u64, available: u64, reserved: u64) -> MemoryPoolSnapshot {
        MemoryPoolSnapshot {
            pool_key: pool_key.into(),
            total_bytes: Some(total),
            os_available_bytes: Some(available),
            other_reservations_bytes: reserved,
        }
    }

    #[test]
    fn formula_kv_overhead_margen_y_serializacion() {
        let estimate = estimate_transformer(&transformer("system-ram")).unwrap();
        assert_eq!(
            estimate.kv_bytes.unwrap().get(),
            estimate_kv_bytes(32, 8, 128, 4096, 1, 4).unwrap()
        );
        assert_eq!(estimate.kv_bytes.unwrap().get(), 134_217_728);
        assert_eq!(estimate.overhead_bytes.unwrap().get(), 256 * MIB);
        let base =
            estimate.total_bytes.unwrap().get() - estimate.safety_margin_bytes.unwrap().get();
        assert_eq!(
            estimate.safety_margin_bytes.unwrap().get(),
            SAFETY_MARGIN_MIN_BYTES.max(ceil_basis_points(base, 500).unwrap())
        );
        let value = serde_json::to_value(&estimate).unwrap();
        assert!(value["total_bytes"].is_string(), "bytes son strings JSON");
        assert_eq!(validate_estimate(&estimate), Ok(()));
    }

    #[test]
    fn fallback_incompleto_y_multimodal_quedan_unknown() {
        let mut incomplete = transformer("system-ram");
        incomplete.kv_heads = None;
        let unknown = estimate_transformer(&incomplete).unwrap();
        assert_eq!(unknown.confidence, Confidence::Unknown);
        assert!(
            check_memory_admission(
                &unknown,
                AdmissionLimits::default(),
                &[pool("system-ram", 32 * GIB, 24 * GIB, 0)],
                no_reserve(),
                AdmissionMode::Automatic,
                &[],
            )
            .unwrap_err()
            .reason
                == AdmissionFailureReason::UnknownEstimate
        );
        let mut multimodal = transformer("system-ram");
        multimodal.multimodal = true;
        assert_eq!(
            estimate_transformer(&multimodal).unwrap().confidence,
            Confidence::Unknown
        );
    }

    #[test]
    fn memoria_unificada_deduplica_el_mismo_pool_y_no_suma_capacidad() {
        let gpu = DeviceInfo {
            kind: DeviceKind::Gpu,
            vendor: Some("fixture".into()),
            device_id: Some("gpu0".into()),
            luid: None,
            name: Some("Unified fixture".into()),
            driver_version: None,
            dedicated_memory_bytes: Some(DecimalU64::new(8 * GIB).unwrap()),
            shared_memory_bytes: Some(DecimalU64::new(24 * GIB).unwrap()),
            unified_pool_id: Some("system-ram".into()),
            backends: vec![],
            evidence_refs: vec![],
        };
        assert_eq!(pool_key_for_device(&gpu).unwrap(), "system-ram");
        let pools = deduplicate_pools(&[
            pool("system-ram", 32 * GIB, 20 * GIB, 2 * GIB),
            pool(
                &pool_key_for_device(&gpu).unwrap(),
                32 * GIB,
                20 * GIB,
                2 * GIB,
            ),
        ])
        .unwrap();
        assert_eq!(pools.len(), 1);
        assert_eq!(
            usable_pool_budget(&pools[0], no_reserve()).unwrap(),
            20 * GIB
        );
    }

    #[test]
    fn limite_efectivo_es_el_menor_y_cero_se_rechaza() {
        assert_eq!(
            effective_memory_limit(AdmissionLimits {
                profile_memory_limit_bytes: Some("900"),
                lease_max_memory_bytes: Some("700"),
                hard_memory_budget_bytes: Some("800"),
            })
            .unwrap(),
            Some(700)
        );
        assert_eq!(
            effective_memory_limit(AdmissionLimits {
                profile_memory_limit_bytes: Some("0"),
                lease_max_memory_bytes: None,
                hard_memory_budget_bytes: None,
            }),
            Err(MemoryError::InvalidInput(
                "profile_memory_limit_zero_or_invalid"
            ))
        );
        assert!(estimate_kv_bytes(u64::MAX, u64::MAX, 2, 2, 2, 2).is_err());
    }

    #[test]
    fn perfil_lease_budget_y_reserva_observada_se_aplican_al_preflight() {
        let estimate = estimate("system-ram", 700);
        let alternatives = vec!["perfil ECO".into(), "variante menor".into()];
        let limit_failure = check_memory_admission(
            &estimate,
            AdmissionLimits {
                profile_memory_limit_bytes: Some("900"),
                lease_max_memory_bytes: Some("650"),
                hard_memory_budget_bytes: Some("800"),
            },
            &[pool("system-ram", 10_000, 10_000, 0)],
            no_reserve(),
            AdmissionMode::Automatic,
            &alternatives,
        )
        .unwrap_err();
        assert_eq!(limit_failure.code, LOAD_OOM_RISK);
        assert_eq!(limit_failure.reason, AdmissionFailureReason::LimitExceeded);
        assert_eq!(limit_failure.effective_limit_bytes, Some(650));
        assert_eq!(limit_failure.alternatives, alternatives);

        let reserved_failure = check_memory_admission(
            &estimate,
            AdmissionLimits::default(),
            &[pool("system-ram", 1_000, 1_000, 400)],
            no_reserve(),
            AdmissionMode::Automatic,
            &[],
        )
        .unwrap_err();
        assert_eq!(
            reserved_failure.reason,
            AdmissionFailureReason::InsufficientPool
        );
        assert_eq!(reserved_failure.available_bytes, Some(600));
    }

    #[test]
    fn hard_budget_explicito_es_la_unica_via_para_unknown_parcial() {
        let mut unknown = unknown_estimate(vec!["adapter:hard-bound".into()]);
        unknown.per_pool.push(RuntimePoolMemoryEstimate {
            pool_key: "system-ram".into(),
            required_bytes: DecimalU64::new(500).unwrap(),
        });
        let rejected = check_memory_admission(
            &unknown,
            AdmissionLimits {
                hard_memory_budget_bytes: Some("600"),
                ..AdmissionLimits::default()
            },
            &[pool("system-ram", 10_000, 10_000, 0)],
            no_reserve(),
            AdmissionMode::Automatic,
            &[],
        )
        .unwrap_err();
        assert_eq!(rejected.reason, AdmissionFailureReason::UnknownEstimate);
        let admitted = check_memory_admission(
            &unknown,
            AdmissionLimits {
                hard_memory_budget_bytes: Some("600"),
                ..AdmissionLimits::default()
            },
            &[pool("system-ram", 10_000, 10_000, 0)],
            no_reserve(),
            AdmissionMode::OperatorHardBudget,
            &[],
        )
        .unwrap();
        assert!(admitted.used_operator_hard_budget);
        assert_eq!(admitted.admitted_bytes, 500);
    }

    #[test]
    fn guards_p019_ordenan_variante_artefactos_y_pools_sin_repetidos() {
        let mut estimate = estimate_transformer(&transformer("system-ram")).unwrap();
        estimate.per_pool.push(RuntimePoolMemoryEstimate {
            pool_key: "system-ram".into(),
            required_bytes: DecimalU64::new(0).unwrap(),
        });
        let requests = admission_guard_requests(
            "variant-1",
            &["artifact-1".into(), "artifact-1".into()],
            &estimate,
        )
        .unwrap();
        assert_eq!(requests.len(), 3);
        assert_eq!(requests[0].0.kind, ResourceKind::Variant);
        assert_eq!(requests[1].0.kind, ResourceKind::Artifact);
        assert_eq!(requests[2].0.kind, ResourceKind::Pool);
        assert_eq!(requests[2].1, LockMode::Exclusive);
    }

    #[tokio::test]
    async fn admission_real_reserva_en_sqlite_bajo_guard_p019() {
        use larm_db::guard::{SqlPoolReservations, SqlResourceGuard};
        use larm_domain::ports::PoolReservations;

        let (_directory, db) = larm_db::testing::temp_migrated().await.unwrap();
        let guards = SqlResourceGuard {
            pool: db.pool.clone(),
        };
        let reservations = SqlPoolReservations {
            pool: db.pool.clone(),
        };
        let estimate = estimate_transformer(&transformer("system-ram")).unwrap();
        let planned_bytes = estimate.total_bytes.unwrap().get();
        let artifacts = ["artifact-1".into()];
        let pools = [pool("system-ram", 16 * GIB, 16 * GIB, 0)];
        let alternatives = [];
        let decision = reserve_memory_admission(
            &guards,
            &reservations,
            &estimate,
            MemoryAdmissionRequest {
                owner_id: "lease-memory-1",
                generation: 1,
                variant_id: "variant-1",
                artifact_ids: &artifacts,
                limits: AdmissionLimits::default(),
                mode: AdmissionMode::Automatic,
                pools: &pools,
                reserve: no_reserve(),
                alternatives: &alternatives,
            },
        )
        .await
        .unwrap();

        assert_eq!(decision.admitted_bytes, planned_bytes);
        assert_eq!(
            reservations.reserved("system-ram").await.unwrap(),
            planned_bytes
        );
        assert!(
            guards
                .holders(&Resource::new(ResourceKind::Variant, "variant-1"))
                .await
                .unwrap()
                .is_empty(),
            "los guards temporales se liberan tras persistir la reserva"
        );
    }

    #[tokio::test]
    async fn admission_unknown_persistida_exige_override_y_budget_duro() {
        use larm_db::guard::{SqlPoolReservations, SqlResourceGuard};
        use larm_domain::ports::PoolReservations;

        let (_directory, db) = larm_db::testing::temp_migrated().await.unwrap();
        let guards = SqlResourceGuard {
            pool: db.pool.clone(),
        };
        let reservations = SqlPoolReservations {
            pool: db.pool.clone(),
        };
        let mut estimate = unknown_estimate(vec!["adapter:hard-bound".into()]);
        estimate.per_pool.push(RuntimePoolMemoryEstimate {
            pool_key: "system-ram".into(),
            required_bytes: DecimalU64::new(500).unwrap(),
        });
        let artifacts = [];
        let pools = [pool("system-ram", 10_000, 10_000, 0)];
        let alternatives = [];
        let limits = AdmissionLimits {
            hard_memory_budget_bytes: Some("600"),
            ..AdmissionLimits::default()
        };
        let request = |mode| MemoryAdmissionRequest {
            owner_id: "lease-unknown-1",
            generation: 1,
            variant_id: "variant-unknown-1",
            artifact_ids: &artifacts,
            limits,
            mode,
            pools: &pools,
            reserve: no_reserve(),
            alternatives: &alternatives,
        };

        let automatic = reserve_memory_admission(
            &guards,
            &reservations,
            &estimate,
            request(AdmissionMode::Automatic),
        )
        .await
        .unwrap_err();
        assert_eq!(automatic.code, ErrorCode::ResourceUnavailable);
        assert_eq!(
            reservations.reserved("system-ram").await.unwrap(),
            0,
            "UNKNOWN automático no debe persistir reservas"
        );

        let admitted = reserve_memory_admission(
            &guards,
            &reservations,
            &estimate,
            request(AdmissionMode::OperatorHardBudget),
        )
        .await
        .unwrap();
        assert!(admitted.used_operator_hard_budget);
        assert_eq!(admitted.admitted_bytes, 500);
        assert_eq!(reservations.reserved("system-ram").await.unwrap(), 500);
    }

    #[test]
    fn hardware_real_de_windows_verifica_admision_con_memoria_observada() {
        #[cfg(windows)]
        {
            use sysinfo::{MemoryRefreshKind, RefreshKind, System};
            let mut system = System::new_with_specifics(
                RefreshKind::nothing().with_memory(MemoryRefreshKind::everything()),
            );
            system.refresh_memory();
            let total = system.total_memory();
            let available = system.available_memory();
            assert!(total > 0, "GlobalMemoryStatusEx debe observar RAM total");
            assert!(available <= total, "RAM disponible no supera la total");
            let fixture = estimate_transformer(&transformer("system-ram")).unwrap();
            let observed_pool = pool("system-ram", total, available, 0);
            let result = check_memory_admission(
                &fixture,
                AdmissionLimits::default(),
                &[observed_pool],
                SystemReservePolicy::default(),
                AdmissionMode::Automatic,
                &[],
            );
            println!(
                "windows_cim_total_bytes={total} available_bytes={available} estimate_bytes={} admission={:?}",
                fixture.total_bytes.unwrap().get(),
                result.as_ref().map(|decision| decision.admitted_bytes)
            );
            assert!(
                result.is_ok()
                    || result.as_ref().unwrap_err().reason
                        == AdmissionFailureReason::InsufficientPool,
                "el preflight real debe admitir o rechazar por capacidad observada"
            );
        }
        #[cfg(not(windows))]
        panic!("prueba REAL de P035 requiere Windows");
    }
}
