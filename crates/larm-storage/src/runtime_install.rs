//! Publicación atómica de instalaciones gestionadas P031.
//!
//! El worker sólo escribe en su carpeta temporal ligada al `plan_id`. Tras
//! drenar el proceso, LARM copia los archivos regulares a `staging`, verifica
//! hashes y renombra el directorio completo al destino side-by-side.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use larm_domain::artifacts::{normalize_relative, resolve_root};
use larm_domain::error::{ErrorCode, LarmError};
use larm_domain::locations::LocationState;
use larm_domain::ports::{StorageLocationRepository, StorageProber};
use larm_domain::runtime_execution::RuntimeInstallWorkspace;
use larm_domain::runtime_install::{
    ManagedRuntimeInstallStore, RuntimeInstallWorkspaceMarker, INSTALL_WORKSPACE_MARKER,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const STAGE_MARKER: &str = ".larm-stage-owner.json";
const PUBLISHED_MARKER: &str = ".larm-managed-install.json";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Owner {
    job_id: String,
    adapter_plan_hash: String,
    plan_hash: String,
    storage_location_id: String,
    relative_destination: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileEntry {
    path: String,
    bytes: u64,
    sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PublishedManifest {
    schema_version: String,
    job_id: String,
    plan_hash: String,
    adapter_plan_hash: String,
    storage_location_id: String,
    relative_destination: String,
    files: Vec<FileEntry>,
}

struct PublishRequest<'a> {
    storage_root: &'a Path,
    job_id: &'a str,
    plan_hash: &'a str,
    adapter_plan_hash: &'a str,
    storage_location_id: &'a str,
    relative_destination: &'a str,
    workspace: Option<&'a RuntimeInstallWorkspace>,
}

/// Store gestionado que reconstruye la raíz desde la ubicación y la identidad
/// actual del volumen; nunca persiste ni confía en una ruta absoluta.
pub struct ManagedRuntimeInstallPublisher {
    pub locations: Arc<dyn StorageLocationRepository>,
    pub prober: Arc<dyn StorageProber>,
}

fn failure(code: ErrorCode, reason: &str, message: &str) -> LarmError {
    LarmError::new(code, message, reason)
}

fn io_error(reason: &str, error: std::io::Error) -> LarmError {
    failure(
        ErrorCode::ResourceUnavailable,
        reason,
        "No se pudo publicar la instalación gestionada",
    )
    .with_technical(error.to_string())
}

fn valid_hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn is_reparse(metadata: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}

fn ensure_directory(path: &Path, create: bool) -> Result<bool, LarmError> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => {
            if !metadata.is_dir() || is_reparse(&metadata) {
                return Err(failure(
                    ErrorCode::Forbidden,
                    "runtime_install_path_reparse_rejected",
                    "La ruta de instalación contiene un enlace o no es una carpeta",
                ));
            }
            Ok(true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && create => {
            match std::fs::create_dir(path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => {
                    return Err(io_error("runtime_install_directory_create_failed", error))
                }
            }
            ensure_directory(path, false)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(io_error("runtime_install_directory_read_failed", error)),
    }
}

fn ensure_relative_directories(root: &Path, relative: &str) -> Result<PathBuf, LarmError> {
    let normalized = normalize_relative(relative)?;
    let mut current = root.to_path_buf();
    for part in normalized.split('/') {
        current.push(part);
        ensure_directory(&current, true)?;
    }
    Ok(current)
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path, reason: &str) -> Result<T, LarmError> {
    let metadata = std::fs::symlink_metadata(path).map_err(|error| io_error(reason, error))?;
    if !metadata.is_file() || is_reparse(&metadata) {
        return Err(failure(
            ErrorCode::IntegrityFailed,
            reason,
            "El manifiesto de instalación no es un archivo regular",
        ));
    }
    serde_json::from_slice(&std::fs::read(path).map_err(|error| io_error(reason, error))?).map_err(
        |error| {
            failure(
                ErrorCode::IntegrityFailed,
                reason,
                "El manifiesto de instalación está dañado",
            )
            .with_technical(error.to_string())
        },
    )
}

fn write_json(path: &Path, value: &impl Serialize) -> Result<(), LarmError> {
    write_json_with_sync(path, value, |file| file.sync_all())
}

fn write_json_with_sync(
    path: &Path,
    value: &impl Serialize,
    mut sync: impl FnMut(&mut std::fs::File) -> std::io::Result<()>,
) -> Result<(), LarmError> {
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| io_error("runtime_install_manifest_write_failed", error))?;
    serde_json::to_writer(&mut file, value).map_err(|error| {
        failure(
            ErrorCode::ResourceUnavailable,
            "runtime_install_manifest_write_failed",
            "No se pudo guardar el manifiesto de instalación",
        )
        .with_technical(error.to_string())
    })?;
    sync(&mut file).map_err(|error| io_error("runtime_install_manifest_sync_failed", error))
}

fn owner_matches(actual: &Owner, expected: &Owner) -> bool {
    actual == expected
}

fn workspace_owner(
    workspace: &RuntimeInstallWorkspace,
    job_id: &str,
    adapter_plan_hash: &str,
) -> Result<(), LarmError> {
    let expected_path = workspace.root.join("runtime-installs").join(job_id);
    if workspace.path != expected_path {
        return Err(failure(
            ErrorCode::Forbidden,
            "runtime_install_workspace_path_mismatch",
            "El directorio temporal no pertenece a esta instalación",
        ));
    }
    ensure_directory(&workspace.root, false)?;
    ensure_directory(&workspace.root.join("runtime-installs"), false)?;
    ensure_directory(&workspace.path, false)?;
    let marker: RuntimeInstallWorkspaceMarker = read_json(
        &workspace.path.join(INSTALL_WORKSPACE_MARKER),
        "runtime_install_workspace_marker_invalid",
    )?;
    if marker.job_id != job_id || marker.adapter_plan_hash != adapter_plan_hash {
        return Err(failure(
            ErrorCode::Conflict,
            "runtime_install_workspace_binding_changed",
            "El directorio temporal pertenece a otro plan",
        ));
    }
    Ok(())
}

fn relative_file(path: &Path, root: &Path) -> Result<String, LarmError> {
    let relative = path.strip_prefix(root).map_err(|_| {
        failure(
            ErrorCode::Forbidden,
            "runtime_install_source_escape",
            "Un archivo de instalación sale de su carpeta temporal",
        )
    })?;
    let text = relative
        .to_str()
        .ok_or_else(|| {
            failure(
                ErrorCode::ValidationFailed,
                "runtime_install_filename_invalid",
                "La instalación contiene un nombre de archivo no válido",
            )
        })?
        .replace('\\', "/");
    normalize_relative(&text)
}

fn collect_files(
    root: &Path,
    skip_marker: Option<&str>,
) -> Result<Vec<(String, PathBuf)>, LarmError> {
    let mut pending = vec![root.to_path_buf()];
    let mut files = Vec::new();
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(&directory)
            .map_err(|error| io_error("runtime_install_source_read_failed", error))?
        {
            let entry =
                entry.map_err(|error| io_error("runtime_install_source_read_failed", error))?;
            let path = entry.path();
            let metadata = std::fs::symlink_metadata(&path)
                .map_err(|error| io_error("runtime_install_source_read_failed", error))?;
            if is_reparse(&metadata) {
                return Err(failure(
                    ErrorCode::Forbidden,
                    "runtime_install_reparse_rejected",
                    "La instalación contiene un enlace o punto de montaje",
                ));
            }
            if metadata.is_dir() {
                pending.push(path);
            } else if metadata.is_file() {
                let relative = relative_file(&path, root)?;
                if skip_marker == Some(relative.as_str())
                    || ((skip_marker == Some(PUBLISHED_MARKER)
                        || skip_marker == Some(STAGE_MARKER))
                        && (relative == PUBLISHED_MARKER || relative == STAGE_MARKER))
                {
                    continue;
                }
                if relative == PUBLISHED_MARKER || relative == STAGE_MARKER {
                    return Err(failure(
                        ErrorCode::Conflict,
                        "runtime_install_reserved_filename",
                        "La instalación usa un nombre reservado por LARM",
                    ));
                }
                files.push((relative, path));
            } else {
                return Err(failure(
                    ErrorCode::Forbidden,
                    "runtime_install_file_type_rejected",
                    "La instalación contiene un tipo de archivo no admitido",
                ));
            }
        }
    }
    files.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(files)
}

fn copy_hashed(source: &Path, destination: &Path) -> Result<FileEntry, LarmError> {
    let before = std::fs::symlink_metadata(source)
        .map_err(|error| io_error("runtime_install_source_read_failed", error))?;
    if !before.is_file() || is_reparse(&before) {
        return Err(failure(
            ErrorCode::Forbidden,
            "runtime_install_reparse_rejected",
            "La instalación contiene un archivo no regular",
        ));
    }
    let mut input = std::fs::File::open(source)
        .map_err(|error| io_error("runtime_install_source_read_failed", error))?;
    let mut output = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)
        .map_err(|error| io_error("runtime_install_stage_write_failed", error))?;
    let mut digest = Sha256::new();
    let mut total = 0_u64;
    let mut buffer = [0_u8; 128 * 1024];
    loop {
        let count = input
            .read(&mut buffer)
            .map_err(|error| io_error("runtime_install_source_read_failed", error))?;
        if count == 0 {
            break;
        }
        output
            .write_all(&buffer[..count])
            .map_err(|error| io_error("runtime_install_stage_write_failed", error))?;
        digest.update(&buffer[..count]);
        total = total.checked_add(count as u64).ok_or_else(|| {
            failure(
                ErrorCode::IntegrityFailed,
                "runtime_install_file_size_overflow",
                "El tamaño de la instalación no es válido",
            )
        })?;
    }
    output
        .sync_all()
        .map_err(|error| io_error("runtime_install_stage_sync_failed", error))?;
    let after = std::fs::symlink_metadata(source)
        .map_err(|error| io_error("runtime_install_source_read_failed", error))?;
    if after.len() != total || after.len() != before.len() || is_reparse(&after) {
        return Err(failure(
            ErrorCode::IntegrityFailed,
            "runtime_install_source_changed",
            "Un archivo cambió durante la publicación",
        ));
    }
    Ok(FileEntry {
        path: String::new(),
        bytes: total,
        sha256: hex::encode(digest.finalize()),
    })
}

fn hash_file(path: &Path) -> Result<FileEntry, LarmError> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|error| io_error("runtime_install_published_file_unavailable", error))?;
    if !metadata.is_file() || is_reparse(&metadata) {
        return Err(failure(
            ErrorCode::IntegrityFailed,
            "runtime_install_published_file_invalid",
            "La instalación publicada contiene un archivo no regular",
        ));
    }
    let mut file = std::fs::File::open(path)
        .map_err(|error| io_error("runtime_install_published_file_unavailable", error))?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 128 * 1024];
    let mut total = 0_u64;
    loop {
        let count = file
            .read(&mut buffer)
            .map_err(|error| io_error("runtime_install_published_file_unavailable", error))?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
        total += count as u64;
    }
    Ok(FileEntry {
        path: String::new(),
        bytes: total,
        sha256: hex::encode(digest.finalize()),
    })
}

fn manifest_matches(actual: &PublishedManifest, expected: &Owner) -> bool {
    actual.schema_version == "1"
        && actual.job_id == expected.job_id
        && actual.plan_hash == expected.plan_hash
        && actual.adapter_plan_hash == expected.adapter_plan_hash
        && actual.storage_location_id == expected.storage_location_id
        && actual.relative_destination == expected.relative_destination
        && !actual.files.is_empty()
}

fn verify_published(path: &Path, expected: &Owner) -> Result<(), LarmError> {
    let manifest: PublishedManifest = read_json(
        &path.join(PUBLISHED_MARKER),
        "runtime_install_manifest_invalid",
    )?;
    if !manifest_matches(&manifest, expected) {
        return Err(failure(
            ErrorCode::Conflict,
            "runtime_install_destination_owned_by_other_job",
            "El destino ya pertenece a otra instalación",
        ));
    }
    let actual = collect_files(path, Some(PUBLISHED_MARKER))?;
    if actual.len() != manifest.files.len() {
        return Err(failure(
            ErrorCode::IntegrityFailed,
            "runtime_install_published_manifest_mismatch",
            "Los archivos publicados no coinciden con su manifiesto",
        ));
    }
    for ((relative, file), expected_file) in actual.iter().zip(&manifest.files) {
        let actual_file = hash_file(file)?;
        if relative != &expected_file.path
            || actual_file.bytes != expected_file.bytes
            || actual_file.sha256 != expected_file.sha256
        {
            return Err(failure(
                ErrorCode::IntegrityFailed,
                "runtime_install_published_manifest_mismatch",
                "Los archivos publicados no coinciden con su manifiesto",
            ));
        }
    }
    Ok(())
}

fn stage_owner(path: &Path, expected: &Owner) -> Result<(), LarmError> {
    if !ensure_directory(path, false)? {
        return Err(failure(
            ErrorCode::Conflict,
            "runtime_install_stage_missing",
            "La carpeta temporal de publicación ya no existe",
        ));
    }
    let stage_owner_path = path.join(STAGE_MARKER);
    if stage_owner_path.exists() {
        let actual: Owner = read_json(&stage_owner_path, "runtime_install_stage_marker_invalid")?;
        if owner_matches(&actual, expected) {
            return Ok(());
        }
    }
    let published_path = path.join(PUBLISHED_MARKER);
    if published_path.exists() {
        let actual: PublishedManifest =
            read_json(&published_path, "runtime_install_stage_marker_invalid")?;
        if manifest_matches(&actual, expected) {
            return Ok(());
        }
    }
    Err(failure(
        ErrorCode::Conflict,
        "runtime_install_stage_owned_by_other_job",
        "La carpeta temporal de publicación pertenece a otro job",
    ))
}

fn discard_workspace_local(
    job_id: &str,
    adapter_plan_hash: &str,
    workspace: &RuntimeInstallWorkspace,
) -> Result<(), LarmError> {
    let expected_path = workspace.root.join("runtime-installs").join(job_id);
    if workspace.path != expected_path {
        return Err(failure(
            ErrorCode::Forbidden,
            "runtime_install_workspace_path_mismatch",
            "El directorio temporal no pertenece a esta instalación",
        ));
    }
    if !ensure_directory(&workspace.path, false)? {
        return Ok(());
    }
    workspace_owner(workspace, job_id, adapter_plan_hash)?;
    collect_files(&workspace.path, Some(INSTALL_WORKSPACE_MARKER))?;
    std::fs::remove_dir_all(&workspace.path)
        .map_err(|error| io_error("runtime_install_workspace_cleanup_failed", error))
}

fn publish_local(
    storage_root: &Path,
    job_id: &str,
    plan_hash: &str,
    adapter_plan_hash: &str,
    storage_location_id: &str,
    relative_destination: &str,
    workspace: Option<&RuntimeInstallWorkspace>,
) -> Result<(), LarmError> {
    publish_local_with_operations(
        PublishRequest {
            storage_root,
            job_id,
            plan_hash,
            adapter_plan_hash,
            storage_location_id,
            relative_destination,
            workspace,
        },
        copy_hashed,
        write_json,
        |source, destination| std::fs::rename(source, destination),
    )
}

fn publish_local_with_operations(
    request: PublishRequest<'_>,
    mut copy: impl FnMut(&Path, &Path) -> Result<FileEntry, LarmError>,
    mut write_manifest: impl FnMut(&Path, &PublishedManifest) -> Result<(), LarmError>,
    mut rename: impl FnMut(&Path, &Path) -> std::io::Result<()>,
) -> Result<(), LarmError> {
    let PublishRequest {
        storage_root,
        job_id,
        plan_hash,
        adapter_plan_hash,
        storage_location_id,
        relative_destination,
        workspace,
    } = request;
    if !larm_domain::bridge::is_uuid_v7(job_id)
        || !larm_domain::bridge::is_uuid_v7(storage_location_id)
        || !valid_hash(plan_hash)
        || !valid_hash(adapter_plan_hash)
    {
        return Err(failure(
            ErrorCode::ValidationFailed,
            "runtime_install_store_binding_invalid",
            "La identidad de la instalación no es válida",
        ));
    }
    let relative_destination = normalize_relative(relative_destination)?;
    let expected = Owner {
        job_id: job_id.into(),
        adapter_plan_hash: adapter_plan_hash.into(),
        plan_hash: plan_hash.into(),
        storage_location_id: storage_location_id.into(),
        relative_destination: relative_destination.clone(),
    };
    ensure_directory(storage_root, false)?;
    let destination_parent = relative_destination
        .rsplit_once('/')
        .map_or("", |(parent, _)| parent);
    if !destination_parent.is_empty() {
        ensure_relative_directories(storage_root, destination_parent)?;
    }
    let destination =
        storage_root.join(relative_destination.replace('/', std::path::MAIN_SEPARATOR_STR));
    match std::fs::symlink_metadata(&destination) {
        Ok(metadata) => {
            if !metadata.is_dir() || is_reparse(&metadata) {
                return Err(failure(
                    ErrorCode::Forbidden,
                    "runtime_install_destination_invalid",
                    "El destino ya existe y no es una carpeta gestionada",
                ));
            }
            return verify_published(&destination, &expected);
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(io_error("runtime_install_destination_read_failed", error)),
    }
    let workspace = workspace.ok_or_else(|| {
        failure(
            ErrorCode::ResourceUnavailable,
            "runtime_install_workspace_missing",
            "Falta el área temporal para publicar la instalación",
        )
    })?;
    workspace_owner(workspace, job_id, adapter_plan_hash)?;
    let source_files = collect_files(&workspace.path, Some(INSTALL_WORKSPACE_MARKER))?;
    if source_files.is_empty() {
        return Err(failure(
            ErrorCode::IntegrityFailed,
            "runtime_install_output_empty",
            "El worker terminó sin producir archivos de runtime",
        ));
    }

    let staging_parent = ensure_relative_directories(storage_root, "staging/runtime-install")?;
    let staging = staging_parent.join(job_id);
    match std::fs::symlink_metadata(&staging) {
        Ok(metadata) => {
            if !metadata.is_dir() || is_reparse(&metadata) {
                return Err(failure(
                    ErrorCode::Forbidden,
                    "runtime_install_stage_path_invalid",
                    "La carpeta temporal de publicación no es segura",
                ));
            }
            stage_owner(&staging, &expected)?;
            let _ = collect_files(&staging, Some(STAGE_MARKER))?;
            std::fs::remove_dir_all(&staging)
                .map_err(|error| io_error("runtime_install_stage_cleanup_failed", error))?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(io_error("runtime_install_stage_read_failed", error)),
    }
    std::fs::create_dir(&staging)
        .map_err(|error| io_error("runtime_install_stage_create_failed", error))?;
    let stage_marker_path = staging.join(STAGE_MARKER);
    write_json(&stage_marker_path, &expected)?;

    let staged = (|| {
        let mut entries = Vec::with_capacity(source_files.len());
        for (relative, source) in &source_files {
            let relative_parent = relative.rsplit_once('/').map_or("", |(parent, _)| parent);
            let target = if relative_parent.is_empty() {
                staging.join(relative)
            } else {
                ensure_relative_directories(&staging, relative_parent)?
                    .join(relative.rsplit('/').next().unwrap_or_default())
            };
            let mut entry = copy(source, &target)?;
            entry.path = relative.clone();
            entries.push(entry);
        }
        let manifest = PublishedManifest {
            schema_version: "1".into(),
            job_id: job_id.into(),
            plan_hash: plan_hash.into(),
            adapter_plan_hash: adapter_plan_hash.into(),
            storage_location_id: storage_location_id.into(),
            relative_destination: relative_destination.clone(),
            files: entries,
        };
        write_manifest(&staging.join(PUBLISHED_MARKER), &manifest)?;
        verify_published(&staging, &expected)?;
        std::fs::remove_file(&stage_marker_path)
            .map_err(|error| io_error("runtime_install_stage_marker_cleanup_failed", error))?;
        rename(&staging, &destination).map_err(|error| {
            let technical = format!(
                "{} -> {}: {error}",
                staging.display(),
                destination.display()
            );
            io_error("runtime_install_publish_failed", error).with_technical(technical)
        })?;
        Ok::<(), LarmError>(())
    })();
    match staged {
        Ok(()) => Ok(()),
        Err(error) => {
            if stage_owner(&staging, &expected).is_ok() {
                let _ = collect_files(&staging, Some(STAGE_MARKER));
                let _ = std::fs::remove_dir_all(&staging);
            }
            // A concurrent retry may have completed the atomic rename.
            if std::fs::symlink_metadata(&destination).is_ok()
                && verify_published(&destination, &expected).is_ok()
            {
                Ok(())
            } else {
                Err(error)
            }
        }
    }
}

#[async_trait]
impl ManagedRuntimeInstallStore for ManagedRuntimeInstallPublisher {
    async fn publish(
        &self,
        job_id: &str,
        plan_hash: &str,
        adapter_plan_hash: &str,
        storage_location_id: &str,
        relative_destination: &str,
        workspace: Option<&RuntimeInstallWorkspace>,
    ) -> Result<(), LarmError> {
        let location = self
            .locations
            .get(storage_location_id)
            .await?
            .ok_or_else(|| {
                failure(
                    ErrorCode::NotFound,
                    "runtime_install_storage_location_missing",
                    "La ubicación de instalación ya no existe",
                )
            })?;
        if matches!(
            location.state,
            LocationState::Offline
                | LocationState::ReadOnly
                | LocationState::Missing
                | LocationState::Error
        ) || location
            .volume_key
            .starts_with(larm_domain::locations::UNKNOWN_VOLUME_PREFIX)
        {
            return Err(failure(
                ErrorCode::ResourceUnavailable,
                "runtime_install_storage_unavailable",
                "La ubicación no está disponible para publicar el runtime",
            ));
        }
        let unc_roots = location.unc_path.iter().cloned().collect::<Vec<_>>();
        let observation = self.prober.probe(&unc_roots).await?;
        let observed = observation
            .by_key(&location.volume_key)
            .filter(|volume| volume.online)
            .ok_or_else(|| {
                failure(
                    ErrorCode::ResourceUnavailable,
                    "runtime_install_storage_offline",
                    "El volumen de instalación no está disponible",
                )
            })?;
        if observed.read_only {
            return Err(failure(
                ErrorCode::ResourceUnavailable,
                "runtime_install_storage_read_only",
                "El volumen de instalación es de solo lectura",
            ));
        }
        let root = resolve_root(&location, &observation.volumes).map_err(|_| {
            failure(
                ErrorCode::ResourceUnavailable,
                "runtime_install_storage_offline",
                "No se pudo resolver la ubicación actual del volumen",
            )
        })?;
        let root = crate::locations::inspect_root(&root)?;
        let root = PathBuf::from(root);
        let job_id = job_id.to_string();
        let plan_hash = plan_hash.to_string();
        let adapter_plan_hash = adapter_plan_hash.to_string();
        let storage_location_id = storage_location_id.to_string();
        let relative_destination = relative_destination.to_string();
        let workspace = workspace.cloned();
        tokio::task::spawn_blocking(move || {
            publish_local(
                &root,
                &job_id,
                &plan_hash,
                &adapter_plan_hash,
                &storage_location_id,
                &relative_destination,
                workspace.as_ref(),
            )
        })
        .await
        .map_err(|error| {
            failure(
                ErrorCode::Internal,
                "runtime_install_publish_task_failed",
                "Falló la publicación de la instalación",
            )
            .with_technical(error.to_string())
        })?
    }

    async fn discard_workspace(
        &self,
        job_id: &str,
        adapter_plan_hash: &str,
        workspace: &RuntimeInstallWorkspace,
    ) -> Result<(), LarmError> {
        let job_id = job_id.to_string();
        let adapter_plan_hash = adapter_plan_hash.to_string();
        let workspace = workspace.clone();
        tokio::task::spawn_blocking(move || {
            discard_workspace_local(&job_id, &adapter_plan_hash, &workspace)
        })
        .await
        .map_err(|error| {
            failure(
                ErrorCode::Internal,
                "runtime_install_cleanup_task_failed",
                "Falló la limpieza de la instalación",
            )
            .with_technical(error.to_string())
        })?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bindings() -> (String, String, String, String, String) {
        (
            uuid::Uuid::now_v7().to_string(),
            "a".repeat(64),
            "b".repeat(64),
            uuid::Uuid::now_v7().to_string(),
            format!(
                "runtimes/{}/{}/{}",
                uuid::Uuid::now_v7(),
                uuid::Uuid::now_v7(),
                "c".repeat(64)
            ),
        )
    }

    fn workspace(
        root: &Path,
        job_id: &str,
        adapter_plan_hash: &str,
        file: &str,
    ) -> Result<RuntimeInstallWorkspace, Box<dyn std::error::Error>> {
        let parent = root.join("runtime-installs");
        let path = parent.join(job_id);
        std::fs::create_dir_all(&path)?;
        std::fs::write(
            path.join(INSTALL_WORKSPACE_MARKER),
            serde_json::to_vec(&RuntimeInstallWorkspaceMarker {
                job_id: job_id.into(),
                adapter_plan_hash: adapter_plan_hash.into(),
            })?,
        )?;
        std::fs::write(path.join(file), b"managed-engine-fixture")?;
        Ok(RuntimeInstallWorkspace {
            root: root.into(),
            path,
        })
    }

    #[test]
    fn managed_install_is_atomically_published_and_retry_is_idempotent(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let storage = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR"))?;
        let work = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR"))?;
        let (job, plan_hash, adapter_hash, location, destination) = bindings();
        let workspace = workspace(work.path(), &job, &adapter_hash, "engine.bin")?;

        publish_local(
            storage.path(),
            &job,
            &plan_hash,
            &adapter_hash,
            &location,
            &destination,
            Some(&workspace),
        )?;
        let published = storage
            .path()
            .join(destination.replace('/', std::path::MAIN_SEPARATOR_STR));
        assert_eq!(
            std::fs::read(published.join("engine.bin"))?,
            b"managed-engine-fixture"
        );
        assert!(published.join(PUBLISHED_MARKER).is_file());
        assert!(workspace.path.exists());
        publish_local(
            storage.path(),
            &job,
            &plan_hash,
            &adapter_hash,
            &location,
            &destination,
            None,
        )?;
        Ok(())
    }

    #[test]
    fn interrupted_owned_staging_is_recovered_before_publication(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let storage = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR"))?;
        let work = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR"))?;
        let (job, plan_hash, adapter_hash, location, destination) = bindings();
        let workspace = workspace(work.path(), &job, &adapter_hash, "engine.bin")?;
        let expected = Owner {
            job_id: job.clone(),
            adapter_plan_hash: adapter_hash.clone(),
            plan_hash: plan_hash.clone(),
            storage_location_id: location.clone(),
            relative_destination: destination.clone(),
        };
        let staging = storage
            .path()
            .join("staging")
            .join("runtime-install")
            .join(&job);
        std::fs::create_dir_all(&staging)?;
        write_json(&staging.join(STAGE_MARKER), &expected)?;
        write_json(
            &staging.join(PUBLISHED_MARKER),
            &PublishedManifest {
                schema_version: "1".into(),
                job_id: job.clone(),
                plan_hash: plan_hash.clone(),
                adapter_plan_hash: adapter_hash.clone(),
                storage_location_id: location.clone(),
                relative_destination: destination.clone(),
                files: Vec::new(),
            },
        )?;
        std::fs::write(staging.join("partial.bin"), b"interrupted copy")?;

        publish_local(
            storage.path(),
            &job,
            &plan_hash,
            &adapter_hash,
            &location,
            &destination,
            Some(&workspace),
        )?;

        let published = storage
            .path()
            .join(destination.replace('/', std::path::MAIN_SEPARATOR_STR));
        assert_eq!(
            std::fs::read(published.join("engine.bin"))?,
            b"managed-engine-fixture"
        );
        assert!(!staging.exists());
        Ok(())
    }

    #[test]
    fn un_fallo_de_escritura_a_mitad_de_copia_limpia_staging_y_conserva_workspace(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let storage = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR"))?;
        let work = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR"))?;
        let (job, plan_hash, adapter_hash, location, destination) = bindings();
        let workspace = workspace(work.path(), &job, &adapter_hash, "engine.bin")?;

        let result = publish_local_with_operations(
            PublishRequest {
                storage_root: storage.path(),
                job_id: &job,
                plan_hash: &plan_hash,
                adapter_plan_hash: &adapter_hash,
                storage_location_id: &location,
                relative_destination: &destination,
                workspace: Some(&workspace),
            },
            |_source, target| {
                std::fs::write(target, b"partial copy")
                    .map_err(|error| io_error("runtime_install_stage_write_failed", error))?;
                Err(io_error(
                    "runtime_install_stage_write_failed",
                    std::io::Error::new(
                        std::io::ErrorKind::StorageFull,
                        "fallo de escritura inyectado",
                    ),
                ))
            },
            write_json,
            |source, destination| std::fs::rename(source, destination),
        );
        let error = result
            .err()
            .ok_or_else(|| std::io::Error::other("se esperaba el fallo de escritura inducido"))?;

        assert_eq!(error.code, ErrorCode::ResourceUnavailable);
        assert_eq!(error.details.reason, "runtime_install_stage_write_failed");
        let staging = storage
            .path()
            .join("staging")
            .join("runtime-install")
            .join(&job);
        assert!(!staging.exists());
        let published = storage
            .path()
            .join(destination.replace('/', std::path::MAIN_SEPARATOR_STR));
        assert!(!published.exists());
        assert_eq!(
            std::fs::read(workspace.path.join("engine.bin"))?,
            b"managed-engine-fixture"
        );
        Ok(())
    }

    #[test]
    fn fallo_parcial_al_escribir_manifest_limpia_staging_y_conserva_workspace(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let storage = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR"))?;
        let work = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR"))?;
        let (job, plan_hash, adapter_hash, location, destination) = bindings();
        let workspace = workspace(work.path(), &job, &adapter_hash, "engine.bin")?;

        let result = publish_local_with_operations(
            PublishRequest {
                storage_root: storage.path(),
                job_id: &job,
                plan_hash: &plan_hash,
                adapter_plan_hash: &adapter_hash,
                storage_location_id: &location,
                relative_destination: &destination,
                workspace: Some(&workspace),
            },
            copy_hashed,
            |path, _manifest| {
                std::fs::write(path, b"{\"schema_version\":")
                    .map_err(|error| io_error("runtime_install_manifest_write_failed", error))?;
                Err(failure(
                    ErrorCode::ResourceUnavailable,
                    "runtime_install_manifest_write_failed",
                    "Fallo parcial de escritura inyectado",
                ))
            },
            |source, destination| std::fs::rename(source, destination),
        );
        let error = result
            .err()
            .ok_or_else(|| std::io::Error::other("se esperaba el fallo parcial del manifiesto"))?;

        assert_eq!(error.code, ErrorCode::ResourceUnavailable);
        assert_eq!(
            error.details.reason,
            "runtime_install_manifest_write_failed"
        );
        let staging = storage
            .path()
            .join("staging")
            .join("runtime-install")
            .join(&job);
        assert!(!staging.exists());
        let published = storage
            .path()
            .join(destination.replace('/', std::path::MAIN_SEPARATOR_STR));
        assert!(!published.exists());
        assert_eq!(
            std::fs::read(workspace.path.join("engine.bin"))?,
            b"managed-engine-fixture"
        );
        Ok(())
    }

    #[test]
    fn fallo_al_sincronizar_manifest_limpia_staging_y_conserva_workspace(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let storage = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR"))?;
        let work = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR"))?;
        let (job, plan_hash, adapter_hash, location, destination) = bindings();
        let workspace = workspace(work.path(), &job, &adapter_hash, "engine.bin")?;

        let result = publish_local_with_operations(
            PublishRequest {
                storage_root: storage.path(),
                job_id: &job,
                plan_hash: &plan_hash,
                adapter_plan_hash: &adapter_hash,
                storage_location_id: &location,
                relative_destination: &destination,
                workspace: Some(&workspace),
            },
            copy_hashed,
            |path, manifest| {
                write_json_with_sync(path, manifest, |_file| {
                    Err(std::io::Error::other("fallo de sync inyectado"))
                })
            },
            |source, destination| std::fs::rename(source, destination),
        );
        let error = result
            .err()
            .ok_or_else(|| std::io::Error::other("se esperaba el fallo de sync del manifiesto"))?;

        assert_eq!(error.code, ErrorCode::ResourceUnavailable);
        assert_eq!(error.details.reason, "runtime_install_manifest_sync_failed");
        let staging = storage
            .path()
            .join("staging")
            .join("runtime-install")
            .join(&job);
        assert!(!staging.exists());
        let published = storage
            .path()
            .join(destination.replace('/', std::path::MAIN_SEPARATOR_STR));
        assert!(!published.exists());
        assert_eq!(
            std::fs::read(workspace.path.join("engine.bin"))?,
            b"managed-engine-fixture"
        );
        Ok(())
    }

    #[test]
    fn fallo_de_renombrado_atomico_limpia_staging_y_conserva_workspace(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let storage = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR"))?;
        let work = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR"))?;
        let (job, plan_hash, adapter_hash, location, destination) = bindings();
        let workspace = workspace(work.path(), &job, &adapter_hash, "engine.bin")?;

        let result = publish_local_with_operations(
            PublishRequest {
                storage_root: storage.path(),
                job_id: &job,
                plan_hash: &plan_hash,
                adapter_plan_hash: &adapter_hash,
                storage_location_id: &location,
                relative_destination: &destination,
                workspace: Some(&workspace),
            },
            copy_hashed,
            write_json,
            |_staging, _destination| {
                Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "fallo de renombrado atomico inyectado",
                ))
            },
        );
        let error = result
            .err()
            .ok_or_else(|| std::io::Error::other("se esperaba el fallo de renombrado inducido"))?;

        assert_eq!(error.code, ErrorCode::ResourceUnavailable);
        assert_eq!(error.details.reason, "runtime_install_publish_failed");
        let staging = storage
            .path()
            .join("staging")
            .join("runtime-install")
            .join(&job);
        assert!(!staging.exists());
        let published = storage
            .path()
            .join(destination.replace('/', std::path::MAIN_SEPARATOR_STR));
        assert!(!published.exists());
        assert_eq!(
            std::fs::read(workspace.path.join("engine.bin"))?,
            b"managed-engine-fixture"
        );
        Ok(())
    }

    #[test]
    fn failed_install_cleanup_only_removes_its_marked_workspace(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let work = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR"))?;
        let job = uuid::Uuid::now_v7().to_string();
        let neighbor = uuid::Uuid::now_v7().to_string();
        let adapter_hash = "d".repeat(64);
        let own = workspace(work.path(), &job, &adapter_hash, "partial.bin")?;
        let other = workspace(work.path(), &neighbor, &"e".repeat(64), "keep.bin")?;

        discard_workspace_local(&job, &adapter_hash, &own)?;
        assert!(!own.path.exists());
        assert_eq!(
            std::fs::read(other.path.join("keep.bin"))?,
            b"managed-engine-fixture"
        );
        Ok(())
    }

    #[test]
    fn destination_traversal_and_unmarked_workspaces_are_rejected(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let storage = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR"))?;
        let work = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR"))?;
        let (job, plan_hash, adapter_hash, location, _) = bindings();
        let workspace = workspace(work.path(), &job, &adapter_hash, "engine.bin")?;
        assert!(publish_local(
            storage.path(),
            &job,
            &plan_hash,
            &adapter_hash,
            &location,
            "../outside",
            Some(&workspace),
        )
        .is_err());

        let bad_job = uuid::Uuid::now_v7().to_string();
        let unmarked = RuntimeInstallWorkspace {
            root: work.path().into(),
            path: work.path().join("runtime-installs").join(&bad_job),
        };
        std::fs::create_dir_all(&unmarked.path)?;
        assert!(discard_workspace_local(&bad_job, &adapter_hash, &unmarked).is_err());
        assert!(unmarked.path.exists());
        Ok(())
    }
}
