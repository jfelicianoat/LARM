//! Ubicación de artefactos y movimiento entre discos (P016; spec §35–§38,
//! §82; contrato 02).
//!
//! - Un artefacto se localiza por `storage_location_id` + `relative_path`,
//!   nunca por una ruta absoluta persistida: la raíz actual se reconstruye
//!   desde la identidad del volumen (GUID) y sus puntos de montaje de ahora.
//! - Mover un modelo es un job con journal durable en las fases de
//!   [`MovePhase`]; el origen sigue siendo la copia válida hasta que el
//!   registro cambia y el destino se abre.
//! - `EXTERNAL`, `HOST_MANAGED` y los blobs compartidos no se mueven ni se
//!   borran: el plan los conserva en su sitio.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::error::{ErrorCode, LarmError};
use crate::estimate::StorageEstimate;
use crate::locations::StorageLocation;
use crate::storage::StorageVolume;
use crate::units::DecimalU64;

/// `kind` del job de movimiento en `operation_journal`.
pub const MOVE_JOB_KIND: &str = "artifact.move";
/// `operation` del plan y de su digest.
pub const MOVE_OPERATION: &str = "model.move";
/// Carpeta de copias provisionales bajo la raíz de destino (contrato 02:
/// `root/staging/job_id/artifact_id.partial`).
pub const STAGING_DIR: &str = "staging";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Ownership {
    LarmManaged,
    HostManaged,
    External,
    Shared,
}

impl Ownership {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::LarmManaged => "LARM_MANAGED",
            Self::HostManaged => "HOST_MANAGED",
            Self::External => "EXTERNAL",
            Self::Shared => "SHARED",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ArtifactState {
    Online,
    Offline,
    Corrupt,
    Missing,
    Copying,
    Unknown,
}

impl ArtifactState {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Online => "ONLINE",
            Self::Offline => "OFFLINE",
            Self::Corrupt => "CORRUPT",
            Self::Missing => "MISSING",
            Self::Copying => "COPYING",
            Self::Unknown => "UNKNOWN",
        }
    }
}

/// Fila `artifact_location` (schema `domain.schema.json#/$defs/artifact_location`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactLocation {
    pub id: String,
    pub created_at: String,
    pub updated_at: String,
    pub row_version: i64,
    pub metadata: Map<String, Value>,
    pub artifact_id: String,
    pub storage_location_id: String,
    pub relative_path: String,
    pub file_size: Option<DecimalU64>,
    pub sha256: Option<String>,
    pub state: ArtifactState,
    pub verified_at: Option<String>,
    pub ownership: Ownership,
}

fn invalid(reason: &str, message: &str) -> LarmError {
    LarmError::new(ErrorCode::ValidationFailed, message, reason)
}

/// Comprueba una ruta relativa a la raíz y la devuelve con `/`.
///
/// Rechaza vacías, absolutas, con unidad o `:` (flujos alternativos), con
/// componentes `.`/`..` o vacíos y nombres reservados de Windows.
///
/// # Errors
/// `VALIDATION_FAILED` `relative_path_invalid`.
pub fn normalize_relative(path: &str) -> Result<String, LarmError> {
    let bad = |detail: &str| {
        invalid(
            "relative_path_invalid",
            "La ruta del artefacto no es relativa a su ubicación",
        )
        .with_technical(detail.to_string())
    };
    if path.is_empty() {
        return Err(bad("vacía"));
    }
    let unified = path.replace('\\', "/");
    if unified.starts_with('/') || unified.contains(':') {
        return Err(bad("absoluta, con unidad o con flujo alternativo"));
    }
    let mut parts = Vec::new();
    for part in unified.split('/') {
        if part.is_empty() || part == "." || part == ".." {
            return Err(bad("componente vacío, . o .."));
        }
        if part.ends_with('.') || part.ends_with(' ') {
            return Err(bad("componente acabado en punto o espacio"));
        }
        if part
            .chars()
            .any(|c| c.is_control() || matches!(c, '<' | '>' | '"' | '|' | '?' | '*'))
        {
            return Err(bad("carácter no válido en Windows"));
        }
        let stem = part
            .split('.')
            .next()
            .unwrap_or_default()
            .to_ascii_uppercase();
        let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
            || (stem.len() == 4
                && (stem.starts_with("COM") || stem.starts_with("LPT"))
                && stem.as_bytes()[3].is_ascii_digit());
        if reserved {
            return Err(bad("nombre reservado de Windows"));
        }
        parts.push(part);
    }
    Ok(parts.join("/"))
}

/// Une una raíz absoluta y una ruta relativa ya normalizada (separador `\`).
#[must_use]
pub fn join(root: &str, relative: &str) -> String {
    let base = root.trim_end_matches(['\\', '/']);
    format!("{base}\\{}", relative.replace('/', "\\"))
}

fn lower_components(path: &str) -> Vec<String> {
    path.replace('/', "\\")
        .split('\\')
        .filter(|c| !c.is_empty())
        .map(str::to_lowercase)
        .collect()
}

/// Parte de `canonical` por debajo del punto de montaje `mount`, si lo está.
#[must_use]
pub fn below_mount(mount: &str, canonical: &str) -> Option<String> {
    let (m, c) = (lower_components(mount), lower_components(canonical));
    if m.len() > c.len() || !m.iter().zip(&c).all(|(a, b)| a == b) {
        return None;
    }
    let original: Vec<&str> = canonical
        .split(['\\', '/'])
        .filter(|c| !c.is_empty())
        .collect();
    Some(original[m.len()..].join("\\"))
}

/// Clave de `metadata` donde la ubicación guarda su ruta dentro del volumen.
pub const VOLUME_RELATIVE_KEY: &str = "volume_relative_path";

/// Por qué no se puede resolver la raíz de una ubicación ahora.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolveError {
    /// El volumen con ese GUID no está montado ni accesible.
    VolumeOffline,
}

/// Raíz actual de una ubicación.
///
/// - UNC: la ruta canónica (su identidad es el propio recurso compartido).
/// - Volumen local con GUID: si la ruta canónica sigue bajo uno de sus puntos
///   de montaje actuales, esa; si la letra cambió, la parte dentro del volumen
///   (`metadata.volume_relative_path` o, si falta, lo que sigue a `X:\`)
///   colgada del primer punto de montaje de ahora; sin puntos de montaje, la
///   ruta del GUID (`\\?\Volume{…}\`).
/// - Sin identidad verificada: la ruta canónica tal cual.
///
/// # Errors
/// [`ResolveError::VolumeOffline`] si el volumen del GUID no está en línea.
pub fn resolve_root(
    location: &StorageLocation,
    volumes: &[StorageVolume],
) -> Result<String, ResolveError> {
    if location.unc_path.is_some() {
        return Ok(location.canonical_path.clone());
    }
    let Some(guid) = location.volume_guid.as_deref() else {
        return Ok(location.canonical_path.clone());
    };
    let volume = volumes
        .iter()
        .find(|v| v.identity.volume_guid.as_deref() == Some(guid) && v.online)
        .ok_or(ResolveError::VolumeOffline)?;
    if volume
        .identity
        .mount_points
        .iter()
        .any(|m| below_mount(m, &location.canonical_path).is_some())
    {
        return Ok(location.canonical_path.clone());
    }
    let inside = location
        .metadata
        .get(VOLUME_RELATIVE_KEY)
        .and_then(Value::as_str)
        .map(String::from)
        .or_else(|| {
            let c = &location.canonical_path;
            let bytes = c.as_bytes();
            (bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic())
                .then(|| c[2..].trim_start_matches(['\\', '/']).to_string())
        })
        .unwrap_or_default();
    let base = volume
        .identity
        .mount_points
        .first()
        .cloned()
        .unwrap_or_else(|| format!("\\\\?\\Volume{{{guid}}}\\"));
    Ok(if inside.is_empty() {
        base
    } else {
        join(&base, &inside)
    })
}

/// Fases del journal de un movimiento (contrato 02), en orden.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum MovePhase {
    Planned,
    Locked,
    Copying,
    Verified,
    DbSwitched,
    AccessTested,
    SourceDeleted,
    Completed,
    Failed,
    Cancelled,
}

impl MovePhase {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Planned => "PLANNED",
            Self::Locked => "LOCKED",
            Self::Copying => "COPYING",
            Self::Verified => "VERIFIED",
            Self::DbSwitched => "DB_SWITCHED",
            Self::AccessTested => "ACCESS_TESTED",
            Self::SourceDeleted => "SOURCE_DELETED",
            Self::Completed => "COMPLETED",
            Self::Failed => "FAILED",
            Self::Cancelled => "CANCELLED",
        }
    }

    /// # Errors
    /// Texto que no es una fase de movimiento.
    pub fn parse(text: &str) -> Result<Self, String> {
        serde_json::from_value(Value::String(text.to_string()))
            .map_err(|_| format!("fase de movimiento desconocida: {text}"))
    }

    /// ¿El registro ya apunta al destino?
    #[must_use]
    pub fn switched(self) -> bool {
        matches!(
            self,
            Self::DbSwitched | Self::AccessTested | Self::SourceDeleted | Self::Completed
        )
    }

    #[must_use]
    pub fn terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }
}

/// Petición `POST /models/{id}/move` (schema `Move`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MoveRequest {
    pub target_storage_location_id: String,
    pub plan_hash: Option<String>,
    pub dry_run: bool,
    pub approval_id: Option<String>,
}

/// Paso de un plan (schema `Plan.steps[]`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanStep {
    pub phase: String,
    pub adapter_id: Option<String>,
    pub request: Map<String, Value>,
}

/// Respuesta `Plan` (schema cerrado de `larm.schema.json`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Plan {
    pub plan_id: String,
    pub plan_hash: String,
    pub operation: String,
    pub resource_ids: Vec<String>,
    pub storage_estimate: StorageEstimate,
    pub steps: Vec<PlanStep>,
    pub expires_at: String,
    pub approval_required: bool,
}

/// Qué hace el movimiento con un artefacto.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum MoveAction {
    /// Mismo volumen: renombrado atómico.
    Rename,
    /// Otro volumen: copia, flush, hash, cambio de registro, prueba de acceso
    /// y borrado del origen.
    Copy,
    /// Se queda donde está (`reason` dice por qué).
    Keep,
}

/// Decisión del plan para una `artifact_location` (sin rutas absolutas).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MoveItem {
    pub artifact_location_id: String,
    pub artifact_id: String,
    pub row_version: i64,
    pub action: MoveAction,
    /// `keep` explica `Keep`: `external_preserved`, `host_managed_preserved`,
    /// `shared_preserved` o `already_there`.
    pub reason: Option<String>,
    pub from_storage_location_id: String,
    pub to_storage_location_id: String,
    pub relative_path: String,
    pub file_size: Option<DecimalU64>,
    pub sha256: Option<String>,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use crate::locations::LocationState;
    use crate::storage::{DriveKind, PerformanceClass, VolumeIdentity};

    #[test]
    fn rutas_relativas() {
        assert_eq!(normalize_relative(r"a\b\c.gguf").unwrap(), "a/b/c.gguf");
        assert_eq!(normalize_relative("modelo.gguf").unwrap(), "modelo.gguf");
        for bad in [
            "",
            "/abs",
            r"\abs",
            "C:/x",
            "a/../b",
            "./a",
            "a//b",
            "a/b.",
            "a/b ",
            "a/CON.txt",
            "lpt1",
            "a:stream",
            "a/b?",
            "a\u{1}b",
        ] {
            let e = normalize_relative(bad).unwrap_err();
            assert_eq!(e.details.reason, "relative_path_invalid", "{bad:?}");
        }
        assert!(normalize_relative("console/com10.bin").is_ok());
        assert_eq!(join(r"D:\Modelos\", "a/b.gguf"), r"D:\Modelos\a\b.gguf");
    }

    #[test]
    fn por_debajo_del_montaje() {
        assert_eq!(
            below_mount(r"C:\", r"C:\LARM\m").as_deref(),
            Some(r"LARM\m")
        );
        assert_eq!(
            below_mount(r"c:\montajes\d\", r"C:\Montajes\D\Datos").as_deref(),
            Some("Datos")
        );
        assert_eq!(below_mount(r"D:\", r"C:\LARM"), None);
        assert_eq!(below_mount(r"C:\a\b", r"C:\a"), None);
    }

    fn location(canonical: &str, guid: Option<&str>, unc: Option<&str>) -> StorageLocation {
        StorageLocation {
            id: "l".into(),
            created_at: String::new(),
            updated_at: String::new(),
            row_version: 1,
            metadata: Map::new(),
            name: "n".into(),
            root_path: canonical.into(),
            canonical_path: canonical.into(),
            volume_guid: guid.map(String::from),
            drive_letter: None,
            unc_path: unc.map(String::from),
            filesystem: "NTFS".into(),
            storage_type: "LOCAL".into(),
            capacity_bytes: DecimalU64::default(),
            free_bytes: DecimalU64::default(),
            reserved_bytes: DecimalU64::default(),
            priority: 0,
            auto_placement: true,
            state: LocationState::Online,
            last_seen_at: None,
            volume_key: guid.unwrap_or("k").into(),
            performance_class: PerformanceClass::Ssd,
        }
    }

    fn volume(guid: &str, mounts: &[&str], online: bool) -> StorageVolume {
        StorageVolume {
            identity: VolumeIdentity {
                volume_key: Some(guid.into()),
                volume_guid: Some(guid.into()),
                unc_path: None,
                mount_points: mounts.iter().map(|m| (*m).to_string()).collect(),
                drive_letter: None,
            },
            label: None,
            filesystem: Some("NTFS".into()),
            drive_kind: DriveKind::Fixed,
            bus: None,
            seek_penalty: None,
            performance_class: PerformanceClass::Ssd,
            capacity_bytes: None,
            free_bytes: None,
            read_only: false,
            online,
            evidence_refs: vec![],
        }
    }

    #[test]
    fn resolutor_por_guid() {
        let g = "0f0e0d0c-1111-2222-3333-444455556666";
        let l = location(r"E:\Modelos", Some(g), None);
        // Misma letra: la ruta canónica.
        assert_eq!(
            resolve_root(&l, &[volume(g, &[r"E:\"], true)]).unwrap(),
            r"E:\Modelos"
        );
        // Letra cambiada: misma carpeta en la letra nueva.
        assert_eq!(
            resolve_root(&l, &[volume(g, &[r"F:\"], true)]).unwrap(),
            r"F:\Modelos"
        );
        // Sin letra: la ruta del GUID.
        assert_eq!(
            resolve_root(&l, &[volume(g, &[], true)]).unwrap(),
            format!(r"\\?\Volume{{{g}}}\Modelos")
        );
        // Montado en carpeta: manda la ruta guardada dentro del volumen.
        let mut m = location(r"C:\Montajes\Datos\Modelos", Some(g), None);
        m.metadata
            .insert(VOLUME_RELATIVE_KEY.into(), Value::String("Modelos".into()));
        assert_eq!(
            resolve_root(&m, &[volume(g, &[r"G:\"], true)]).unwrap(),
            r"G:\Modelos"
        );
        // Desconectado: no se inventa una ruta.
        assert_eq!(
            resolve_root(&l, &[volume(g, &[r"E:\"], false)]),
            Err(ResolveError::VolumeOffline)
        );
        assert_eq!(resolve_root(&l, &[]), Err(ResolveError::VolumeOffline));
        // UNC y sin identidad: la canónica.
        let u = location(r"\\srv\share\m", None, Some(r"\\srv\share"));
        assert_eq!(resolve_root(&u, &[]).unwrap(), r"\\srv\share\m");
        let n = location(r"Z:\m", None, None);
        assert_eq!(resolve_root(&n, &[]).unwrap(), r"Z:\m");
    }

    #[test]
    fn fases_y_serializacion() {
        assert!(MovePhase::Planned < MovePhase::DbSwitched);
        assert!(!MovePhase::Verified.switched());
        assert!(MovePhase::DbSwitched.switched());
        assert!(MovePhase::Failed.terminal() && !MovePhase::SourceDeleted.terminal());
        for p in [
            MovePhase::Planned,
            MovePhase::Locked,
            MovePhase::Copying,
            MovePhase::Verified,
            MovePhase::DbSwitched,
            MovePhase::AccessTested,
            MovePhase::SourceDeleted,
            MovePhase::Completed,
            MovePhase::Failed,
            MovePhase::Cancelled,
        ] {
            assert_eq!(MovePhase::parse(p.as_str()).unwrap(), p);
            assert_eq!(serde_json::to_value(p).unwrap(), p.as_str());
        }
        assert!(MovePhase::parse("RUNNING").is_err());
        assert_eq!(
            serde_json::to_value(Ownership::LarmManaged).unwrap(),
            "LARM_MANAGED"
        );
        assert_eq!(
            serde_json::to_value(ArtifactState::Copying).unwrap(),
            "COPYING"
        );
        let r: Result<MoveRequest, _> = serde_json::from_value(serde_json::json!({
            "target_storage_location_id": "x", "plan_hash": null, "dry_run": true,
            "approval_id": null, "extra": 1
        }));
        assert!(r.is_err(), "campos desconocidos rechazados");
    }
}
