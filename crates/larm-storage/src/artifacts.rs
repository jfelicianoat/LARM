//! Plan de movimiento de una variante (P016; spec §37, contrato 02).
//!
//! Funciones puras: deciden qué hace el movimiento con cada artefacto, qué
//! locks toma, cuánto espacio nuevo necesita y el hash del plan. El hash
//! cubre cada fila con su `row_version`, así que un plan aprobado deja de
//! valer en cuanto cambia algo de lo que describe.

use larm_domain::artifacts::{MoveAction, MoveItem, Ownership, MOVE_OPERATION};
use larm_domain::error::{ErrorCode, LarmError};
use larm_domain::estimate::{ItemKind, Phase, PhaseItem, Size};
use larm_domain::guard::{normalize, LockMode, Resource, ResourceKind};
use larm_domain::locations::{LocationState, StorageLocation, UNKNOWN_VOLUME_PREFIX};
use larm_domain::ports::VariantArtifact;

fn unavailable(reason: &str, message: &str) -> LarmError {
    LarmError::new(ErrorCode::ResourceUnavailable, message, reason)
}

/// Comprueba que la ubicación puede recibir el modelo.
///
/// # Errors
/// `RESOURCE_UNAVAILABLE` si no está en línea o su volumen no se identificó.
pub fn check_target(target: &StorageLocation) -> Result<(), LarmError> {
    if target.state != LocationState::Online {
        return Err(unavailable(
            &format!("target_state_{}", target.state.as_str().to_lowercase()),
            "La ubicación de destino no está disponible",
        )
        .with_resource(target.id.clone()));
    }
    if target.volume_key.starts_with(UNKNOWN_VOLUME_PREFIX) {
        return Err(unavailable(
            "volume_identity_unknown",
            "No se pudo identificar el volumen de destino",
        )
        .with_resource(target.id.clone()));
    }
    Ok(())
}

/// Decide la acción de cada ubicación de la variante.
///
/// - `EXTERNAL`, `HOST_MANAGED` y los blobs compartidos (propiedad `SHARED`
///   o usados por más de una variante) se quedan donde están.
/// - Lo que ya está en el destino, también.
/// - El resto: renombrado si origen y destino están en el mismo volumen;
///   copia verificada si no. Debe estar `ONLINE` y su origen debe admitir
///   el borrado (ni `READ_ONLY` ni desconectado).
///
/// # Errors
/// Variante sin artefactos o sin nada que mover, un artefacto no disponible
/// o una ubicación de origen desconocida o no utilizable.
pub fn plan_items(
    target: &StorageLocation,
    artifacts: &[VariantArtifact],
    locations: &[StorageLocation],
) -> Result<Vec<MoveItem>, LarmError> {
    check_target(target)?;
    if artifacts.is_empty() {
        return Err(LarmError::new(
            ErrorCode::InvalidStateTransition,
            "La variante no tiene artefactos en disco",
            "variant_has_no_artifacts",
        ));
    }
    let mut items = Vec::with_capacity(artifacts.len());
    for a in artifacts {
        let l = &a.location;
        let keep = match l.ownership {
            Ownership::External => Some("external_preserved"),
            Ownership::HostManaged => Some("host_managed_preserved"),
            Ownership::Shared => Some("shared_preserved"),
            Ownership::LarmManaged if a.referenced_by > 1 => Some("shared_preserved"),
            Ownership::LarmManaged if l.storage_location_id == target.id => Some("already_there"),
            Ownership::LarmManaged => None,
        };
        let action = if keep.is_some() {
            MoveAction::Keep
        } else {
            if l.state != larm_domain::artifacts::ArtifactState::Online {
                return Err(unavailable(
                    &format!("artifact_{}", l.state.as_str().to_lowercase()),
                    "Un artefacto del modelo no está disponible",
                )
                .with_resource(l.id.clone()));
            }
            let source = locations
                .iter()
                .find(|s| s.id == l.storage_location_id)
                .ok_or_else(|| {
                    unavailable("source_location_missing", "Ubicación de origen desconocida")
                        .with_resource(l.storage_location_id.clone())
                })?;
            if !matches!(
                source.state,
                LocationState::Online | LocationState::LowSpace
            ) {
                return Err(unavailable(
                    &format!("source_state_{}", source.state.as_str().to_lowercase()),
                    "La ubicación de origen no admite el movimiento",
                )
                .with_resource(source.id.clone()));
            }
            if source.volume_key == target.volume_key
                && !source.volume_key.starts_with(UNKNOWN_VOLUME_PREFIX)
            {
                MoveAction::Rename
            } else {
                MoveAction::Copy
            }
        };
        items.push(MoveItem {
            artifact_location_id: l.id.clone(),
            artifact_id: l.artifact_id.clone(),
            row_version: l.row_version,
            action,
            reason: keep.map(String::from),
            from_storage_location_id: l.storage_location_id.clone(),
            to_storage_location_id: if action == MoveAction::Keep {
                l.storage_location_id.clone()
            } else {
                target.id.clone()
            },
            relative_path: l.relative_path.clone(),
            file_size: l.file_size,
            sha256: l.sha256.clone(),
        });
    }
    if items.iter().all(|i| i.action == MoveAction::Keep) {
        return Err(LarmError::new(
            ErrorCode::InvalidStateTransition,
            "No hay nada que mover: todo está ya en el destino o se conserva en su sitio",
            "nothing_to_move",
        ));
    }
    Ok(items)
}

/// Locks del movimiento en orden global: volúmenes de origen y destino
/// compartidos, variante y artefactos movidos en exclusiva.
#[must_use]
pub fn lock_set(
    variant_id: &str,
    target: &StorageLocation,
    items: &[MoveItem],
    locations: &[StorageLocation],
) -> Vec<(Resource, LockMode)> {
    let mut requests = vec![
        (
            Resource::new(ResourceKind::Volume, target.volume_key.clone()),
            LockMode::Shared,
        ),
        (
            Resource::new(ResourceKind::Variant, variant_id),
            LockMode::Exclusive,
        ),
    ];
    for i in items.iter().filter(|i| i.action != MoveAction::Keep) {
        if let Some(source) = locations
            .iter()
            .find(|s| s.id == i.from_storage_location_id)
        {
            requests.push((
                Resource::new(ResourceKind::Volume, source.volume_key.clone()),
                LockMode::Shared,
            ));
        }
        requests.push((
            Resource::new(ResourceKind::Artifact, i.artifact_id.clone()),
            LockMode::Exclusive,
        ));
    }
    normalize(&requests)
}

/// Fases de espacio para P015: solo las copias asignan bytes nuevos, en el
/// volumen de destino; un renombrado no ocupa nada más.
#[must_use]
pub fn space_phases(target: &StorageLocation, items: &[MoveItem]) -> Vec<Phase> {
    vec![Phase {
        name: "COPYING".into(),
        items: items
            .iter()
            .filter(|i| i.action == MoveAction::Copy)
            .map(|i| PhaseItem {
                name: i.artifact_id.clone(),
                kind: ItemKind::FinalArtifact,
                volume_key: target.volume_key.clone(),
                size: i
                    .file_size
                    .map_or_else(Size::unknown, |s| Size::exact(s.get())),
                existing: false,
            })
            .collect(),
    }]
}

/// Hash del plan: RFC 8785 de la operación, la variante, el destino y cada
/// decisión con su `row_version`.
///
/// # Errors
/// Fallo de canonicalización (no ocurre con estos datos).
pub fn plan_hash(
    variant_id: &str,
    target: &StorageLocation,
    items: &[MoveItem],
) -> Result<String, LarmError> {
    let value = serde_json::json!({
        "operation": MOVE_OPERATION,
        "variant_id": variant_id,
        "target_storage_location_id": target.id,
        "target_volume_key": target.volume_key,
        "items": items,
    });
    larm_domain::canonical::sha256_hex(&value).map_err(|e| {
        LarmError::new(ErrorCode::Internal, "Plan no canonicalizable", "plan_hash")
            .with_technical(e)
    })
}

/// Pasos visibles del plan (sin rutas absolutas).
#[must_use]
pub fn plan_steps(items: &[MoveItem]) -> Vec<larm_domain::artifacts::PlanStep> {
    items
        .iter()
        .map(|i| {
            let mut request = serde_json::Map::new();
            request.insert(
                "artifact_location_id".into(),
                i.artifact_location_id.clone().into(),
            );
            request.insert("artifact_id".into(), i.artifact_id.clone().into());
            request.insert(
                "from_storage_location_id".into(),
                i.from_storage_location_id.clone().into(),
            );
            request.insert(
                "to_storage_location_id".into(),
                i.to_storage_location_id.clone().into(),
            );
            request.insert("relative_path".into(), i.relative_path.clone().into());
            request.insert(
                "file_size".into(),
                i.file_size
                    .map_or(serde_json::Value::Null, |s| s.get().to_string().into()),
            );
            request.insert(
                "reason".into(),
                i.reason.clone().map_or(serde_json::Value::Null, Into::into),
            );
            larm_domain::artifacts::PlanStep {
                phase: match i.action {
                    MoveAction::Rename => "RENAME",
                    MoveAction::Copy => "COPY",
                    MoveAction::Keep => "KEEP",
                }
                .into(),
                adapter_id: None,
                request,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use larm_domain::artifacts::{ArtifactLocation, ArtifactState};
    use larm_domain::storage::PerformanceClass;
    use larm_domain::units::DecimalU64;

    pub(crate) fn location(id: &str, volume: &str, state: LocationState) -> StorageLocation {
        StorageLocation {
            id: id.into(),
            created_at: "2026-10-11T00:00:00.000Z".into(),
            updated_at: "2026-10-11T00:00:00.000Z".into(),
            row_version: 1,
            metadata: serde_json::Map::new(),
            name: id.into(),
            root_path: format!(r"C:\{id}"),
            canonical_path: format!(r"C:\{id}"),
            volume_guid: None,
            drive_letter: None,
            unc_path: None,
            filesystem: "NTFS".into(),
            storage_type: "LOCAL".into(),
            capacity_bytes: DecimalU64::default(),
            free_bytes: DecimalU64::default(),
            reserved_bytes: DecimalU64::default(),
            priority: 0,
            auto_placement: true,
            state,
            last_seen_at: None,
            volume_key: volume.into(),
            performance_class: PerformanceClass::Ssd,
        }
    }

    fn artifact(id: &str, at: &str, ownership: Ownership, refs: u64) -> VariantArtifact {
        VariantArtifact {
            location: ArtifactLocation {
                id: format!("loc-{id}"),
                created_at: String::new(),
                updated_at: String::new(),
                row_version: 3,
                metadata: serde_json::Map::new(),
                artifact_id: id.into(),
                storage_location_id: at.into(),
                relative_path: format!("m/{id}.gguf"),
                file_size: DecimalU64::new(10),
                sha256: None,
                state: ArtifactState::Online,
                verified_at: None,
                ownership,
            },
            referenced_by: refs,
        }
    }

    #[test]
    fn decisiones_por_artefacto() {
        let src = location("src", "vol-a", LocationState::Online);
        let same = location("same", "vol-a", LocationState::Online);
        let other = location("other", "vol-b", LocationState::Online);
        let all = [src.clone(), same.clone(), other.clone()];
        let arts = [
            artifact("w", "src", Ownership::LarmManaged, 1),
            artifact("x", "src", Ownership::External, 1),
            artifact("h", "src", Ownership::HostManaged, 1),
            artifact("s", "src", Ownership::LarmManaged, 2),
            artifact("t", "src", Ownership::Shared, 1),
        ];
        let items = plan_items(&other, &arts, &all).unwrap();
        let by = |id: &str| items.iter().find(|i| i.artifact_id == id).unwrap();
        assert_eq!(by("w").action, MoveAction::Copy);
        assert_eq!(by("w").to_storage_location_id, "other");
        assert_eq!(by("x").reason.as_deref(), Some("external_preserved"));
        assert_eq!(
            by("x").to_storage_location_id,
            "src",
            "lo conservado no cambia de sitio"
        );
        assert_eq!(by("h").reason.as_deref(), Some("host_managed_preserved"));
        assert_eq!(by("s").reason.as_deref(), Some("shared_preserved"));
        assert_eq!(by("t").reason.as_deref(), Some("shared_preserved"));
        // Mismo volumen: renombrado.
        let items = plan_items(&same, &arts, &all).unwrap();
        assert_eq!(items[0].action, MoveAction::Rename);
        // Locks: volúmenes compartidos, variante y artefacto movido exclusivos.
        let locks = lock_set(
            "v1",
            &other,
            &plan_items(&other, &arts, &all).unwrap(),
            &all,
        );
        let keys: Vec<(String, LockMode)> = locks.iter().map(|(r, m)| (r.key(), *m)).collect();
        assert_eq!(
            keys,
            vec![
                ("volume:vol-a".into(), LockMode::Shared),
                ("volume:vol-b".into(), LockMode::Shared),
                ("variant:v1".into(), LockMode::Exclusive),
                ("artifact:w".into(), LockMode::Exclusive),
            ]
        );
        // Espacio: solo la copia, en el destino.
        let phases = space_phases(&other, &plan_items(&other, &arts, &all).unwrap());
        assert_eq!(phases[0].items.len(), 1);
        assert_eq!(phases[0].items[0].volume_key, "vol-b");
        assert!(
            space_phases(&same, &plan_items(&same, &arts, &all).unwrap())[0]
                .items
                .is_empty()
        );
    }

    #[test]
    fn negativos_del_plan() {
        let src = location("src", "vol-a", LocationState::Online);
        let all = [src.clone(), location("dst", "vol-b", LocationState::Online)];
        let reason = |r: Result<Vec<MoveItem>, LarmError>| r.unwrap_err().details.reason;
        let offline = location("dst", "vol-b", LocationState::Offline);
        assert_eq!(
            reason(plan_items(
                &offline,
                &[artifact("w", "src", Ownership::LarmManaged, 1)],
                &all
            )),
            "target_state_offline"
        );
        let unknown = location("dst", "unknown:c:\\x", LocationState::Online);
        assert_eq!(
            reason(plan_items(
                &unknown,
                &[artifact("w", "src", Ownership::LarmManaged, 1)],
                &all
            )),
            "volume_identity_unknown"
        );
        let target = &all[1];
        assert_eq!(
            reason(plan_items(target, &[], &all)),
            "variant_has_no_artifacts"
        );
        assert_eq!(
            reason(plan_items(
                target,
                &[artifact("x", "src", Ownership::External, 1)],
                &all
            )),
            "nothing_to_move"
        );
        assert_eq!(
            reason(plan_items(
                target,
                &[artifact("w", "dst", Ownership::LarmManaged, 1)],
                &all
            )),
            "nothing_to_move"
        );
        let mut missing = artifact("w", "src", Ownership::LarmManaged, 1);
        missing.location.state = ArtifactState::Missing;
        assert_eq!(
            reason(plan_items(target, &[missing], &all)),
            "artifact_missing"
        );
        let ro = [
            location("src", "vol-a", LocationState::ReadOnly),
            all[1].clone(),
        ];
        assert_eq!(
            reason(plan_items(
                target,
                &[artifact("w", "src", Ownership::LarmManaged, 1)],
                &ro
            )),
            "source_state_read_only"
        );
        assert_eq!(
            reason(plan_items(
                target,
                &[artifact("w", "nadie", Ownership::LarmManaged, 1)],
                &all
            )),
            "source_location_missing"
        );
    }

    #[test]
    fn hash_determinista_y_sensible_a_la_version() {
        let all = [
            location("src", "vol-a", LocationState::Online),
            location("dst", "vol-b", LocationState::Online),
        ];
        let arts = [artifact("w", "src", Ownership::LarmManaged, 1)];
        let items = plan_items(&all[1], &arts, &all).unwrap();
        let h = plan_hash("v1", &all[1], &items).unwrap();
        assert_eq!(h, plan_hash("v1", &all[1], &items).unwrap());
        assert_eq!(h.len(), 64);
        let mut bumped = arts.clone();
        bumped[0].location.row_version += 1;
        let items2 = plan_items(&all[1], &bumped, &all).unwrap();
        assert_ne!(h, plan_hash("v1", &all[1], &items2).unwrap());
        assert_ne!(h, plan_hash("v2", &all[1], &items).unwrap());
        // Pasos sin rutas absolutas.
        let steps = serde_json::to_string(&plan_steps(&items)).unwrap();
        assert!(!steps.contains(":\\\\"), "{steps}");
        assert!(steps.contains("\"COPY\""));
    }
}
