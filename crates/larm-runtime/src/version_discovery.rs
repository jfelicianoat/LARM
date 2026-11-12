//! P031: recogida acotada, sin persistir páginas parciales ni promover versiones.
//! El host concreto debe conservar sus guards y cancelar el RPC al interrumpir.
use larm_domain::error::{ErrorCode, LarmError};
use larm_domain::runtime::{Runtime, RuntimeChannel, RuntimeVersion};
use larm_domain::runtime_install::{RuntimeVersionSource, VersionQuery};
use std::collections::BTreeSet;
use std::num::NonZeroU32;

#[derive(Debug, PartialEq)]
pub struct DiscoveryBatch {
    /// Inventario completo devuelto para los canales consultados, incluido UNKNOWN.
    pub versions: Vec<RuntimeVersion>,
    /// Cada página conserva el snapshot declarado; no se inventa un snapshot global.
    pub page_snapshots: Vec<(RuntimeChannel, String)>,
}

/// El presupuesto de páginas lo proporciona el propietario del trabajo. Agotarlo
/// devuelve error sin un batch parcial; no se etiqueta una lista truncada completa.
pub async fn collect(
    source: &dyn RuntimeVersionSource,
    runtime: &Runtime,
    max_pages: NonZeroU32,
) -> Result<DiscoveryBatch, LarmError> {
    runtime.validate()?;
    let mut batch = DiscoveryBatch {
        versions: Vec::new(),
        page_snapshots: Vec::new(),
    };
    let mut remaining = max_pages.get();
    let mut ids = BTreeSet::new();
    let channels: BTreeSet<_> = runtime
        .channel_policy
        .allowed_channels
        .iter()
        .copied()
        .collect();
    for channel in channels {
        let mut query = VersionQuery {
            runtime_id: runtime.id.clone(),
            channel,
            cursor: None,
        };
        let mut cursors = BTreeSet::new();
        loop {
            if remaining == 0 {
                return Err(LarmError::new(
                    ErrorCode::ResourceUnavailable,
                    "El descubrimiento superó su presupuesto de páginas",
                    "runtime_discovery_page_budget_exhausted",
                ));
            }
            remaining -= 1;
            let page = source.list_versions(&query).await?;
            page.validate_for(&query)?;
            for mut version in page.items {
                version.normalize_observed_channel_metadata()?;
                if !ids.insert(version.id.clone()) {
                    return Err(LarmError::new(
                        ErrorCode::ProtocolError,
                        "El adaptador repitió una identidad de versión",
                        "runtime_discovery_duplicate_version",
                    ));
                }
                batch.versions.push(version);
            }
            batch.page_snapshots.push((channel, page.snapshot_at));
            let Some(next) = page.next_cursor else {
                break;
            };
            if !cursors.insert(next.clone()) {
                return Err(LarmError::new(
                    ErrorCode::ProtocolError,
                    "El adaptador repitió un cursor",
                    "runtime_discovery_cursor_repeated",
                ));
            }
            query.cursor = Some(next);
        }
    }
    Ok(batch)
}
