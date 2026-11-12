//! `larm-daemon storage-preflight` (P015): preflight de espacio de una
//! operación descrita en JSON contra las ubicaciones y el espacio reales, y
//! admisión opcional con reserva por volumen.

use std::path::Path;

use larm_core::config::Settings;
use larm_db::locations::SqlStorageLocations;
use larm_db::pool::Db;
use larm_db::reservations::SqlVolumeReservations;
use larm_domain::error::{ErrorCode, LarmError};
use larm_domain::estimate::{HardBudget, MarginPolicy, Phase};
use larm_domain::ports::{StorageLocationRepository, StorageProber};
use larm_storage::estimate::{admit, plan_destinations, snapshots, SpacePolicy};

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Operation {
    phases: Vec<Phase>,
    /// Presupuesto duro del operador para tamaños desconocidos.
    #[serde(default)]
    hard_budget_bytes: Option<larm_domain::units::DecimalU64>,
}

fn invalid(detail: String) -> LarmError {
    LarmError::new(
        ErrorCode::ValidationFailed,
        "Operación no válida",
        "schema_violation",
    )
    .with_technical(detail)
}

/// # Errors
/// Fichero ilegible, sondeo fallido, base o admisión rechazada.
pub async fn run(
    db: &Db,
    settings: &Settings,
    file: &Path,
    admit_to: Option<(&str, &str)>,
) -> Result<serde_json::Value, LarmError> {
    let text =
        std::fs::read_to_string(file).map_err(|e| invalid(format!("{}: {e}", file.display())))?;
    let op: Operation = serde_json::from_str(&text).map_err(|e| invalid(e.to_string()))?;
    let locations = SqlStorageLocations {
        pool: db.pool.clone(),
    }
    .all()
    .await?;
    let prober = crate::hardware::storage_prober().ok_or_else(|| {
        LarmError::new(
            ErrorCode::Internal,
            "Sin ruta del ejecutable",
            "storage_probe_unavailable",
        )
    })?;
    let mut unc: Vec<String> = locations
        .iter()
        .filter_map(|l| l.unc_path.clone())
        .collect();
    unc.sort();
    unc.dedup();
    let observation = prober.probe(&unc).await?;
    let snapshot_at = chrono::DateTime::parse_from_rfc3339(&observation.observed_at)
        .map(|d| d.with_timezone(&chrono::Utc))
        .unwrap_or_else(|_| chrono::Utc::now());
    let reservations = SqlVolumeReservations {
        pool: db.pool.clone(),
    };
    let policy = SpacePolicy {
        margin: MarginPolicy {
            minimum_margin_bytes: settings.storage.minimum_margin_bytes.get(),
            margin_basis_points: settings.storage.margin_basis_points,
        },
        minimum_free_bytes: settings.storage.minimum_free_bytes.get(),
    };
    let job = admit_to.map(|(_, j)| j);
    let volumes = snapshots(&observation, &locations, &reservations, policy, job).await?;
    let budget = op.hard_budget_bytes.map(|b| HardBudget(b.get()));
    let options = plan_destinations(&op.phases, &locations, &volumes, policy, budget)?;
    let mut admitted = serde_json::Value::Null;
    if let Some((location_id, job_id)) = admit_to {
        let option = options
            .iter()
            .find(|o| o.location_id == location_id)
            .ok_or_else(|| {
                LarmError::new(ErrorCode::NotFound, "Ubicación desconocida", "not_found")
                    .with_resource(location_id.to_string())
            })?;
        if !option.eligible {
            return Err(LarmError::new(
                ErrorCode::StorageInsufficient,
                "El destino no sirve para esta operación",
                "destination_not_eligible",
            )
            .with_technical(option.reason.clone()));
        }
        let estimate = option.estimate.as_ref().ok_or_else(|| {
            LarmError::new(ErrorCode::UnknownSize, "Sin estimate", "size_unknown")
        })?;
        admit(job_id, estimate, &volumes, snapshot_at, &reservations).await?;
        admitted = serde_json::json!({"job_id": job_id, "location_id": location_id});
    }
    Ok(serde_json::json!({
        "observed_at": observation.observed_at,
        "options": options,
        "admitted": admitted,
    }))
}
