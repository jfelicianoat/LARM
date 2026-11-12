//! Planificador de destinos y admisión (P015; spec §30, §45, §137).
//!
//! Para una operación descrita en fases, con los ficheros nuevos marcados
//! con el volumen comodín [`DESTINATION`], evalúa **todas** las ubicaciones y
//! explica por qué cada una sirve o no. Elegida una, la admite reservando
//! el espacio por volumen físico en una sola transacción.

use larm_domain::error::{ErrorCode, LarmError};
use larm_domain::estimate::{
    estimate, Confidence, EstimateError, HardBudget, MarginPolicy, Phase, StorageEstimate,
    VolumeSnapshot,
};
use larm_domain::locations::{LocationState, StorageLocation, UNKNOWN_VOLUME_PREFIX};
use larm_domain::ports::{VolumeDemand, VolumeReservationRepository};
use larm_domain::storage::StorageObservation;

/// Volumen comodín: «el de la ubicación de destino».
pub const DESTINATION: &str = "$destination";

const FAT32_MAX_FILE: u64 = (1 << 32) - 1;

/// Parámetros de la política de espacio (`Settings.storage`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpacePolicy {
    pub margin: MarginPolicy,
    /// `minimum_free_bytes` global; el mínimo de un volumen es el mayor entre
    /// este y las reservas de sus raíces.
    pub minimum_free_bytes: u64,
}

/// Una ubicación evaluada como destino.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct DestinationOption {
    pub location_id: String,
    pub name: String,
    pub volume_key: String,
    pub eligible: bool,
    /// `fits`, `state_<estado>`, `volume_identity_unknown`, `fat32_file_too_large`,
    /// `size_unknown` o `insufficient_space: …`.
    pub reason: String,
    pub estimate: Option<StorageEstimate>,
}

fn overflow(e: EstimateError) -> LarmError {
    match e {
        EstimateError::Overflow(d) => LarmError::new(
            ErrorCode::ValidationFailed,
            "Los tamaños de la operación desbordan",
            "size_overflow",
        )
        .with_technical(d),
        EstimateError::MissingVolume(v) => LarmError::new(
            ErrorCode::ResourceUnavailable,
            "Un volumen de la operación no está disponible",
            "volume_unavailable",
        )
        .with_resource(v),
    }
}

/// Fases con el comodín sustituido por el volumen de la ubicación.
#[must_use]
pub fn bind_destination(template: &[Phase], volume_key: &str) -> Vec<Phase> {
    template
        .iter()
        .map(|p| Phase {
            name: p.name.clone(),
            items: p
                .items
                .iter()
                .map(|i| {
                    let mut i = i.clone();
                    if i.volume_key == DESTINATION {
                        i.volume_key = volume_key.to_string();
                    }
                    i
                })
                .collect(),
        })
        .collect()
}

/// Mínimo de política de un volumen: el mayor entre el global y las
/// reservas de sus raíces (no la suma).
#[must_use]
pub fn policy_minimum(volume_key: &str, locations: &[StorageLocation], global: u64) -> u64 {
    locations
        .iter()
        .filter(|l| l.volume_key == volume_key)
        .map(|l| l.reserved_bytes.get())
        .chain(std::iter::once(global))
        .max()
        .unwrap_or(global)
}

/// Estado de todos los volúmenes observados para el cálculo.
///
/// # Errors
/// Fallo del repositorio de reservas.
pub async fn snapshots(
    observation: &StorageObservation,
    locations: &[StorageLocation],
    reservations: &dyn VolumeReservationRepository,
    policy: SpacePolicy,
    exclude_job: Option<&str>,
) -> Result<Vec<VolumeSnapshot>, LarmError> {
    let mut out = Vec::new();
    for v in observation.volumes.iter().filter(|v| v.online) {
        let (Some(key), Some(free)) = (v.identity.volume_key.as_ref(), v.free_bytes) else {
            continue;
        };
        out.push(VolumeSnapshot {
            volume_key: key.clone(),
            os_free: free.get(),
            policy_minimum: policy_minimum(key, locations, policy.minimum_free_bytes),
            other_reservations: reservations.others(key, exclude_job).await?,
        });
    }
    Ok(out)
}

/// Evalúa cada ubicación como destino de la operación.
///
/// # Errors
/// Desbordamiento de tamaños.
pub fn plan_destinations(
    template: &[Phase],
    locations: &[StorageLocation],
    volumes: &[VolumeSnapshot],
    policy: SpacePolicy,
    budget: Option<HardBudget>,
) -> Result<Vec<DestinationOption>, LarmError> {
    let largest = template
        .iter()
        .flat_map(|p| p.items.iter())
        .filter(|i| i.volume_key == DESTINATION)
        .filter_map(|i| i.size.upper_bound.or(i.size.bytes))
        .max()
        .unwrap_or(0);
    let mut options = Vec::new();
    for l in locations {
        let option =
            |eligible: bool, reason: String, estimate: Option<StorageEstimate>| DestinationOption {
                location_id: l.id.clone(),
                name: l.name.clone(),
                volume_key: l.volume_key.clone(),
                eligible,
                reason,
                estimate,
            };
        if l.state != LocationState::Online {
            options.push(option(
                false,
                format!("state_{}", l.state.as_str().to_lowercase()),
                None,
            ));
            continue;
        }
        if l.volume_key.starts_with(UNKNOWN_VOLUME_PREFIX) {
            options.push(option(false, "volume_identity_unknown".into(), None));
            continue;
        }
        if l.filesystem.eq_ignore_ascii_case("FAT32") && largest > FAT32_MAX_FILE {
            options.push(option(false, "fat32_file_too_large".into(), None));
            continue;
        }
        let phases = bind_destination(template, &l.volume_key);
        match estimate(&phases, volumes, policy.margin, budget) {
            Ok(e) if e.confidence == Confidence::Unknown && e.required_free_bytes.is_none() => {
                options.push(option(false, "size_unknown".into(), Some(e)));
            }
            Ok(e) => {
                let eligible = e.per_volume.iter().all(|v| v.eligible);
                let reason = if eligible {
                    "fits".to_string()
                } else {
                    e.per_volume
                        .iter()
                        .find(|v| !v.eligible)
                        .map_or_else(|| "insufficient_space".into(), |v| v.reason.clone())
                };
                options.push(option(eligible, reason, Some(e)));
            }
            Err(EstimateError::MissingVolume(v)) => {
                options.push(option(false, format!("volume_unavailable: {v}"), None));
            }
            Err(e) => return Err(overflow(e)),
        }
    }
    // Primero las que sirven; dentro, más espacio utilizable antes.
    let usable = |o: &DestinationOption| {
        o.estimate
            .as_ref()
            .and_then(|e| e.per_volume.iter().find(|v| v.volume_key == o.volume_key))
            .map_or(0, |v| v.usable_free_bytes.get())
    };
    options.sort_by(|a, b| {
        b.eligible
            .cmp(&a.eligible)
            .then_with(|| usable(b).cmp(&usable(a)))
            .then_with(|| a.location_id.cmp(&b.location_id))
    });
    Ok(options)
}

/// Admite la operación en el destino elegido: recalcula con las reservas
/// vigentes y reserva cada volumen con espacio nuevo, todo o nada.
///
/// # Errors
/// `STORAGE_INSUFFICIENT`, `UNKNOWN_SIZE`, instantánea vieja o desbordamiento.
pub async fn admit(
    job_id: &str,
    estimate: &StorageEstimate,
    volumes: &[VolumeSnapshot],
    snapshot_at: chrono::DateTime<chrono::Utc>,
    reservations: &dyn VolumeReservationRepository,
) -> Result<(), LarmError> {
    if estimate.required_free_bytes.is_none() {
        return Err(LarmError::new(
            ErrorCode::UnknownSize,
            "El tamaño es desconocido: hace falta una cota o un presupuesto duro",
            "size_unknown",
        ));
    }
    let mut demands = Vec::new();
    for v in &estimate.per_volume {
        let snapshot = volumes
            .iter()
            .find(|s| s.volume_key == v.volume_key)
            .ok_or_else(|| overflow(EstimateError::MissingVolume(v.volume_key.clone())))?;
        let required = v
            .peak_extra_bytes
            .get()
            .checked_add(v.margin_bytes.get())
            .ok_or_else(|| overflow(EstimateError::Overflow("requerido".into())))?;
        demands.push(VolumeDemand {
            volume_key: v.volume_key.clone(),
            required_bytes: required,
            os_free: snapshot.os_free,
            policy_minimum: snapshot.policy_minimum,
        });
    }
    reservations
        .admit(job_id, &demands, snapshot_at, None)
        .await
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use larm_domain::estimate::{ItemKind, PhaseItem, Size};
    use larm_domain::storage::PerformanceClass;
    use larm_domain::units::DecimalU64;

    const GB: u64 = 1_000_000_000;

    fn loc(id: &str, key: &str, state: LocationState, fs: &str, reserved: u64) -> StorageLocation {
        StorageLocation {
            id: id.into(),
            created_at: "t".into(),
            updated_at: "t".into(),
            row_version: 1,
            metadata: serde_json::Map::new(),
            name: id.into(),
            root_path: format!("{id}:\\"),
            canonical_path: format!("{id}:\\"),
            volume_guid: Some(key.into()),
            drive_letter: None,
            unc_path: None,
            filesystem: fs.into(),
            storage_type: "LOCAL".into(),
            capacity_bytes: DecimalU64::new(1000 * GB).unwrap(),
            free_bytes: DecimalU64::new(100 * GB).unwrap(),
            reserved_bytes: DecimalU64::new(reserved).unwrap(),
            priority: 0,
            auto_placement: true,
            state,
            last_seen_at: None,
            volume_key: key.into(),
            performance_class: PerformanceClass::Ssd,
        }
    }

    fn download(bytes: u64) -> Vec<Phase> {
        vec![Phase {
            name: "descargar".into(),
            items: vec![PhaseItem {
                name: "m.gguf".into(),
                kind: ItemKind::Download,
                volume_key: DESTINATION.into(),
                size: Size::exact(bytes),
                existing: false,
            }],
        }]
    }

    fn vol(key: &str, free: u64, minimum: u64) -> VolumeSnapshot {
        VolumeSnapshot {
            volume_key: key.into(),
            os_free: free,
            policy_minimum: minimum,
            other_reservations: 0,
        }
    }

    const POLICY: SpacePolicy = SpacePolicy {
        margin: MarginPolicy {
            minimum_margin_bytes: 1 << 30,
            margin_basis_points: 500,
        },
        minimum_free_bytes: 2 << 30,
    };

    #[test]
    fn explica_cada_ubicacion() {
        let locations = vec![
            loc("d", "vd", LocationState::Online, "NTFS", 0),
            loc("e", "ve", LocationState::Online, "NTFS", 85 * GB),
            loc("f", "vf", LocationState::Offline, "NTFS", 0),
            loc("g", "unknown:g:\\", LocationState::Online, "NTFS", 0),
            loc("h", "vh", LocationState::Online, "FAT32", 0),
            loc("i", "vi", LocationState::LowSpace, "NTFS", 0),
        ];
        let volumes = vec![
            vol("vd", 100 * GB, 2 << 30),
            vol("ve", 100 * GB, 85 * GB),
            vol("vh", 900 * GB, 0),
        ];
        let options =
            plan_destinations(&download(18 * GB), &locations, &volumes, POLICY, None).unwrap();
        let reason = |id: &str| {
            options
                .iter()
                .find(|o| o.location_id == id)
                .unwrap()
                .reason
                .clone()
        };
        assert_eq!(options[0].location_id, "d", "la única que sirve, primero");
        assert!(options[0].eligible);
        assert!(
            reason("e").starts_with("insufficient_space"),
            "reserva respetada: {}",
            reason("e")
        );
        assert_eq!(reason("f"), "state_offline");
        assert_eq!(reason("i"), "state_low_space");
        assert_eq!(reason("g"), "volume_identity_unknown");
        assert_eq!(reason("h"), "fat32_file_too_large");
        // Determinista.
        assert_eq!(
            plan_destinations(&download(18 * GB), &locations, &volumes, POLICY, None).unwrap(),
            options
        );
    }

    #[test]
    fn minimo_de_politica_es_el_mayor_no_la_suma() {
        let locations = vec![
            loc("a", "v", LocationState::Online, "NTFS", 5 * GB),
            loc("b", "v", LocationState::Online, "NTFS", 3 * GB),
        ];
        assert_eq!(policy_minimum("v", &locations, 2 << 30), 5 * GB);
        assert_eq!(policy_minimum("otro", &locations, 2 << 30), 2 << 30);
    }

    #[test]
    fn tamano_desconocido_no_es_elegible_sin_presupuesto() {
        let phases = vec![Phase {
            name: "descargar".into(),
            items: vec![PhaseItem {
                name: "x".into(),
                kind: ItemKind::Download,
                volume_key: DESTINATION.into(),
                size: Size::unknown(),
                existing: false,
            }],
        }];
        let locations = vec![loc("d", "vd", LocationState::Online, "NTFS", 0)];
        let volumes = vec![vol("vd", 100 * GB, 0)];
        let options = plan_destinations(&phases, &locations, &volumes, POLICY, None).unwrap();
        assert_eq!(
            (options[0].eligible, options[0].reason.as_str()),
            (false, "size_unknown")
        );
        let options = plan_destinations(
            &phases,
            &locations,
            &volumes,
            POLICY,
            Some(HardBudget(10 * GB)),
        )
        .unwrap();
        assert!(options[0].eligible);
    }
}
