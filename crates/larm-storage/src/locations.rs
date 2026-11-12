//! Registro de StorageLocation (P014): validación de la raíz antes de
//! guardar, identidad del volumen desde la observación (P012) y CRUD seguro.
//!
//! Validación de una raíz (contratos 02 y 08):
//! 1. ruta absoluta local (`C:\…`) o UNC (`\\servidor\recurso\…`); se
//!    rechazan rutas relativas, de dispositivo (`\\.\`, `\\?\GLOBALROOT`),
//!    flujos alternativos (`:` fuera de la letra) y `..`;
//! 2. existe y es una carpeta;
//! 3. ni ella ni ningún ancestro es un reparse point (symlink, junction o
//!    montaje): una raíz no puede redirigir a otro sitio;
//! 4. se puede escribir (fichero temporal propio que se borra al momento);
//! 5. no se solapa con otra ubicación (ni dentro ni contenedora);
//! 6. su volumen sale de la observación: el GUID del punto de montaje más
//!    largo que la contiene (local) o `\\servidor\recurso` (UNC). Sin
//!    identidad verificable, `volume_key = unknown:<ruta>` y la colocación
//!    automática no la elegirá.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use larm_domain::error::{ErrorCode, LarmError};
use larm_domain::locations::{
    LocationState, StorageLocation, StorageLocationCreate, UNKNOWN_VOLUME_PREFIX,
};
use larm_domain::ports::{
    LocationFilter, LocationService, MutationContext, RootInspector, StorageLocationRepository,
    StorageProber,
};
use larm_domain::storage::{
    location_state, parse_unc, PerformanceClass, StorageObservation, StorageVolume,
};
use larm_domain::units::DecimalU64;

/// Tiempo máximo de una operación de disco sobre la raíz (una ruta de red
/// caída puede bloquear decenas de segundos).
pub const FS_TIMEOUT: Duration = Duration::from_secs(20);

fn invalid(reason: &str, message: &str) -> LarmError {
    LarmError::new(ErrorCode::ValidationFailed, message, reason)
}

async fn blocking<T: Send + 'static>(
    what: &'static str,
    f: impl FnOnce() -> T + Send + 'static,
) -> Result<T, LarmError> {
    match tokio::time::timeout(FS_TIMEOUT, tokio::task::spawn_blocking(f)).await {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(e)) => Err(LarmError::new(
            ErrorCode::Internal,
            "Fallo interno comprobando la ruta",
            "fs_task_failed",
        )
        .with_technical(e.to_string())),
        Err(_) => Err(LarmError::new(
            ErrorCode::ResourceUnavailable,
            "La ruta no respondió a tiempo",
            "fs_timeout",
        )
        .retryable(true)
        .with_technical(what)),
    }
}

/// Forma de una raíz escrita por el usuario.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RootKind {
    Local,
    Unc(larm_domain::storage::UncRoot),
}

/// Sintaxis de la raíz, antes de tocar el disco.
///
/// # Errors
/// `VALIDATION_FAILED` `root_path_invalid`.
pub fn check_syntax(root: &str) -> Result<RootKind, LarmError> {
    let bad = |m: &str| invalid("root_path_invalid", m);
    let path = root.trim();
    if path.is_empty() || path != root {
        return Err(bad(
            "La ruta está vacía o tiene espacios al principio o al final",
        ));
    }
    if path.starts_with(r"\\") || path.starts_with("//") {
        if path.starts_with(r"\\?\GLOBALROOT") {
            return Err(bad("Las rutas de dispositivo no son ubicaciones"));
        }
        let unc = parse_unc(path).map_err(|e| bad(&e))?;
        if unc.rest.contains(':') {
            return Err(bad("Los flujos alternativos (ADS) no son ubicaciones"));
        }
        return Ok(RootKind::Unc(unc));
    }
    let bytes = path.as_bytes();
    let drive = bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'\\' || bytes[2] == b'/');
    if !drive {
        return Err(bad(
            "La ruta debe ser absoluta (C:\\…) o de red (\\\\servidor\\recurso\\…)",
        ));
    }
    if path[2..].contains(':') {
        return Err(bad("Los flujos alternativos (ADS) no son ubicaciones"));
    }
    if path.replace('/', "\\").split('\\').any(|c| c == "..") {
        return Err(bad("'..' no se admite en una ubicación"));
    }
    Ok(RootKind::Local)
}

/// Quita los prefijos `\\?\` y `\\?\UNC\` de una ruta canónica de Windows.
#[must_use]
pub fn plain_path(path: &Path) -> String {
    let text = path.display().to_string();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else if let Some(rest) = text.strip_prefix(r"\\?\") {
        rest.to_string()
    } else {
        text
    }
}

fn components(path: &str) -> Vec<String> {
    path.replace('/', "\\")
        .split('\\')
        .filter(|c| !c.is_empty())
        .map(str::to_lowercase)
        .collect()
}

/// ¿`inner` está dentro de `outer` (o es la misma)? Por componentes, sin
/// distinguir mayúsculas.
#[must_use]
pub fn within(outer: &str, inner: &str) -> bool {
    let (o, i) = (components(outer), components(inner));
    o.len() <= i.len() && o.iter().zip(&i).all(|(a, b)| a == b)
}

/// Comprueba la raíz en disco y devuelve su ruta canónica.
pub(crate) fn inspect_root(root: &str) -> Result<String, LarmError> {
    let path = PathBuf::from(root);
    let meta = std::fs::metadata(&path).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => invalid("root_not_found", "La carpeta no existe"),
        std::io::ErrorKind::PermissionDenied => {
            invalid("root_not_readable", "No hay permiso para leer la carpeta")
        }
        _ => {
            invalid("root_not_found", "La carpeta no está accesible").with_technical(e.to_string())
        }
    })?;
    if !meta.is_dir() {
        return Err(invalid("root_not_directory", "La ruta no es una carpeta"));
    }
    // Ningún componente puede ser un reparse point.
    let mut current = PathBuf::new();
    for part in path.components() {
        current.push(part);
        if matches!(
            part,
            std::path::Component::Prefix(_) | std::path::Component::RootDir
        ) {
            continue;
        }
        let m = std::fs::symlink_metadata(&current).map_err(|e| {
            invalid("root_not_found", "La carpeta no está accesible").with_technical(e.to_string())
        })?;
        if is_reparse(&m) {
            return Err(invalid(
                "reparse_point_rejected",
                "La ruta pasa por un enlace simbólico, junction o punto de montaje",
            )
            .with_technical(current.display().to_string()));
        }
    }
    let canonical = std::fs::canonicalize(&path).map_err(|e| {
        invalid("root_not_found", "No se pudo resolver la ruta").with_technical(e.to_string())
    })?;
    let canonical = plain_path(&canonical);
    // Permiso de escritura: fichero propio, creado en exclusiva y borrado.
    let probe = path.join(format!(".larm-write-test-{}", uuid::Uuid::now_v7()));
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
        .and_then(|mut f| std::io::Write::write_all(&mut f, b"larm"))
        .map_err(|e| {
            invalid("root_not_writable", "No se puede escribir en la carpeta")
                .with_technical(e.to_string())
        })?;
    std::fs::remove_file(&probe).map_err(|e| {
        invalid(
            "root_not_writable",
            "No se pudo borrar el fichero de prueba",
        )
        .with_technical(e.to_string())
    })?;
    Ok(canonical)
}

#[cfg(windows)]
fn is_reparse(m: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    m.file_attributes() & 0x400 != 0 // FILE_ATTRIBUTE_REPARSE_POINT
}

#[cfg(not(windows))]
fn is_reparse(m: &std::fs::Metadata) -> bool {
    m.file_type().is_symlink()
}

/// Volumen que contiene la ruta: punto de montaje más largo (local) o la
/// raíz UNC.
#[must_use]
pub fn volume_for<'a>(
    observation: &'a StorageObservation,
    canonical: &str,
    kind: &RootKind,
) -> Option<&'a StorageVolume> {
    match kind {
        RootKind::Unc(unc) => observation.by_key(&unc.volume_key()),
        RootKind::Local => observation
            .volumes
            .iter()
            .filter(|v| v.identity.volume_guid.is_some())
            .flat_map(|v| v.identity.mount_points.iter().map(move |m| (m, v)))
            .filter(|(m, _)| within(m, canonical))
            .max_by_key(|(m, _)| components(m).len())
            .map(|(_, v)| v),
    }
}

/// Inspector de raíces para la observación periódica (P012/P014).
pub struct FsRootInspector;

#[async_trait]
impl RootInspector for FsRootInspector {
    async fn present(&self, canonical_path: &str) -> Option<bool> {
        let path = canonical_path.to_string();
        let outcome = blocking("present", move || match std::fs::metadata(&path) {
            Ok(m) => Some(m.is_dir()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Some(false),
            Err(_) => None,
        })
        .await;
        outcome.ok().flatten()
    }
}

pub struct LocationServiceImpl {
    pub repo: Arc<dyn StorageLocationRepository>,
    pub prober: Arc<dyn StorageProber>,
    pub thresholds: Option<larm_domain::locations::DiskThresholds>,
}

impl LocationServiceImpl {
    /// Valida la raíz y construye los campos calculados por el servidor.
    async fn resolve(
        &self,
        request: &StorageLocationCreate,
        exclude_id: Option<&str>,
    ) -> Result<Resolved, LarmError> {
        if request.name.trim().is_empty() {
            return Err(invalid("name_invalid", "El nombre no puede estar vacío"));
        }
        let kind = check_syntax(&request.root_path)?;
        let root = request.root_path.clone();
        let canonical = blocking("inspect_root", move || inspect_root(&root)).await??;
        for other in self.repo.all().await? {
            if Some(other.id.as_str()) == exclude_id {
                continue;
            }
            if within(&other.canonical_path, &canonical)
                || within(&canonical, &other.canonical_path)
            {
                return Err(LarmError::new(
                    ErrorCode::Conflict,
                    "La carpeta se solapa con otra ubicación registrada",
                    "root_overlap",
                )
                .with_resource(other.id));
            }
        }
        let unc: Vec<String> = match &kind {
            RootKind::Unc(u) => vec![u.root()],
            RootKind::Local => vec![],
        };
        let observation = self.prober.probe(&unc).await?;
        let volume = volume_for(&observation, &canonical, &kind).cloned();
        if let Some(capacity) = volume.as_ref().and_then(|v| v.capacity_bytes) {
            if request.reserved_bytes > capacity {
                return Err(invalid(
                    "reservation_exceeds_capacity",
                    "El espacio reservado no puede superar la capacidad del disco",
                ));
            }
        }
        Ok(Resolved {
            kind,
            canonical,
            volume,
            observed_at: observation.observed_at,
        })
    }

    fn build(
        &self,
        base: Option<&StorageLocation>,
        request: &StorageLocationCreate,
        r: Resolved,
    ) -> StorageLocation {
        let now = larm_domain::bridge::timestamp(chrono::Utc::now());
        let v = r.volume.as_ref();
        let observed_class = v.map_or(PerformanceClass::Unknown, |v| v.performance_class);
        let class = if observed_class == PerformanceClass::Unknown {
            request.performance_class
        } else {
            observed_class
        };
        let volume_key = v
            .and_then(|v| v.identity.volume_key.clone())
            .unwrap_or_else(|| format!("{UNKNOWN_VOLUME_PREFIX}{}", r.canonical.to_lowercase()));
        let state = location_state(v, Some(true), self.thresholds);
        let mut metadata = base.map(|b| b.metadata.clone()).unwrap_or_default();
        metadata.insert(
            "performance_class_source".into(),
            serde_json::Value::String(
                if observed_class == PerformanceClass::Unknown {
                    "hint"
                } else {
                    "observed"
                }
                .into(),
            ),
        );
        // Ruta dentro del volumen (P016): si la letra cambia, la raíz se
        // reconstruye colgándola del punto de montaje nuevo.
        if let (Some(v), RootKind::Local) = (v, &r.kind) {
            if let Some(inside) = v
                .identity
                .mount_points
                .iter()
                .filter_map(|m| larm_domain::artifacts::below_mount(m, &r.canonical))
                .min_by_key(String::len)
            {
                metadata.insert(
                    larm_domain::artifacts::VOLUME_RELATIVE_KEY.into(),
                    serde_json::Value::String(inside),
                );
            }
        }
        StorageLocation {
            id: base.map_or_else(|| uuid::Uuid::now_v7().to_string(), |b| b.id.clone()),
            created_at: base.map_or_else(|| now.clone(), |b| b.created_at.clone()),
            updated_at: now,
            row_version: base.map_or(1, |b| b.row_version),
            metadata,
            name: request.name.trim().to_string(),
            root_path: request.root_path.clone(),
            canonical_path: r.canonical,
            volume_guid: v.and_then(|v| v.identity.volume_guid.clone()),
            drive_letter: v.and_then(|v| v.identity.drive_letter.clone()),
            unc_path: match &r.kind {
                RootKind::Unc(u) => Some(u.root()),
                RootKind::Local => None,
            },
            filesystem: v
                .and_then(|v| v.filesystem.clone())
                .unwrap_or_else(|| "UNKNOWN".into()),
            storage_type: match &r.kind {
                RootKind::Unc(_) => "NETWORK".into(),
                RootKind::Local => "LOCAL".into(),
            },
            capacity_bytes: v.and_then(|v| v.capacity_bytes).unwrap_or_default(),
            free_bytes: v.and_then(|v| v.free_bytes).unwrap_or_default(),
            reserved_bytes: request.reserved_bytes,
            priority: request.priority,
            auto_placement: request.auto_placement,
            state,
            last_seen_at: v.filter(|v| v.online).map(|_| r.observed_at.clone()),
            volume_key,
            performance_class: class,
        }
    }

    fn not_found(id: &str) -> LarmError {
        LarmError::new(ErrorCode::NotFound, "Ubicación desconocida", "not_found")
            .with_resource(id.to_string())
    }
}

struct Resolved {
    kind: RootKind,
    canonical: String,
    volume: Option<StorageVolume>,
    observed_at: String,
}

#[async_trait]
impl LocationService for LocationServiceImpl {
    async fn create(
        &self,
        request: &StorageLocationCreate,
        ctx: &MutationContext,
    ) -> Result<StorageLocation, LarmError> {
        let resolved = self.resolve(request, None).await?;
        let location = self.build(None, request, resolved);
        self.repo.insert(&location, ctx).await?;
        Ok(location)
    }

    async fn update(
        &self,
        id: &str,
        if_match: i64,
        request: &StorageLocationCreate,
        ctx: &MutationContext,
    ) -> Result<StorageLocation, LarmError> {
        let current = self
            .repo
            .get(id)
            .await?
            .ok_or_else(|| Self::not_found(id))?;
        if current.row_version != if_match {
            return Err(LarmError::new(
                ErrorCode::StaleRevision,
                "La ubicación cambió desde que la leíste",
                "stale_revision",
            )
            .with_resource(id.to_string()));
        }
        let artifacts = self.repo.artifact_count(id).await?;
        if request.root_path != current.root_path && artifacts > 0 {
            return Err(LarmError::new(
                ErrorCode::Conflict,
                "Cambiar la carpeta de una ubicación con modelos es una migración: usa mover modelos",
                "root_change_requires_migration",
            )
            .with_resource(id.to_string()));
        }
        let resolved = self.resolve(request, Some(id)).await?;
        let next = self.build(Some(&current), request, resolved);
        // La misma ruta en otro volumen (otro disco con la misma letra): sus
        // modelos siguen en el volumen original.
        if artifacts > 0 && next.volume_key != current.volume_key {
            return Err(LarmError::new(
                ErrorCode::Conflict,
                "La carpeta pertenece ahora a otro volumen; sus modelos siguen en el original",
                "volume_identity_changed",
            )
            .with_resource(id.to_string()));
        }
        self.repo.update(&next, if_match, ctx).await
    }

    async fn delete(
        &self,
        id: &str,
        if_match: i64,
        ctx: &MutationContext,
    ) -> Result<(), LarmError> {
        self.repo.delete(id, if_match, ctx).await
    }

    async fn get(&self, id: &str) -> Result<StorageLocation, LarmError> {
        let location = self
            .repo
            .get(id)
            .await?
            .ok_or_else(|| Self::not_found(id))?;
        let all = self.repo.all().await?;
        Ok(with_budget(vec![location], &all).remove(0))
    }

    async fn list(
        &self,
        after: Option<(String, String)>,
        limit: u32,
        filter: &LocationFilter,
    ) -> Result<Vec<StorageLocation>, LarmError> {
        let page = self.repo.list(after, limit, filter).await?;
        let all = self.repo.all().await?;
        Ok(with_budget(page, &all))
    }
}

/// Añade a la respuesta (no se guarda) `metadata.volume_budget`: espacio
/// utilizable del volumen tras la mayor reserva de sus raíces y con qué
/// otras ubicaciones comparte ese presupuesto.
fn with_budget(mut items: Vec<StorageLocation>, all: &[StorageLocation]) -> Vec<StorageLocation> {
    let usable = usable_by_location(all);
    for item in &mut items {
        let shared: Vec<&str> = all
            .iter()
            .filter(|o| o.volume_key == item.volume_key && o.id != item.id)
            .map(|o| o.id.as_str())
            .collect();
        item.metadata.insert(
            "volume_budget".into(),
            serde_json::json!({
                "usable_bytes": usable.get(&item.id).copied().unwrap_or_default(),
                "shared_with": shared,
            }),
        );
    }
    items
}

/// Espacio utilizable de cada ubicación con el presupuesto compartido de su
/// volumen (la mayor reserva de sus raíces), para la API y la interfaz.
#[must_use]
pub fn usable_by_location(
    locations: &[StorageLocation],
) -> std::collections::BTreeMap<String, DecimalU64> {
    // Por volumen: todas las reservas y el espacio libre de la observación
    // más reciente entre sus raíces (cada fila guarda lo que vio al
    // actualizarse; dos raíces del mismo volumen deben dar el mismo presupuesto).
    let mut reservations: std::collections::BTreeMap<&str, Vec<u64>> =
        std::collections::BTreeMap::new();
    let mut freshest: std::collections::BTreeMap<&str, (Option<&str>, u64)> =
        std::collections::BTreeMap::new();
    for l in locations {
        let key = l.volume_key.as_str();
        reservations
            .entry(key)
            .or_default()
            .push(l.reserved_bytes.get());
        let seen = l.last_seen_at.as_deref();
        let entry = freshest.entry(key).or_insert((seen, l.free_bytes.get()));
        if seen > entry.0 {
            *entry = (seen, l.free_bytes.get());
        }
    }
    locations
        .iter()
        .map(|l| {
            let key = l.volume_key.as_str();
            let usable = if l.state == LocationState::Offline {
                0
            } else {
                larm_domain::locations::volume_budget(freshest[key].1, &reservations[key])
            };
            (l.id.clone(), DecimalU64::new(usable).unwrap_or_default())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn sintaxis_de_raices() {
        assert_eq!(check_syntax(r"C:\IA\Modelos").unwrap(), RootKind::Local);
        assert!(matches!(
            check_syntax(r"\\NAS\IA\Modelos").unwrap(),
            RootKind::Unc(_)
        ));
        for (bad, reason) in [
            ("", "root_path_invalid"),
            (r"IA\Modelos", "root_path_invalid"),
            (r"C:IA", "root_path_invalid"),
            (r"C:\IA\fichero.txt:secreto", "root_path_invalid"),
            (r"C:\IA\..\Windows", "root_path_invalid"),
            (r"\\.\PhysicalDrive0", "root_path_invalid"),
            (
                r"\\?\GLOBALROOT\Device\HarddiskVolume3",
                "root_path_invalid",
            ),
            (r" C:\IA", "root_path_invalid"),
            (r"\\NAS\IA\x:ads", "root_path_invalid"),
        ] {
            assert_eq!(
                check_syntax(bad).unwrap_err().details.reason,
                reason,
                "{bad:?}"
            );
        }
    }

    #[test]
    fn solapamiento_por_componentes() {
        assert!(within(r"C:\IA", r"c:\ia\modelos"));
        assert!(within(r"C:\IA\", r"C:\IA"));
        assert!(!within(r"C:\IA", r"C:\IA2"), "no por prefijo de texto");
        assert!(!within(r"C:\IA\Modelos", r"C:\IA"));
        assert_eq!(plain_path(Path::new(r"\\?\C:\IA")), r"C:\IA");
        assert_eq!(plain_path(Path::new(r"\\?\UNC\nas\ia")), r"\\nas\ia");
    }

    #[test]
    fn raiz_real_inexistente_fichero_escribible_y_junction() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("modelos");
        assert_eq!(
            inspect_root(&root.display().to_string())
                .unwrap_err()
                .details
                .reason,
            "root_not_found"
        );
        std::fs::create_dir(&root).unwrap();
        let canonical = inspect_root(&root.display().to_string()).unwrap();
        assert!(within(
            &canonical,
            &plain_path(&std::fs::canonicalize(&root).unwrap())
        ));
        assert_eq!(
            std::fs::read_dir(&root).unwrap().count(),
            0,
            "el fichero de prueba se borra"
        );
        let file = dir.path().join("f.txt");
        std::fs::write(&file, b"x").unwrap();
        assert_eq!(
            inspect_root(&file.display().to_string())
                .unwrap_err()
                .details
                .reason,
            "root_not_directory"
        );
        #[cfg(windows)]
        {
            // Junction (no exige privilegios): la raíz no puede redirigir.
            let link = dir.path().join("enlace");
            let ok = std::process::Command::new("cmd")
                .args([
                    "/C",
                    "mklink",
                    "/J",
                    &link.display().to_string(),
                    &root.display().to_string(),
                ])
                .output()
                .unwrap()
                .status
                .success();
            assert!(ok, "mklink /J");
            assert_eq!(
                inspect_root(&link.display().to_string())
                    .unwrap_err()
                    .details
                    .reason,
                "reparse_point_rejected"
            );
            let below = link.join("sub");
            std::fs::create_dir(root.join("sub")).unwrap();
            assert_eq!(
                inspect_root(&below.display().to_string())
                    .unwrap_err()
                    .details
                    .reason,
                "reparse_point_rejected"
            );
        }
    }

    fn loc(id: &str, key: &str, free: u64, reserved: u64, state: LocationState) -> StorageLocation {
        StorageLocation {
            id: id.into(),
            created_at: "t".into(),
            updated_at: "t".into(),
            row_version: 1,
            metadata: serde_json::Map::new(),
            name: id.into(),
            root_path: format!(r"C:\{id}"),
            canonical_path: format!(r"C:\{id}"),
            volume_guid: Some(key.into()),
            drive_letter: Some("C:".into()),
            unc_path: None,
            filesystem: "NTFS".into(),
            storage_type: "LOCAL".into(),
            capacity_bytes: DecimalU64::new(1000).unwrap(),
            free_bytes: DecimalU64::new(free).unwrap(),
            reserved_bytes: DecimalU64::new(reserved).unwrap(),
            priority: 0,
            auto_placement: true,
            state,
            last_seen_at: None,
            volume_key: key.into(),
            performance_class: PerformanceClass::Nvme,
        }
    }

    #[test]
    fn presupuesto_compartido_por_volumen() {
        let ls = vec![
            loc("a", "v1", 500, 100, LocationState::Online),
            loc("b", "v1", 500, 300, LocationState::Online),
            loc("c", "v2", 50, 100, LocationState::Online),
            loc("d", "v3", 900, 0, LocationState::Offline),
        ];
        let u = usable_by_location(&ls);
        assert_eq!(u["a"].get(), 200, "mayor reserva del volumen, no la suma");
        assert_eq!(u["b"].get(), 200);
        assert_eq!(u["c"].get(), 0, "nunca negativo");
        assert_eq!(u["d"].get(), 0, "offline no ofrece espacio");
        // Dos raíces del mismo volumen con observaciones de distinto momento:
        // manda la más reciente y ambas dan el mismo presupuesto.
        let mut old = loc("x", "v9", 900, 100, LocationState::Online);
        old.last_seen_at = Some("2026-10-11T10:00:00.000Z".into());
        let mut new = loc("y", "v9", 400, 0, LocationState::Online);
        new.last_seen_at = Some("2026-10-11T11:00:00.000Z".into());
        let u = usable_by_location(&[old, new]);
        assert_eq!((u["x"].get(), u["y"].get()), (300, 300));
    }
}
