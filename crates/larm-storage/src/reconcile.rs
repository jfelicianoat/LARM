//! Reconciliación de artefactos con el estado de sus volúmenes (P018; spec
//! §29, §135; contrato 01).
//!
//! - Ubicación `OFFLINE` (volumen desconectado): sus `artifact_location`
//!   pasan a `OFFLINE`. Ubicación `MISSING` (volumen presente, raíz
//!   inexistente): a `MISSING`. Nunca se borra un registro ni se toca la
//!   etapa del ciclo de vida; la disponibilidad de la variante pasa a
//!   `OFFLINE` sin guardar evidencia en metadata libre.
//! - Cuando la ubicación vuelve, cada fila no `ONLINE` se **reverifica
//!   leyendo los bytes**: mismo sha256 → `ONLINE` con `verified_at`; otro →
//!   `CORRUPT`; sin fichero → `MISSING`.
//! - Cuando los artefactos requeridos vuelven `ONLINE`, la disponibilidad
//!   pasa a `UNKNOWN` hasta recalcular gates, smoke y huella de ejecución. Un
//!   artefacto `CORRUPT` revoca disponibilidad.

use std::collections::BTreeSet;
use std::sync::Arc;

use larm_domain::artifacts::{join, resolve_root, ArtifactLocation, ArtifactState};
use larm_domain::error::LarmError;
use larm_domain::locations::{LocationState, StorageLocation};
use larm_domain::ports::{
    ArtifactLocationRepository, MutationContext, StorageLocationRepository, StorageProber,
    VariantRepository,
};
use serde::Serialize;

/// Puertos de la reconciliación.
#[derive(Clone)]
pub struct Reconciler {
    pub locations: Arc<dyn StorageLocationRepository>,
    pub artifacts: Arc<dyn ArtifactLocationRepository>,
    pub variants: Arc<dyn VariantRepository>,
    pub prober: Arc<dyn StorageProber>,
}

/// Qué cambió.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ReconcileReport {
    pub offline: usize,
    pub missing: usize,
    pub reverified: usize,
    pub corrupt: usize,
    pub variants_offline: usize,
    pub variants_restored: usize,
    pub variants_corrupt: usize,
}

const LEGACY_BEFORE: &str = "availability_before_offline";

fn hash(path: String) -> std::io::Result<Option<(String, u64)>> {
    match crate::cas::sha256_file(std::path::Path::new(&path)) {
        Ok(h) => Ok(Some(h)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

impl Reconciler {
    /// Recorre todas las ubicaciones y sus artefactos.
    ///
    /// # Errors
    /// Fallo de la base o del sondeo. Un fichero ilegible no detiene la
    /// reconciliación: la fila queda como estaba.
    pub async fn run(&self, ctx: &MutationContext) -> Result<ReconcileReport, LarmError> {
        let locations = self.locations.all().await?;
        let mut unc: Vec<String> = locations
            .iter()
            .filter_map(|l| l.unc_path.clone())
            .collect();
        unc.sort();
        unc.dedup();
        let observation = self.prober.probe(&unc).await?;
        let mut report = ReconcileReport::default();
        let mut touched = BTreeSet::new();
        for location in &locations {
            let rows = self.artifacts.by_location(&location.id).await?;
            if rows.is_empty() {
                continue;
            }
            let changed = match location.state {
                LocationState::Offline | LocationState::Error => {
                    self.mark(&rows, ArtifactState::Offline, &mut report.offline, ctx)
                        .await?
                }
                LocationState::Missing => {
                    self.mark(&rows, ArtifactState::Missing, &mut report.missing, ctx)
                        .await?
                }
                _ => match resolve_root(location, &observation.volumes) {
                    Ok(root) => {
                        self.reverify(location, &root, &rows, &mut report, ctx)
                            .await?
                    }
                    // Volumen sin montar aunque la fila diga otra cosa: no se toca.
                    Err(_) => false,
                },
            };
            if changed {
                touched.extend(self.artifacts.variants_at(&location.id).await?);
            }
        }
        for variant in touched {
            self.variant(&variant, &mut report, ctx).await?;
        }
        Ok(report)
    }

    async fn mark(
        &self,
        rows: &[ArtifactLocation],
        state: ArtifactState,
        counter: &mut usize,
        ctx: &MutationContext,
    ) -> Result<bool, LarmError> {
        let mut changed = false;
        for row in rows.iter().filter(|r| r.state != state) {
            self.artifacts
                .set_state(&row.id, row.row_version, state, None, ctx)
                .await?;
            *counter += 1;
            changed = true;
        }
        Ok(changed)
    }

    async fn reverify(
        &self,
        _location: &StorageLocation,
        root: &str,
        rows: &[ArtifactLocation],
        report: &mut ReconcileReport,
        ctx: &MutationContext,
    ) -> Result<bool, LarmError> {
        let mut changed = false;
        for row in rows.iter().filter(|r| r.state != ArtifactState::Online) {
            let path = join(root, &row.relative_path);
            let observed = tokio::task::spawn_blocking(move || hash(path))
                .await
                .map_err(|e| {
                    LarmError::new(
                        larm_domain::error::ErrorCode::Internal,
                        "Reverificación interrumpida",
                        "join",
                    )
                    .with_technical(e.to_string())
                })?;
            let Ok(observed) = observed else { continue };
            let now = larm_domain::bridge::timestamp(chrono::Utc::now());
            let (state, verified) = match (observed, row.sha256.as_deref()) {
                (None, _) => (ArtifactState::Missing, None),
                (Some((sha, size)), Some(expected))
                    if sha == expected && row.file_size.is_none_or(|s| s.get() == size) =>
                {
                    (ArtifactState::Online, Some(now.as_str()))
                }
                // Sin hash registrado no se puede afirmar nada: desconocido.
                (Some(_), None) => (ArtifactState::Unknown, None),
                (Some(_), Some(_)) => (ArtifactState::Corrupt, None),
            };
            if state == row.state {
                continue;
            }
            self.artifacts
                .set_state(&row.id, row.row_version, state, verified, ctx)
                .await?;
            match state {
                ArtifactState::Online => report.reverified += 1,
                ArtifactState::Corrupt => report.corrupt += 1,
                ArtifactState::Missing => report.missing += 1,
                _ => {}
            }
            changed = true;
        }
        Ok(changed)
    }

    /// Disponibilidad de una variante según sus artefactos requeridos.
    async fn variant(
        &self,
        id: &str,
        report: &mut ReconcileReport,
        ctx: &MutationContext,
    ) -> Result<(), LarmError> {
        let Some(variant) = self.variants.get_variant(id).await? else {
            return Ok(());
        };
        let artifacts = self.variants.artifacts_of(id).await?;
        let rows = self.artifacts.for_variant(id).await?.unwrap_or_default();
        let required: Vec<&str> = artifacts
            .iter()
            .filter(|a| a.required)
            .map(|a| a.id.as_str())
            .collect();
        let states = |artifact: &str| -> Vec<ArtifactState> {
            rows.iter()
                .filter(|r| r.location.artifact_id == artifact)
                .map(|r| r.location.state)
                .collect()
        };
        let all_online = required
            .iter()
            .all(|a| states(a).contains(&ArtifactState::Online));
        let any_corrupt = required.iter().any(|a| {
            let s = states(a);
            !s.contains(&ArtifactState::Online) && s.contains(&ArtifactState::Corrupt)
        });
        let offline = variant.availability == "OFFLINE";
        if any_corrupt {
            if variant.lifecycle != "CORRUPT" || variant.availability != "CORRUPT" {
                report.variants_corrupt += 1;
                self.variants
                    .mark_variant_corrupt(id, variant.row_version, ctx)
                    .await?;
            }
            return Ok(());
        }
        if all_online {
            if variant.lifecycle == "CORRUPT" {
                report.variants_restored += 1;
                self.variants
                    .finish_variant_reverification(id, variant.row_version, ctx)
                    .await?;
                return Ok(());
            }
            if !offline && variant.availability != "CORRUPT" {
                return Ok(());
            }
            report.variants_restored += 1;
            return self
                .variants
                .set_availability(
                    id,
                    variant.row_version,
                    "UNKNOWN",
                    serde_json::json!({ LEGACY_BEFORE: null }),
                    ctx,
                )
                .await
                .map(|_| ());
        }
        if offline || variant.lifecycle == "CORRUPT" || variant.availability == "CORRUPT" {
            return Ok(());
        }
        let target = "OFFLINE";
        let patch = serde_json::json!({ LEGACY_BEFORE: null });
        if variant.availability == target {
            return Ok(());
        }
        report.variants_offline += 1;
        self.variants
            .set_availability(id, variant.row_version, target, patch, ctx)
            .await
            .map(|_| ())
    }
}
