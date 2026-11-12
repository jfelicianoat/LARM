//! Planificación determinista de bundles y destinos (P052).
//!
//! Este módulo consume metadatos ya obtenidos; no consulta fuentes, calcula
//! reservas persistentes ni crea jobs. La ruta pública espera el contrato de
//! selección opcional y los servicios durables de sus dependencias.

use larm_domain::{
    adapter_updates::is_sha256,
    download::{DownloadArtifactEstimate, DownloadSourceCandidate},
    estimate::{Confidence, HardBudget, ItemKind, Phase, PhaseItem, Size, VolumeSnapshot},
    locations::StorageLocation,
    model::{bundle_path, ArtifactSizeConfidence, BundleState, ModelBundle},
    units::DecimalU64,
};
use larm_storage::estimate::{plan_destinations, DestinationOption, SpacePolicy, DESTINATION};
use std::collections::{BTreeMap, BTreeSet};

const MAX_DECIMAL_BYTES: u64 = i64::MAX as u64;

/// Evidencia de tamaño conservada por artefacto durante la planificación.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactSize {
    Exact {
        bytes: u64,
    },
    High {
        bytes: u64,
        upper_bound_bytes: u64,
    },
    Estimated {
        bytes: u64,
        upper_bound_bytes: u64,
    },
    /// Un intervalo se muestra como intervalo; para admisión se reserva su máximo.
    Range {
        minimum_bytes: u64,
        maximum_bytes: u64,
    },
    Unknown,
}

impl ArtifactSize {
    fn validate(self) -> Result<(), PlannerError> {
        match self {
            Self::Exact { bytes } if bytes <= MAX_DECIMAL_BYTES => Ok(()),
            Self::High {
                bytes,
                upper_bound_bytes,
            }
            | Self::Estimated {
                bytes,
                upper_bound_bytes,
            } if bytes <= upper_bound_bytes && upper_bound_bytes <= MAX_DECIMAL_BYTES => Ok(()),
            Self::Range {
                minimum_bytes,
                maximum_bytes,
            } if minimum_bytes <= maximum_bytes && maximum_bytes <= MAX_DECIMAL_BYTES => Ok(()),
            Self::Unknown => Ok(()),
            _ => Err(PlannerError::InvalidSize),
        }
    }

    fn storage_size(self) -> Size {
        match self {
            Self::Exact { bytes } => Size::exact(bytes),
            Self::High {
                bytes,
                upper_bound_bytes,
            } => Size::estimated(bytes, upper_bound_bytes, Confidence::High),
            Self::Estimated {
                bytes,
                upper_bound_bytes,
            } => Size::estimated(bytes, upper_bound_bytes, Confidence::Estimated),
            Self::Range { maximum_bytes, .. } => Size {
                bytes: None,
                upper_bound: Some(maximum_bytes),
                confidence: Confidence::Estimated,
            },
            Self::Unknown => Size::unknown(),
        }
    }

    fn displayed_confidence(self) -> Confidence {
        match self {
            Self::Exact { .. } => Confidence::Exact,
            Self::High { .. } => Confidence::High,
            Self::Estimated { .. } | Self::Range { .. } => Confidence::Estimated,
            Self::Unknown => Confidence::Unknown,
        }
    }

    fn display_bounds(self) -> (Option<u64>, Option<u64>) {
        match self {
            Self::Exact { bytes } => (Some(bytes), Some(bytes)),
            Self::High {
                bytes,
                upper_bound_bytes,
            }
            | Self::Estimated {
                bytes,
                upper_bound_bytes,
            } => (Some(bytes), Some(upper_bound_bytes)),
            Self::Range {
                minimum_bytes,
                maximum_bytes,
            } => (Some(minimum_bytes), Some(maximum_bytes)),
            Self::Unknown => (None, None),
        }
    }
}

/// Descriptor estable de un artefacto incluido en el manifiesto del bundle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannerArtifact {
    pub artifact_id: String,
    pub relative_path: String,
    pub required: bool,
    pub size: ArtifactSize,
    pub sha256: Option<String>,
}

/// Errores de selección, manifiesto o cálculo de tamaños.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlannerError {
    BundleNotVerified,
    InvalidAcquisition,
    InvalidBundle,
    InvalidArtifact,
    MissingArtifact,
    InvalidOptionalSelection,
    InvalidSize,
    InvalidMirrorCandidate,
    InvalidMirrorPreference,
    SizeOverflow,
    DestinationPlanning,
}

/// Resumen agregado que mantiene desconocido el total si algún fichero no
/// tiene tamaño puntual; un rango conserva sus límites en cada artefacto.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BundleSizeSummary {
    pub download_bytes: Option<u64>,
    pub upper_bound_bytes: Option<u64>,
    pub confidence: Confidence,
}

/// Salida pura del planificador: artefactos seleccionados, tamaños y todas las
/// alternativas de destino con su motivo de admisión o rechazo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BundleDownloadPreview {
    pub artifacts: Vec<PlannerArtifact>,
    pub size: BundleSizeSummary,
    pub destinations: Vec<DestinationOption>,
}

fn checked_add(left: u64, right: u64) -> Result<u64, PlannerError> {
    left.checked_add(right)
        .filter(|value| *value <= MAX_DECIMAL_BYTES)
        .ok_or(PlannerError::SizeOverflow)
}

fn validate_artifact(artifact: &PlannerArtifact) -> Result<(), PlannerError> {
    if artifact.artifact_id.is_empty() || bundle_path(&artifact.relative_path).is_err() {
        return Err(PlannerError::InvalidArtifact);
    }
    artifact.size.validate()?;
    if artifact
        .sha256
        .as_deref()
        .is_some_and(|hash| !is_sha256(hash))
    {
        return Err(PlannerError::InvalidArtifact);
    }
    Ok(())
}

/// Aplica la selección del operador: todos los artefactos requeridos y solo
/// los opcionales seleccionados. Rechaza IDs repetidos, ajenos o ausentes.
pub fn select_bundle_artifacts(
    bundle: &ModelBundle,
    artifacts: &[PlannerArtifact],
    selected_optional_ids: &[String],
) -> Result<Vec<PlannerArtifact>, PlannerError> {
    if bundle.state != BundleState::Verified {
        return Err(PlannerError::BundleNotVerified);
    }
    if !is_sha256(&bundle.manifest_hash) {
        return Err(PlannerError::InvalidBundle);
    }
    select_artifacts(
        &bundle.required_artifact_ids,
        &bundle.optional_artifact_ids,
        artifacts,
        selected_optional_ids,
    )
}

fn select_artifacts(
    required_ids: &[String],
    optional_ids: &[String],
    artifacts: &[PlannerArtifact],
    selected_optional_ids: &[String],
) -> Result<Vec<PlannerArtifact>, PlannerError> {
    let required: BTreeSet<&str> = required_ids.iter().map(String::as_str).collect();
    let optional: BTreeSet<&str> = optional_ids.iter().map(String::as_str).collect();
    if required.len() != required_ids.len()
        || optional.len() != optional_ids.len()
        || !required.is_disjoint(&optional)
    {
        return Err(PlannerError::InvalidBundle);
    }

    let selected: BTreeSet<&str> = selected_optional_ids.iter().map(String::as_str).collect();
    if selected.len() != selected_optional_ids.len() || !selected.is_subset(&optional) {
        return Err(PlannerError::InvalidOptionalSelection);
    }

    let mut by_id = BTreeMap::new();
    let mut paths = BTreeSet::new();
    for artifact in artifacts {
        validate_artifact(artifact)?;
        let lower_path = artifact.relative_path.to_lowercase();
        if !paths.insert(lower_path)
            || by_id
                .insert(artifact.artifact_id.as_str(), artifact)
                .is_some()
        {
            return Err(PlannerError::InvalidArtifact);
        }
        let belongs_required = required.contains(artifact.artifact_id.as_str());
        let belongs_optional = optional.contains(artifact.artifact_id.as_str());
        if (!belongs_required && !belongs_optional) || artifact.required != belongs_required {
            return Err(PlannerError::InvalidArtifact);
        }
    }
    if by_id.len() != required.len() + optional.len() {
        return Err(PlannerError::MissingArtifact);
    }

    let mut chosen = Vec::with_capacity(required.len() + selected.len());
    for id in required.iter().chain(selected.iter()) {
        let artifact = by_id.get(id).ok_or(PlannerError::MissingArtifact)?;
        chosen.push((*artifact).clone());
    }
    chosen.sort_by(|left, right| {
        left.relative_path
            .cmp(&right.relative_path)
            .then_with(|| left.artifact_id.cmp(&right.artifact_id))
    });
    Ok(chosen)
}

/// Selección remota pura: exige snapshot íntegro, sin fabricar ModelBundle ni
/// VERIFIED. El servicio consumidor debe además validar la cualificación actual.
pub fn select_acquisition_artifacts(
    manifest: &larm_domain::acquisition::ModelAcquisitionManifest,
    selected_optional_ids: &[String],
) -> Result<Vec<PlannerArtifact>, PlannerError> {
    manifest
        .validate()
        .map_err(|_| PlannerError::InvalidAcquisition)?;
    let artifacts = manifest
        .artifacts
        .iter()
        .map(|artifact| {
            Ok(PlannerArtifact {
                artifact_id: artifact.artifact_id.clone(),
                relative_path: artifact.relative_path.clone(),
                required: artifact.required,
                sha256: artifact.sha256.clone(),
                size: crate::preview_service::planner_size(&artifact.size_estimate)
                    .map_err(|_| PlannerError::InvalidSize)?,
            })
        })
        .collect::<Result<Vec<_>, PlannerError>>()?;
    select_artifacts(
        &manifest.required_artifact_ids,
        &manifest.optional_artifact_ids,
        &artifacts,
        selected_optional_ids,
    )
}

/// Suma segura: cualquier fichero sin tamaño puntual mantiene el total como
/// desconocido; una cota agregada existe solo si todos los ficheros tienen cota.
pub fn summarize_sizes(artifacts: &[PlannerArtifact]) -> Result<BundleSizeSummary, PlannerError> {
    if artifacts.is_empty() {
        return Err(PlannerError::InvalidBundle);
    }
    let mut known_total = Some(0u64);
    let mut upper_total = Some(0u64);
    let mut confidence = Confidence::Exact;
    for artifact in artifacts {
        validate_artifact(artifact)?;
        let (displayed, upper) = artifact.size.display_bounds();
        let known = if matches!(artifact.size, ArtifactSize::Range { .. }) {
            None
        } else {
            displayed
        };
        confidence = confidence.max(artifact.size.displayed_confidence());
        known_total = match (known_total, known) {
            (Some(total), Some(value)) => Some(checked_add(total, value)?),
            _ => None,
        };
        upper_total = match (upper_total, upper) {
            (Some(total), Some(value)) => Some(checked_add(total, value)?),
            _ => None,
        };
    }
    Ok(BundleSizeSummary {
        download_bytes: known_total,
        upper_bound_bytes: upper_total,
        confidence,
    })
}

/// Calcula alternativas con el motor P015; no reserva ni inicia la descarga.
pub fn preview_bundle_download(
    bundle: &ModelBundle,
    artifacts: &[PlannerArtifact],
    selected_optional_ids: &[String],
    locations: &[StorageLocation],
    volumes: &[VolumeSnapshot],
    policy: SpacePolicy,
    hard_budget: Option<HardBudget>,
) -> Result<BundleDownloadPreview, PlannerError> {
    let chosen = select_bundle_artifacts(bundle, artifacts, selected_optional_ids)?;
    preview_selected(chosen, locations, volumes, policy, hard_budget)
}

/// Alternativas para la primera adquisición; sólo calcula, no crea jobs/reservas.
pub fn preview_acquisition_download(
    manifest: &larm_domain::acquisition::ModelAcquisitionManifest,
    selected_optional_ids: &[String],
    locations: &[StorageLocation],
    volumes: &[VolumeSnapshot],
    policy: SpacePolicy,
    hard_budget: Option<HardBudget>,
) -> Result<BundleDownloadPreview, PlannerError> {
    let chosen = select_acquisition_artifacts(manifest, selected_optional_ids)?;
    preview_selected(chosen, locations, volumes, policy, hard_budget)
}

fn preview_selected(
    chosen: Vec<PlannerArtifact>,
    locations: &[StorageLocation],
    volumes: &[VolumeSnapshot],
    policy: SpacePolicy,
    hard_budget: Option<HardBudget>,
) -> Result<BundleDownloadPreview, PlannerError> {
    let size = summarize_sizes(&chosen)?;
    let phase = Phase {
        name: "download".into(),
        items: chosen
            .iter()
            .map(|artifact| PhaseItem {
                name: artifact.relative_path.clone(),
                kind: ItemKind::Download,
                volume_key: DESTINATION.into(),
                size: artifact.size.storage_size(),
                existing: false,
            })
            .collect(),
    };
    let mut destinations = plan_destinations(&[phase], locations, volumes, policy, hard_budget)
        .map_err(|_| PlannerError::DestinationPlanning)?;
    // P015 may substitute a hard budget to reserve bounded physical space.
    // That bound is not a declared byte count: retain unknown/range points in
    // the public download/final fields, while peak/margin/reservation stay bounded.
    for destination in &mut destinations {
        if let Some(estimate) = &mut destination.estimate {
            estimate.download_bytes = size.download_bytes.and_then(DecimalU64::new);
            estimate.final_artifact_bytes = size.download_bytes.and_then(DecimalU64::new);
        }
    }
    Ok(BundleDownloadPreview {
        artifacts: chosen,
        size,
        destinations,
    })
}

/// Datos de un posible mirror para un mismo artefacto de identidad fijada.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirrorCandidate {
    pub source_id: String,
    pub revision_key: String,
    pub revision_pinned: bool,
    pub available: bool,
    pub authenticated: bool,
    pub authentication_required: bool,
    pub confidence: Confidence,
    pub https: bool,
    pub source_priority: u64,
    pub size_bytes: u64,
    pub sha256: String,
}

/// Selecciona un mirror con identidad común y revisión fijada. No mezcla
/// segmentos entre fuentes; si no existe hash+tamaño compartido, no hay mirror.
#[must_use]
pub fn choose_mirror<'a>(
    candidates: &'a [MirrorCandidate],
    expected_size_bytes: u64,
    expected_sha256: &str,
) -> Option<&'a MirrorCandidate> {
    rank_mirrors(candidates, expected_size_bytes, expected_sha256)
        .into_iter()
        .next()
}

/// Devuelve todos los mirrors elegibles en el orden contractual, conservando
/// la identidad de cada fuente para que el plan pueda sellar y mostrar las
/// alternativas antes de pedir confirmación.
#[must_use]
pub fn rank_mirrors<'a>(
    candidates: &'a [MirrorCandidate],
    expected_size_bytes: u64,
    expected_sha256: &str,
) -> Vec<&'a MirrorCandidate> {
    if !is_sha256(expected_sha256) || expected_size_bytes > MAX_DECIMAL_BYTES {
        return Vec::new();
    }
    let mut eligible = candidates
        .iter()
        .filter(|candidate| {
            candidate.available
                && candidate.revision_pinned
                && !candidate.revision_key.is_empty()
                && !candidate.source_id.is_empty()
                && (!candidate.authentication_required || candidate.authenticated)
                && candidate.size_bytes == expected_size_bytes
                && candidate.sha256 == expected_sha256
                && is_sha256(&candidate.sha256)
        })
        .collect::<Vec<_>>();
    eligible.sort_by(|left, right| {
        right
            .authenticated
            .cmp(&left.authenticated)
            .then_with(|| confidence_rank(right.confidence).cmp(&confidence_rank(left.confidence)))
            .then_with(|| right.https.cmp(&left.https))
            .then_with(|| right.source_priority.cmp(&left.source_priority))
            .then_with(|| left.source_id.cmp(&right.source_id))
            .then_with(|| left.revision_key.cmp(&right.revision_key))
    });
    eligible
}

/// Comprueba que cada candidato cubra exactamente los artefactos aprobados y
/// devuelve una lista determinista. El source_id solicitado se coloca primero,
/// pero sólo si está dentro del conjunto elegible.
pub fn order_source_candidates(
    mut candidates: Vec<DownloadSourceCandidate>,
    selected_artifacts: &[DownloadArtifactEstimate],
    preferred_source_id: Option<&str>,
) -> Result<Vec<DownloadSourceCandidate>, PlannerError> {
    let selected = selected_artifacts
        .iter()
        .filter(|artifact| artifact.selected)
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        return Ok(candidates);
    }
    if selected.is_empty() || candidates.len() > 10_000 {
        return Err(PlannerError::InvalidMirrorCandidate);
    }
    let mut ids = BTreeSet::new();
    for candidate in &candidates {
        if !larm_domain::bridge::is_uuid_v7(&candidate.source_id)
            || !larm_domain::bridge::is_uuid_v7(&candidate.variant_id)
            || !larm_domain::bridge::is_uuid_v7(&candidate.revision_id)
            || candidate.revision_key.trim().is_empty()
            || candidate.revision_key.len() > 4096
            || !is_sha256(&candidate.manifest_hash)
            || candidate.source_priority > i64::MAX as u64
            || candidate.artifacts.len() != selected.len()
            || !ids.insert(candidate.source_id.as_str())
        {
            return Err(PlannerError::InvalidMirrorCandidate);
        }
        let mut target_ids = BTreeSet::new();
        let mut source_ids = BTreeSet::new();
        let mut paths = BTreeSet::new();
        for mapping in &candidate.artifacts {
            if !larm_domain::bridge::is_uuid_v7(&mapping.target_artifact_id)
                || !larm_domain::bridge::is_uuid_v7(&mapping.source_artifact_id)
                || !is_sha256(&mapping.sha256)
                || bundle_path(&mapping.relative_path).is_err()
                || !target_ids.insert(mapping.target_artifact_id.as_str())
                || !source_ids.insert(mapping.source_artifact_id.as_str())
                || !paths.insert(mapping.relative_path.to_lowercase())
            {
                return Err(PlannerError::InvalidMirrorCandidate);
            }
            let expected = selected
                .iter()
                .find(|artifact| artifact.artifact_id == mapping.target_artifact_id)
                .ok_or(PlannerError::InvalidMirrorCandidate)?;
            if expected.relative_path != mapping.relative_path
                || expected.sha256.as_deref() != Some(mapping.sha256.as_str())
                || expected.size_estimate.confidence != ArtifactSizeConfidence::Exact
                || expected.size_estimate.bytes.map(DecimalU64::get)
                    != Some(mapping.size_bytes.get())
            {
                return Err(PlannerError::InvalidMirrorCandidate);
            }
        }
        if candidate.confidence != ArtifactSizeConfidence::Exact {
            return Err(PlannerError::InvalidMirrorCandidate);
        }
    }

    candidates.sort_by(|left, right| {
        right
            .authenticated
            .cmp(&left.authenticated)
            .then_with(|| {
                mirror_confidence_rank(right.confidence)
                    .cmp(&mirror_confidence_rank(left.confidence))
            })
            .then_with(|| right.https.cmp(&left.https))
            .then_with(|| right.source_priority.cmp(&left.source_priority))
            .then_with(|| left.source_id.cmp(&right.source_id))
    });
    if let Some(preferred) = preferred_source_id {
        let Some(index) = candidates
            .iter()
            .position(|candidate| candidate.source_id == preferred)
        else {
            return Err(PlannerError::InvalidMirrorPreference);
        };
        candidates.swap(0, index);
    }
    candidates.truncate(16);
    Ok(candidates)
}

const fn mirror_confidence_rank(confidence: ArtifactSizeConfidence) -> u8 {
    match confidence {
        ArtifactSizeConfidence::Exact => 4,
        ArtifactSizeConfidence::High => 3,
        ArtifactSizeConfidence::Estimated => 2,
        ArtifactSizeConfidence::Unknown => 1,
    }
}

const fn confidence_rank(confidence: Confidence) -> u8 {
    match confidence {
        Confidence::Exact => 4,
        Confidence::High => 3,
        Confidence::Estimated => 2,
        Confidence::Unknown => 1,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use larm_domain::{
        estimate::{MarginPolicy, VolumeSnapshot},
        locations::LocationState,
        model::{BundleState, ModelBundle},
        storage::PerformanceClass,
        units::DecimalU64,
    };

    const HASH: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    fn bundle(required: &[&str], optional: &[&str]) -> ModelBundle {
        ModelBundle {
            id: "bundle".into(),
            created_at: "2026-01-01T00:00:00.000Z".into(),
            updated_at: "2026-01-01T00:00:00.000Z".into(),
            row_version: 1,
            metadata: Default::default(),
            variant_id: "variant".into(),
            required_artifact_ids: required.iter().map(|id| (*id).into()).collect(),
            optional_artifact_ids: optional.iter().map(|id| (*id).into()).collect(),
            manifest_hash: HASH.into(),
            state: BundleState::Verified,
        }
    }

    fn artifact(id: &str, path: &str, required: bool, size: ArtifactSize) -> PlannerArtifact {
        PlannerArtifact {
            artifact_id: id.into(),
            relative_path: path.into(),
            required,
            size,
            sha256: Some(HASH.into()),
        }
    }

    fn location(id: &str, key: &str, state: LocationState, free: u64) -> StorageLocation {
        StorageLocation {
            id: id.into(),
            created_at: "t".into(),
            updated_at: "t".into(),
            row_version: 1,
            metadata: Default::default(),
            name: id.into(),
            root_path: format!("{id}:\\"),
            canonical_path: format!("{id}:\\"),
            volume_guid: Some(key.into()),
            drive_letter: None,
            unc_path: None,
            filesystem: "NTFS".into(),
            storage_type: "LOCAL".into(),
            capacity_bytes: DecimalU64::new(10_000).unwrap(),
            free_bytes: DecimalU64::new(free).unwrap(),
            reserved_bytes: DecimalU64::default(),
            priority: 0,
            auto_placement: true,
            state,
            last_seen_at: None,
            volume_key: key.into(),
            performance_class: PerformanceClass::Ssd,
        }
    }

    fn exact_download_artifact() -> DownloadArtifactEstimate {
        let size = DecimalU64::new(4).unwrap();
        DownloadArtifactEstimate {
            artifact_id: "0192f000-0000-7000-8000-00000000e101".into(),
            relative_path: "weights.bin".into(),
            required: true,
            selected: true,
            size_estimate: larm_domain::model::ArtifactSizeEstimate {
                confidence: ArtifactSizeConfidence::Exact,
                bytes: Some(size),
                lower_bound_bytes: Some(size),
                upper_bound_bytes: Some(size),
                source: larm_domain::model::ArtifactSizeSource::Manifest,
                evidence_refs: vec!["fixture:manifest".into()],
                reason: None,
            },
            sha256: Some(HASH.into()),
        }
    }

    fn source_candidate(index: u64, priority: u64) -> DownloadSourceCandidate {
        let source_id = format!("0192f000-0000-7000-8000-{index:012x}");
        DownloadSourceCandidate {
            source_id,
            variant_id: "0192f000-0000-7000-8000-00000000e102".into(),
            revision_id: "0192f000-0000-7000-8000-00000000e103".into(),
            revision_key: format!("revision-{index}"),
            manifest_hash: HASH.into(),
            source_priority: priority,
            confidence: ArtifactSizeConfidence::Exact,
            authenticated: false,
            https: true,
            artifacts: vec![larm_domain::download::DownloadMirrorArtifact {
                target_artifact_id: "0192f000-0000-7000-8000-00000000e101".into(),
                source_artifact_id: format!("0192f000-0000-7000-8000-{:012x}", index + 100),
                relative_path: "weights.bin".into(),
                sha256: HASH.into(),
                size_bytes: DecimalU64::new(4).unwrap(),
            }],
        }
    }

    fn volume(key: &str, free: u64) -> VolumeSnapshot {
        VolumeSnapshot {
            volume_key: key.into(),
            os_free: free,
            policy_minimum: 0,
            other_reservations: 0,
        }
    }

    const SPACE_POLICY: SpacePolicy = SpacePolicy {
        margin: MarginPolicy {
            minimum_margin_bytes: 0,
            margin_basis_points: 0,
        },
        minimum_free_bytes: 0,
    };

    fn acquisition_fixture() -> larm_domain::acquisition::ModelAcquisitionManifest {
        use larm_domain::acquisition::*;
        let id = || uuid::Uuid::now_v7().to_string();
        let source_id = id();
        let artifact_id = id();
        ModelAcquisitionManifest {
            id: id(),
            variant_id: id(),
            revision_id: id(),
            source_id: source_id.clone(),
            source_reference: SourceCatalogReference {
                source_id,
                upstream_namespace: "fixture".into(),
                upstream_model_id: "model".into(),
                revision_key: "r1".into(),
            },
            upstream_manifest_hash: "a".repeat(64),
            snapshot_hash: String::new(),
            observed_at: "2026-10-27T16:00:00.000Z".into(),
            required_artifact_ids: vec![artifact_id.clone()],
            optional_artifact_ids: vec![],
            artifacts: vec![ModelAcquisitionArtifact {
                artifact_id,
                relative_path: "weights.bin".into(),
                size_bytes: None,
                sha256: None,
                required: true,
                role: "weights".into(),
                size_estimate: larm_domain::model::ArtifactSizeEstimate::unknown(
                    "not_reported",
                    "fixture:manifest",
                ),
            }],
            adapter_installation_id: id(),
            adapter_installation_revision: "2".into(),
            adapter_installation_sha256: "b".repeat(64),
            conformance_run_id: id(),
            conformance_run_sha256: "c".repeat(64),
        }
        .seal()
        .unwrap()
    }

    #[test]
    fn first_acquisition_selects_snapshot_without_a_physical_bundle() {
        let manifest = acquisition_fixture();
        let locations = vec![location("d", "vd", LocationState::Online, 10000)];
        let volumes = vec![volume("vd", 10000)];
        let blocked =
            preview_acquisition_download(&manifest, &[], &locations, &volumes, SPACE_POLICY, None)
                .unwrap();
        assert_eq!(blocked.destinations[0].reason, "size_unknown");
        let bounded = preview_acquisition_download(
            &manifest,
            &[],
            &locations,
            &volumes,
            SPACE_POLICY,
            Some(HardBudget(100)),
        )
        .unwrap();
        assert!(bounded.destinations[0].eligible);
        assert_eq!(bounded.size.download_bytes, None);
        assert_eq!(bounded.size.upper_bound_bytes, None);
        assert_eq!(bounded.size.confidence, Confidence::Unknown);
        let estimate = bounded.destinations[0].estimate.as_ref().unwrap();
        assert_eq!(estimate.download_bytes, None);
        assert_eq!(estimate.final_artifact_bytes, None);
        assert!(estimate.required_free_bytes.is_some() && estimate.peak_extra_bytes.is_some());
        assert_eq!(estimate.confidence, Confidence::Unknown);
        assert_eq!(
            select_acquisition_artifacts(&manifest, &[uuid::Uuid::now_v7().to_string()]),
            Err(PlannerError::InvalidOptionalSelection)
        );
        let mut corrupt = manifest;
        corrupt.artifacts[0].role = "changed".into();
        assert_eq!(
            select_acquisition_artifacts(&corrupt, &[]),
            Err(PlannerError::InvalidAcquisition)
        );
    }

    #[test]
    fn acquisition_interval_keeps_unknown_point_and_evidenced_estimated_bound() {
        let mut manifest = acquisition_fixture();
        manifest.artifacts[0].size_estimate = larm_domain::model::ArtifactSizeEstimate {
            confidence: ArtifactSizeConfidence::Estimated,
            bytes: None,
            lower_bound_bytes: DecimalU64::new(10),
            upper_bound_bytes: DecimalU64::new(20),
            source: larm_domain::model::ArtifactSizeSource::SourceMetadata,
            evidence_refs: vec!["fixture:range".into()],
            reason: None,
        };
        let manifest = manifest.seal().unwrap();
        let result = preview_acquisition_download(
            &manifest,
            &[],
            &[location("d", "vd", LocationState::Online, 10000)],
            &[volume("vd", 10000)],
            SPACE_POLICY,
            None,
        )
        .unwrap();
        assert_eq!(
            result.artifacts[0].size,
            ArtifactSize::Range {
                minimum_bytes: 10,
                maximum_bytes: 20
            }
        );
        assert!(result.destinations[0].eligible);
        assert_eq!(
            result.size.download_bytes, None,
            "an interval has no measured point"
        );
        assert_eq!(result.size.upper_bound_bytes, Some(20));
        assert_eq!(result.size.confidence, Confidence::Estimated);
    }

    #[test]
    fn bundle_exacto_suma_solo_requeridos_y_opcionales_elegidos() {
        let bundle = bundle(&["weights"], &["tokenizer", "mmproj"]);
        let artifacts = vec![
            artifact(
                "weights",
                "model.gguf",
                true,
                ArtifactSize::Exact { bytes: 100 },
            ),
            artifact(
                "tokenizer",
                "tokenizer.json",
                false,
                ArtifactSize::Exact { bytes: 20 },
            ),
            artifact(
                "mmproj",
                "mmproj.gguf",
                false,
                ArtifactSize::Exact { bytes: 40 },
            ),
        ];
        let selected = select_bundle_artifacts(&bundle, &artifacts, &["tokenizer".into()]).unwrap();
        let summary = summarize_sizes(&selected).unwrap();
        assert_eq!(selected.len(), 2);
        assert_eq!(
            (summary.download_bytes, summary.upper_bound_bytes),
            (Some(120), Some(120))
        );
        assert_eq!(summary.confidence, Confidence::Exact);
    }

    #[test]
    fn opcionales_exigen_eleccion_y_rechazan_ids_ajenos_o_repetidos() {
        let bundle = bundle(&["weights"], &["tokenizer"]);
        let artifacts = vec![
            artifact(
                "weights",
                "model.gguf",
                true,
                ArtifactSize::Exact { bytes: 100 },
            ),
            artifact(
                "tokenizer",
                "tokenizer.json",
                false,
                ArtifactSize::Exact { bytes: 20 },
            ),
        ];
        let selected = select_bundle_artifacts(&bundle, &artifacts, &[]).unwrap();
        assert_eq!(
            selected
                .iter()
                .map(|a| a.artifact_id.as_str())
                .collect::<Vec<_>>(),
            ["weights"]
        );
        assert_eq!(
            select_bundle_artifacts(&bundle, &artifacts, &["missing".into()]),
            Err(PlannerError::InvalidOptionalSelection)
        );
        assert_eq!(
            select_bundle_artifacts(
                &bundle,
                &artifacts,
                &["tokenizer".into(), "tokenizer".into()]
            ),
            Err(PlannerError::InvalidOptionalSelection)
        );
    }

    #[test]
    fn rango_y_unknown_se_mantienen_distintos_y_no_generan_total_falso() {
        let artifacts = vec![
            artifact(
                "range",
                "range.bin",
                true,
                ArtifactSize::Range {
                    minimum_bytes: 10,
                    maximum_bytes: 30,
                },
            ),
            artifact("unknown", "unknown.bin", true, ArtifactSize::Unknown),
        ];
        let summary = summarize_sizes(&artifacts).unwrap();
        assert_eq!(summary.download_bytes, None);
        assert_eq!(summary.upper_bound_bytes, None);
        assert_eq!(summary.confidence, Confidence::Unknown);
        assert_eq!(artifacts[0].size.display_bounds(), (Some(10), Some(30)));
        assert_eq!(artifacts[1].size.display_bounds(), (None, None));
    }

    #[test]
    fn bundle_invalido_artefacto_ausente_y_overflow_se_rechazan() {
        let mut bundle = bundle(&["weights"], &[]);
        let valid = artifact(
            "weights",
            "model.gguf",
            true,
            ArtifactSize::Exact { bytes: 1 },
        );
        bundle.state = BundleState::Incomplete;
        assert_eq!(
            select_bundle_artifacts(&bundle, std::slice::from_ref(&valid), &[]),
            Err(PlannerError::BundleNotVerified)
        );
        bundle.state = BundleState::Verified;
        assert_eq!(
            select_bundle_artifacts(&bundle, &[], &[]),
            Err(PlannerError::MissingArtifact)
        );
        let sizes = [
            artifact(
                "a",
                "a.bin",
                true,
                ArtifactSize::Exact {
                    bytes: MAX_DECIMAL_BYTES,
                },
            ),
            artifact("b", "b.bin", true, ArtifactSize::Exact { bytes: 1 }),
        ];
        assert_eq!(summarize_sizes(&sizes), Err(PlannerError::SizeOverflow));
    }

    #[test]
    fn alternativas_de_destino_explican_cual_cabe() {
        let bundle = bundle(&["weights"], &[]);
        let artifacts = vec![artifact(
            "weights",
            "model.gguf",
            true,
            ArtifactSize::Exact { bytes: 100 },
        )];
        let locations = vec![
            location("d", "vd", LocationState::Online, 200),
            location("e", "ve", LocationState::Online, 50),
            location("f", "vf", LocationState::Offline, 500),
        ];
        let volumes = vec![volume("vd", 200), volume("ve", 50)];
        let preview = preview_bundle_download(
            &bundle,
            &artifacts,
            &[],
            &locations,
            &volumes,
            SPACE_POLICY,
            None,
        )
        .unwrap();
        let reasons: BTreeMap<&str, (&str, bool)> = preview
            .destinations
            .iter()
            .map(|option| {
                (
                    option.location_id.as_str(),
                    (option.reason.as_str(), option.eligible),
                )
            })
            .collect();
        assert_eq!(reasons["d"], ("fits", true));
        assert!(reasons["e"].0.starts_with("insufficient_space"));
        assert_eq!(reasons["f"], ("state_offline", false));
    }

    #[test]
    fn tamano_desconocido_bloquea_sin_presupuesto_y_usa_cota_dura_si_se_da() {
        let bundle = bundle(&["weights"], &[]);
        let artifacts = vec![artifact(
            "weights",
            "model.gguf",
            true,
            ArtifactSize::Unknown,
        )];
        let locations = vec![location("d", "vd", LocationState::Online, 10_000)];
        let volumes = vec![volume("vd", 10_000)];
        let blocked = preview_bundle_download(
            &bundle,
            &artifacts,
            &[],
            &locations,
            &volumes,
            SPACE_POLICY,
            None,
        )
        .unwrap();
        assert!(!blocked.destinations[0].eligible);
        assert_eq!(blocked.destinations[0].reason, "size_unknown");
        let bounded = preview_bundle_download(
            &bundle,
            &artifacts,
            &[],
            &locations,
            &volumes,
            SPACE_POLICY,
            Some(HardBudget(100)),
        )
        .unwrap();
        assert!(bounded.destinations[0].eligible);
        assert_eq!(bounded.size.confidence, Confidence::Unknown);
    }

    #[test]
    fn mirrors_exigen_identidad_comun_y_siguen_la_prioridad_contractual() {
        let candidates = vec![
            MirrorCandidate {
                source_id: "z".into(),
                revision_key: "r2".into(),
                revision_pinned: true,
                available: true,
                authenticated: false,
                authentication_required: false,
                confidence: Confidence::Exact,
                https: true,
                source_priority: 99,
                size_bytes: 100,
                sha256: HASH.into(),
            },
            MirrorCandidate {
                source_id: "b".into(),
                revision_key: "r1".into(),
                revision_pinned: true,
                available: true,
                authenticated: true,
                authentication_required: true,
                confidence: Confidence::Estimated,
                https: false,
                source_priority: 1,
                size_bytes: 100,
                sha256: HASH.into(),
            },
            MirrorCandidate {
                source_id: "a".into(),
                revision_key: "r1".into(),
                revision_pinned: false,
                available: true,
                authenticated: true,
                authentication_required: false,
                confidence: Confidence::Exact,
                https: true,
                source_priority: 100,
                size_bytes: 100,
                sha256: HASH.into(),
            },
            MirrorCandidate {
                source_id: "c".into(),
                revision_key: "r1".into(),
                revision_pinned: true,
                available: true,
                authenticated: true,
                authentication_required: false,
                confidence: Confidence::Exact,
                https: true,
                source_priority: 100,
                size_bytes: 101,
                sha256: HASH.into(),
            },
        ];
        assert_eq!(
            choose_mirror(&candidates, 100, HASH).unwrap().source_id,
            "b"
        );
        assert!(choose_mirror(
            &candidates,
            100,
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
        )
        .is_none());
    }

    #[test]
    fn mirrors_ordenan_autenticacion_confianza_https_prioridad_y_source_id() {
        let candidate = |source_id: &str,
                         authenticated: bool,
                         confidence: Confidence,
                         https: bool,
                         source_priority: u64| MirrorCandidate {
            source_id: source_id.into(),
            revision_key: "fixed-revision".into(),
            revision_pinned: true,
            available: true,
            authenticated,
            authentication_required: false,
            confidence,
            https,
            source_priority,
            size_bytes: 100,
            sha256: HASH.into(),
        };
        let candidates = vec![
            candidate("0", false, Confidence::Exact, true, 1000),
            candidate("z", true, Confidence::Exact, false, 1),
            candidate("c", true, Confidence::Exact, true, 4),
            candidate("a", true, Confidence::Exact, true, 4),
            candidate("b", true, Confidence::High, true, 100),
            candidate("unavailable", true, Confidence::Exact, true, 9999),
        ];
        let mut candidates = candidates;
        candidates.last_mut().unwrap().available = false;
        assert_eq!(
            choose_mirror(&candidates, 100, HASH).unwrap().source_id,
            "a"
        );
        assert_eq!(
            rank_mirrors(&candidates, 100, HASH)
                .iter()
                .map(|candidate| candidate.source_id.as_str())
                .collect::<Vec<_>>(),
            vec!["a", "c", "z", "b", "0"]
        );
    }

    #[test]
    fn lista_vacia_es_valida_y_lista_larga_se_ordena_y_limita_a_dieciséis() {
        let unknown_selected = DownloadArtifactEstimate {
            size_estimate: larm_domain::model::ArtifactSizeEstimate::unknown(
                "unknown",
                "fixture:unknown",
            ),
            sha256: None,
            ..exact_download_artifact()
        };
        assert_eq!(
            order_source_candidates(vec![], &[unknown_selected], None).unwrap(),
            Vec::<DownloadSourceCandidate>::new()
        );

        let candidates = (0..18)
            .map(|index| source_candidate(index + 1, index))
            .collect();
        let ordered =
            order_source_candidates(candidates, &[exact_download_artifact()], None).unwrap();
        assert_eq!(ordered.len(), 16);
        assert_eq!(ordered.first().unwrap().source_priority, 17);
        assert_eq!(ordered.last().unwrap().source_priority, 2);
    }
}
