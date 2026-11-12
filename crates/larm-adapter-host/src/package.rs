//! Paquetes ZIP de adaptadores: inspección sin efectos, snapshot verificado,
//! extracción acotada y publicación por versión. Nunca ejecuta su contenido.
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use larm_adapter_sdk::manifest::AdapterManifest;
use larm_adapter_sdk::permissions::{package_hash_checked, Assessment, TrustStore};
use larm_domain::error::{ErrorCode, LarmError};
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

pub const MAX_BYTES: u64 = 1 << 30;
pub const MAX_FILES: usize = 100_000;
pub const MAX_RATIO: u64 = 1000;
pub const MANIFEST_BYTES: u64 = 1 << 20;
pub const MAX_METADATA_BYTES: u64 = 64 << 20;

fn error(code: ErrorCode, reason: &str, message: &str) -> LarmError {
    LarmError::new(code, message, reason)
}
fn invalid(reason: &str) -> LarmError {
    error(
        ErrorCode::ValidationFailed,
        reason,
        "El paquete del adaptador no es válido",
    )
}
fn io_error(e: impl std::fmt::Display) -> LarmError {
    error(
        ErrorCode::ResourceUnavailable,
        "package_io_failed",
        "No se pudo acceder al paquete del adaptador",
    )
    .with_technical(e.to_string())
}

/// Control cooperativo; el llamador conserva el flag para cancelar también
/// cuando vence su timeout de spawn_blocking. No permite publicación tardía.
type Checkpoint = Arc<dyn Fn() -> Result<(), LarmError> + Send + Sync>;
type WriteProgress = Arc<dyn Fn(u64) -> Result<(), LarmError> + Send + Sync>;
#[derive(Clone)]
pub struct PackageControl {
    pub cancelled: Arc<AtomicBool>,
    deadline: Instant,
    checkpoint: Option<Checkpoint>,
    written: Option<WriteProgress>,
}
impl Default for PackageControl {
    fn default() -> Self {
        Self::with_timeout(Duration::from_secs(120))
    }
}
impl PackageControl {
    #[must_use]
    pub fn with_timeout(timeout: Duration) -> Self {
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
            deadline: Instant::now() + timeout,
            checkpoint: None,
            written: None,
        }
    }
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }
    /// Los callbacks sólo viven en el hilo de IO; no deben bloquear el hilo
    /// del runtime. El coordinador los liga al guard y a la reserva persistida.
    pub fn with_io_hooks(mut self, checkpoint: Checkpoint, written: WriteProgress) -> Self {
        self.checkpoint = Some(checkpoint);
        self.written = Some(written);
        self
    }
    pub fn check(&self) -> Result<(), LarmError> {
        if self.cancelled.load(Ordering::Acquire) {
            return Err(error(
                ErrorCode::ResourceUnavailable,
                "operation_cancelled",
                "La operación se canceló",
            ));
        }
        if Instant::now() >= self.deadline {
            return Err(error(
                ErrorCode::Timeout,
                "package_timeout",
                "El paquete no se pudo procesar a tiempo",
            ));
        }
        if let Some(checkpoint) = &self.checkpoint {
            checkpoint()?;
        }
        Ok(())
    }
}

/// Un componente de ruta válido también en Windows. Se rechazan aliases de
/// dispositivos, ADS, controles y normalizaciones ambiguas del filesystem.
pub fn component(name: &str) -> Result<(), LarmError> {
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.ends_with(['.', ' '])
        || name.chars().any(|c| {
            c.is_control() || matches!(c, '/' | '\\' | ':' | '<' | '>' | '"' | '|' | '?' | '*')
        })
    {
        return Err(invalid("package_path_invalid"));
    }
    let stem = name
        .split('.')
        .next()
        .unwrap_or_default()
        .trim_matches(' ')
        .to_ascii_uppercase();
    if matches!(
        stem.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) || ["COM", "LPT"].iter().any(|prefix| {
        stem.strip_prefix(prefix).is_some_and(|v| {
            matches!(
                v,
                "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
            )
        })
    }) {
        return Err(invalid("package_path_invalid"));
    }
    Ok(())
}
fn relative(name: &str, directory: bool) -> Result<String, LarmError> {
    let name = if directory {
        name.strip_suffix('/').unwrap_or(name)
    } else {
        name
    };
    for c in name.split('/') {
        component(c)?;
    }
    Ok(name.to_lowercase())
}

pub fn check_managed_path(path: &Path) -> Result<(), LarmError> {
    if !path.is_absolute()
        || path
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(invalid("package_root_invalid"));
    }
    for ancestor in path.ancestors() {
        match std::fs::symlink_metadata(ancestor) {
            Ok(meta) => {
                #[cfg(windows)]
                let reparse = {
                    use std::os::windows::fs::MetadataExt;
                    meta.file_attributes() & 0x400 != 0
                };
                #[cfg(not(windows))]
                let reparse = false;
                if meta.file_type().is_symlink() || reparse {
                    return Err(invalid("package_reparse_rejected"));
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(io_error(e)),
        }
    }
    Ok(())
}

fn copy_bounded(
    reader: &mut impl Read,
    writer: &mut impl Write,
    max: u64,
    control: &PackageControl,
    disk_write: bool,
) -> Result<(u64, String), LarmError> {
    let mut bytes = 0u64;
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 65536];
    loop {
        control.check()?;
        let n = reader.read(&mut buffer).map_err(io_error)?;
        if n == 0 {
            break;
        }
        bytes = bytes
            .checked_add(n as u64)
            .ok_or_else(|| invalid("package_size_overflow"))?;
        if bytes > max {
            return Err(invalid("package_size_limit"));
        }
        control.check()?;
        writer.write_all(&buffer[..n]).map_err(io_error)?;
        if disk_write {
            if let Some(written) = &control.written {
                written(n as u64)?;
            }
        }
        hash.update(&buffer[..n]);
    }
    Ok((bytes, hex::encode(hash.finalize())))
}

// Acota el directorio central ANTES de que el lector ZIP reserve su índice.
// Los offsets son del formato ZIP/ZIP64; no se admiten archivos multidisco.
fn preflight_directory(file: &mut File, length: u64) -> Result<(), LarmError> {
    let tail_len = length.min(65557) as usize;
    file.seek(SeekFrom::End(-(tail_len as i64)))
        .map_err(io_error)?;
    let mut tail = vec![0; tail_len];
    file.read_exact(&mut tail).map_err(io_error)?;
    let start = (0..tail.len().saturating_sub(21))
        .rev()
        .find(|&n| {
            tail[n..].starts_with(b"PK\x05\x06")
                && n + 22 + u16::from_le_bytes([tail[n + 20], tail[n + 21]]) as usize == tail.len()
        })
        .ok_or_else(|| invalid("zip_directory_invalid"))?;
    let record = &tail[start..];
    let u16_at = |n| u16::from_le_bytes([record[n], record[n + 1]]);
    let u32_at = |n| u32::from_le_bytes([record[n], record[n + 1], record[n + 2], record[n + 3]]);
    if u16_at(4) != 0 || u16_at(6) != 0 || u16_at(8) != u16_at(10) {
        return Err(invalid("zip_multidisk_rejected"));
    }
    let mut count = u64::from(u16_at(10));
    let mut metadata = u64::from(u32_at(12));
    let mut offset = u64::from(u32_at(16));
    let eocd = length - tail_len as u64 + start as u64;
    if count == 65535 || metadata == u64::from(u32::MAX) || offset == u64::from(u32::MAX) {
        let locator_offset = eocd
            .checked_sub(20)
            .ok_or_else(|| invalid("zip64_directory_invalid"))?;
        file.seek(SeekFrom::Start(locator_offset))
            .map_err(io_error)?;
        let mut locator = [0u8; 20];
        file.read_exact(&mut locator).map_err(io_error)?;
        if &locator[..8] != b"PK\x06\x07\0\0\0\0" || locator[16..] != [1, 0, 0, 0] {
            return Err(invalid("zip64_directory_invalid"));
        }
        let mut value = [0u8; 8];
        value.copy_from_slice(&locator[8..16]);
        let zip64_offset = u64::from_le_bytes(value);
        if zip64_offset
            .checked_add(56)
            .is_none_or(|end| end > locator_offset)
        {
            return Err(invalid("zip64_directory_invalid"));
        }
        file.seek(SeekFrom::Start(zip64_offset)).map_err(io_error)?;
        let mut zip64 = [0u8; 56];
        file.read_exact(&mut zip64).map_err(io_error)?;
        let n64 = |n| {
            let mut bytes = [0u8; 8];
            bytes.copy_from_slice(&zip64[n..n + 8]);
            u64::from_le_bytes(bytes)
        };
        if &zip64[..4] != b"PK\x06\x06"
            || zip64[16..24] != [0; 8]
            || n64(24) != n64(32)
            || n64(4) < 44
        {
            return Err(invalid("zip64_directory_invalid"));
        }
        count = n64(32);
        metadata = n64(40);
        offset = n64(48);
    }
    if count > MAX_FILES as u64 {
        return Err(invalid("package_file_limit"));
    }
    if metadata > MAX_METADATA_BYTES {
        return Err(invalid("package_metadata_limit"));
    }
    if offset.checked_add(metadata).is_none_or(|end| end > eocd) || count > metadata / 46 {
        return Err(invalid("zip_directory_invalid"));
    }
    file.rewind().map_err(io_error)?;
    Ok(())
}

#[derive(Debug, Clone)]
pub struct ArchivePlan {
    pub source: PathBuf,
    pub archive_hash: String,
    pub archive_bytes: u64,
    pub unpacked_bytes: u64,
    pub manifest: AdapterManifest,
}

/// Entrada asíncrona con timeout; el flag impide trabajo tardío al cancelar.
pub async fn inspect_async(
    source: PathBuf,
    expected_hash: String,
) -> Result<ArchivePlan, LarmError> {
    let control = PackageControl::default();
    let worker_control = control.clone();
    let task = tokio::task::spawn_blocking(move || {
        ArchivePlan::inspect(&source, &expected_hash, &worker_control)
    });
    match tokio::time::timeout(Duration::from_secs(30), task).await {
        Ok(Ok(result)) => result,
        Ok(Err(e)) => Err(error(
            ErrorCode::Internal,
            "package_task_failed",
            "No se pudo inspeccionar el paquete",
        )
        .with_technical(e.to_string())),
        Err(_) => {
            control.cancel();
            Err(error(
                ErrorCode::Timeout,
                "package_timeout",
                "El paquete no respondió a tiempo",
            ))
        }
    }
}

impl ArchivePlan {
    /// expected_hash es el hash de los bytes ZIP; package_sha256 es el hash
    /// canónico P022 de sus ficheros, excluyendo manifest.json.
    pub fn inspect(
        source: &Path,
        expected_hash: &str,
        control: &PackageControl,
    ) -> Result<Self, LarmError> {
        control.check()?;
        if !source.is_absolute()
            || expected_hash.len() != 64
            || !expected_hash
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
        {
            return Err(invalid("package_request_invalid"));
        }
        let mut file = File::open(source).map_err(io_error)?;
        let (archive_bytes, hash) =
            copy_bounded(&mut file, &mut std::io::sink(), MAX_BYTES, control, false)?;
        if hash != expected_hash {
            return Err(error(
                ErrorCode::IntegrityFailed,
                "archive_hash_mismatch",
                "El ZIP no coincide con el hash esperado",
            ));
        }
        preflight_directory(&mut file, archive_bytes)?;
        let mut zip = ZipArchive::new(file)
            .map_err(|e| invalid("zip_invalid").with_technical(e.to_string()))?;
        if zip.len() > MAX_FILES {
            return Err(invalid("package_file_limit"));
        }
        let mut names = BTreeMap::new();
        let mut total = 0u64;
        let mut manifest = None;
        for index in 0..zip.len() {
            control.check()?;
            let mut entry = zip
                .by_index(index)
                .map_err(|e| invalid("zip_invalid").with_technical(e.to_string()))?;
            let name = entry
                .name()
                .map_err(|_| invalid("package_path_encoding"))?
                .into_owned();
            let key = relative(&name, entry.is_dir())?;
            let kind = entry.unix_mode().unwrap_or_default() & 0o170000;
            if entry.is_symlink() || !matches!(kind, 0 | 0o040000 | 0o100000) {
                return Err(invalid("package_link_rejected"));
            }
            if entry.encrypted() {
                return Err(error(
                    ErrorCode::UnsupportedCapability,
                    "encrypted_package",
                    "No se admiten paquetes cifrados",
                ));
            }
            if names.insert(key.clone(), entry.is_dir()).is_some() {
                return Err(invalid("package_duplicate_path"));
            }
            total = total
                .checked_add(entry.size())
                .ok_or_else(|| invalid("package_size_overflow"))?;
            if total > MAX_BYTES || entry.size() > entry.compressed_size().saturating_mul(MAX_RATIO)
            {
                return Err(invalid("package_bomb_rejected"));
            }
            if key == "manifest.json" {
                if name != "manifest.json" || entry.is_dir() || entry.size() > MANIFEST_BYTES {
                    return Err(invalid("package_manifest_invalid"));
                }
                let mut data = Vec::new();
                copy_bounded(&mut entry, &mut data, MANIFEST_BYTES, control, false)?;
                let text =
                    std::str::from_utf8(&data).map_err(|_| invalid("package_manifest_encoding"))?;
                manifest = Some(AdapterManifest::parse(text)?);
            }
        }
        for name in names.keys() {
            let mut parent = name.as_str();
            while let Some((p, _)) = parent.rsplit_once('/') {
                if names.get(p) == Some(&false) {
                    return Err(invalid("package_path_collision"));
                }
                parent = p;
            }
        }
        let manifest = manifest.ok_or_else(|| invalid("package_manifest_missing"))?;
        component(&manifest.id)?;
        component(&manifest.version)?;
        if manifest.id.eq_ignore_ascii_case(".staging") {
            return Err(invalid("package_path_invalid"));
        }
        let exe = relative(&manifest.executable.relative_path.replace('\\', "/"), false)?;
        if names.get(&exe) != Some(&false) {
            return Err(invalid("package_executable_missing"));
        }
        Ok(Self {
            source: source.to_path_buf(),
            archive_hash: hash,
            archive_bytes,
            unpacked_bytes: total,
            manifest,
        })
    }

    /// Pico: ZIP privado + ficheros expandidos + margen del contrato P015.
    pub fn required_bytes(&self) -> u64 {
        let peak = self.archive_bytes + self.unpacked_bytes;
        peak + (1 << 30).max(peak.div_ceil(20))
    }
}

pub struct StagedPackage {
    _temporary: TempDir,
    pub payload: PathBuf,
    pub plan: ArchivePlan,
    pub assessment: Assessment,
}

impl StagedPackage {
    /// Sólo se llama tras reservar required_bytes del volumen destino.
    pub fn extract(
        plan: ArchivePlan,
        root: &Path,
        trust: &TrustStore,
        control: &PackageControl,
    ) -> Result<Self, LarmError> {
        control.check()?;
        check_managed_path(root)?;
        let assessment = trust.assess(&plan.manifest, false)?;
        control.check()?;
        std::fs::create_dir_all(root).map_err(io_error)?;
        let staging = root.join(".staging");
        check_managed_path(&staging)?;
        control.check()?;
        std::fs::create_dir_all(&staging).map_err(io_error)?;
        control.check()?;
        let temporary = tempfile::Builder::new()
            .prefix("package-")
            .tempdir_in(&staging)
            .map_err(io_error)?;
        let snapshot_path = temporary.path().join("archive.zip");
        let mut input = File::open(&plan.source).map_err(io_error)?;
        control.check()?;
        let mut snapshot = OpenOptions::new()
            .create_new(true)
            .write(true)
            .read(true)
            .open(&snapshot_path)
            .map_err(io_error)?;
        let (bytes, hash) = copy_bounded(&mut input, &mut snapshot, MAX_BYTES, control, true)?;
        if bytes != plan.archive_bytes || hash != plan.archive_hash {
            return Err(error(
                ErrorCode::IntegrityFailed,
                "archive_changed",
                "El ZIP cambió durante la preparación",
            ));
        }
        snapshot.sync_all().map_err(io_error)?;
        snapshot.rewind().map_err(io_error)?;
        let mut zip = ZipArchive::new(snapshot).map_err(io_error)?;
        let payload = temporary.path().join("payload");
        control.check()?;
        std::fs::create_dir(&payload).map_err(io_error)?;
        let mut written = 0u64;
        let mut extracted = BTreeSet::new();
        for index in 0..zip.len() {
            control.check()?;
            let mut entry = zip.by_index(index).map_err(io_error)?;
            let name = entry.name().map_err(io_error)?.into_owned();
            let key = relative(&name, entry.is_dir())?;
            if !extracted.insert(key) {
                return Err(invalid("package_duplicate_path"));
            }
            let target = payload.join(&name);
            if entry.is_dir() {
                std::fs::create_dir_all(target).map_err(io_error)?;
                continue;
            }
            if let Some(parent) = target.parent() {
                control.check()?;
                std::fs::create_dir_all(parent).map_err(io_error)?;
            }
            control.check()?;
            let mut out = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&target)
                .map_err(io_error)?;
            let size = entry.size();
            let (count, _) = copy_bounded(
                &mut entry,
                &mut out,
                size.min(MAX_BYTES - written),
                control,
                true,
            )?;
            if count != size {
                return Err(error(
                    ErrorCode::IntegrityFailed,
                    "package_size_mismatch",
                    "El tamaño expandido no coincide",
                ));
            }
            out.sync_all().map_err(io_error)?;
            written += count;
        }
        let extracted_manifest = read_manifest(&payload.join("manifest.json"), control)?;
        if extracted_manifest != plan.manifest {
            return Err(error(
                ErrorCode::IntegrityFailed,
                "package_manifest_changed",
                "El manifiesto expandido no coincide con el inspeccionado",
            ));
        }
        if written != plan.unpacked_bytes
            || controlled_hash(&payload, control)? != plan.manifest.package_sha256
        {
            return Err(error(
                ErrorCode::IntegrityFailed,
                "package_hash_mismatch",
                "El contenido del paquete no coincide con su manifiesto",
            ));
        }
        control.check()?;
        Ok(Self {
            _temporary: temporary,
            payload,
            plan,
            assessment,
        })
    }

    /// El guard conserva el bloqueo de versión hasta el commit en SQLite.
    /// Si no se confirma, Drop retira únicamente el directorio recién creado.
    pub fn publish(
        self,
        root: &Path,
        control: &PackageControl,
    ) -> Result<PublishedPackage, LarmError> {
        control.check()?;
        check_managed_path(root)?;
        let current = read_manifest(&self.payload.join("manifest.json"), control)?;
        if current != self.plan.manifest
            || controlled_hash(&self.payload, control)? != self.plan.manifest.package_sha256
        {
            return Err(error(
                ErrorCode::IntegrityFailed,
                "package_changed_before_publish",
                "El paquete cambió antes de publicarse",
            ));
        }
        let (lock, destination) = lock_version(root, &self.plan.manifest, control)?;
        if destination.exists() {
            return Err(error(
                ErrorCode::Conflict,
                "adapter_version_exists",
                "Ya existe esta versión del adaptador",
            ));
        }
        control.check()?;
        std::fs::rename(&self.payload, &destination).map_err(io_error)?;
        Ok(PublishedPackage {
            stage: self,
            destination,
            _lock: lock,
            committed: false,
        })
    }
}

/// Compartido con recuperación: ni dos instaladores ni un reconciliador
/// pueden decidir simultáneamente sobre la misma versión física.
pub fn lock_version(
    root: &Path,
    manifest: &AdapterManifest,
    control: &PackageControl,
) -> Result<(File, PathBuf), LarmError> {
    component(&manifest.id)?;
    component(&manifest.version)?;
    check_managed_path(root)?;
    let parent = root.join(&manifest.id);
    check_managed_path(&parent)?;
    control.check()?;
    std::fs::create_dir_all(&parent).map_err(io_error)?;
    let lock_path = parent.join(format!(".{}.install-lock", manifest.version));
    check_managed_path(&lock_path)?;
    control.check()?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(lock_path)
        .map_err(io_error)?;
    lock.try_lock().map_err(|_| {
        error(
            ErrorCode::Conflict,
            "adapter_install_busy",
            "Esta versión ya se está instalando",
        )
    })?;
    let destination = parent.join(&manifest.version);
    check_managed_path(&destination)?;
    Ok((lock, destination))
}

pub struct PublishedPackage {
    stage: StagedPackage,
    pub destination: PathBuf,
    _lock: File,
    committed: bool,
}
impl PublishedPackage {
    pub fn manifest(&self) -> &AdapterManifest {
        &self.stage.plan.manifest
    }
    pub fn assessment(&self) -> Assessment {
        self.stage.assessment
    }
    pub fn commit(mut self) {
        self.committed = true;
    }
    /// El journal ya contiene PUBLISHING. A partir de aquí un resultado de
    /// commit incierto se reconcilia; Drop nunca borra bytes posiblemente
    /// registrados. Se conserva el bloqueo de versión mientras vive el guard.
    pub fn retain_for_recovery(&mut self) {
        self.committed = true;
    }
}

/// Hash canónico P022 con límites sobre bytes reales y comprobación de reparse
/// points en cada entrada; compartido por publicación y recuperación.
pub fn controlled_hash(dir: &Path, control: &PackageControl) -> Result<String, LarmError> {
    let mut paths = crate::package_paths::PinnedPaths::default();
    controlled_hash_with_paths(dir, control, &mut paths)
}

pub(crate) fn controlled_hash_with_paths(
    dir: &Path,
    control: &PackageControl,
    paths: &mut crate::package_paths::PinnedPaths,
) -> Result<String, LarmError> {
    package_hash_checked(dir, &["manifest.json"], &mut |path, entries, bytes| {
        control.check()?;
        if entries > MAX_FILES || bytes > MAX_BYTES {
            return Err(invalid("package_size_limit"));
        }
        paths.check(path)
    })
}

pub fn verify_published(
    dir: &Path,
    manifest: &AdapterManifest,
    control: &PackageControl,
) -> Result<(), LarmError> {
    control.check()?;
    check_managed_path(dir)?;
    if read_manifest(&dir.join("manifest.json"), control)? != *manifest
        || controlled_hash(dir, control)? != manifest.package_sha256
    {
        return Err(error(
            ErrorCode::IntegrityFailed,
            "package_hash_mismatch",
            "El paquete publicado no coincide con el plan",
        ));
    }
    control.check()
}

/// Same complete manifest/inventory/byte verification, borrowing only live
/// path handles. No metadata, inventory or hash result is reused.
pub(crate) fn verify_published_with_paths(
    dir: &Path,
    manifest: &AdapterManifest,
    control: &PackageControl,
    paths: &mut crate::package_paths::PinnedPaths,
) -> Result<(), LarmError> {
    control.check()?;
    paths.check(dir)?;
    if read_manifest(&dir.join("manifest.json"), control)? != *manifest
        || controlled_hash_with_paths(dir, control, paths)? != manifest.package_sha256
    {
        return Err(error(
            ErrorCode::IntegrityFailed,
            "package_hash_mismatch",
            "El paquete publicado no coincide con el plan",
        ));
    }
    control.check()
}

#[derive(Debug, Clone)]
pub struct ExportedPackage {
    pub destination: PathBuf,
    pub archive_hash: String,
    pub archive_bytes: u64,
    pub manifest: AdapterManifest,
}

struct ExportFile {
    source: PathBuf,
    archive_path: String,
    bytes: u64,
    sha256: String,
}

fn collect_export_files(
    root: &Path,
    current: &Path,
    control: &PackageControl,
    files: &mut Vec<ExportFile>,
    names: &mut BTreeSet<String>,
    total: &mut u64,
) -> Result<(), LarmError> {
    control.check()?;
    check_managed_path(current)?;
    for entry in std::fs::read_dir(current).map_err(io_error)? {
        control.check()?;
        let entry = entry.map_err(io_error)?;
        let path = entry.path();
        check_managed_path(&path)?;
        let file_type = entry.file_type().map_err(io_error)?;
        if file_type.is_dir() {
            collect_export_files(root, &path, control, files, names, total)?;
            continue;
        }
        if !file_type.is_file() {
            return Err(invalid("package_link_rejected"));
        }
        if files.len() >= MAX_FILES {
            return Err(invalid("package_file_limit"));
        }
        let relative_path = path
            .strip_prefix(root)
            .map_err(|_| invalid("package_path_invalid"))?;
        let archive_path = relative_path
            .to_str()
            .ok_or_else(|| invalid("package_path_encoding"))?
            .replace('\\', "/");
        let key = relative(&archive_path, false)?;
        if !names.insert(key) {
            return Err(invalid("package_duplicate_path"));
        }
        let mut source = File::open(&path).map_err(io_error)?;
        let (bytes, sha256) = copy_bounded(
            &mut source,
            &mut std::io::sink(),
            MAX_BYTES.saturating_sub(*total),
            control,
            false,
        )?;
        let reported_size = source.metadata().map_err(io_error)?.len();
        if bytes != reported_size {
            return Err(error(
                ErrorCode::IntegrityFailed,
                "package_size_mismatch",
                "El tamaño del paquete cambió durante la exportación",
            ));
        }
        *total = total
            .checked_add(bytes)
            .ok_or_else(|| invalid("package_size_overflow"))?;
        files.push(ExportFile {
            source: path,
            archive_path,
            bytes,
            sha256,
        });
    }
    Ok(())
}

fn within_case_insensitive(path: &Path, root: &Path) -> bool {
    let normalize = |value: &Path| {
        value
            .to_string_lossy()
            .replace('/', "\\")
            .trim_end_matches('\\')
            .to_lowercase()
    };
    let (path, root) = (normalize(path), normalize(root));
    path == root || path.starts_with(&format!("{root}\\"))
}

/// Exporta un paquete instalado como ZIP reproducible, sin leer configuración
/// de usuario ni resolver referencias protegidas. El archivo final se publica
/// sólo después de verificar la huella del paquete y volver a inspeccionar el ZIP.
///
/// # Errors
/// Rechaza destinos existentes, rutas no gestionables, paquetes alterados y fallos de IO.
pub fn export_published_package(
    source: &Path,
    destination: &Path,
    control: &PackageControl,
) -> Result<ExportedPackage, LarmError> {
    control.check()?;
    check_managed_path(source)?;
    check_managed_path(destination)?;
    if !source.is_dir() {
        return Err(error(
            ErrorCode::NotFound,
            "adapter_package_not_found",
            "No se encontró el paquete instalado",
        ));
    }
    if !destination.is_absolute() || destination.file_name().is_none() {
        return Err(invalid("export_destination_invalid"));
    }
    let parent = destination
        .parent()
        .ok_or_else(|| invalid("export_destination_invalid"))?;
    check_managed_path(parent)?;
    if !parent.is_dir() {
        return Err(error(
            ErrorCode::ResourceUnavailable,
            "export_destination_parent_unavailable",
            "La carpeta de destino no está disponible",
        ));
    }
    if destination.exists() {
        return Err(error(
            ErrorCode::Conflict,
            "export_destination_exists",
            "El archivo de destino ya existe",
        ));
    }
    let source_root = source.canonicalize().map_err(io_error)?;
    let destination_parent = parent.canonicalize().map_err(io_error)?;
    if within_case_insensitive(&destination_parent, &source_root) {
        return Err(invalid("export_destination_inside_package"));
    }

    let manifest = read_manifest(&source.join("manifest.json"), control)?;
    verify_published(source, &manifest, control)?;
    let mut files = Vec::new();
    let mut names = BTreeSet::new();
    let mut total = 0u64;
    collect_export_files(source, source, control, &mut files, &mut names, &mut total)?;
    files.sort_by(|left, right| left.archive_path.cmp(&right.archive_path));
    if controlled_hash(source, control)? != manifest.package_sha256 {
        return Err(error(
            ErrorCode::IntegrityFailed,
            "package_hash_mismatch",
            "El contenido instalado no coincide con su manifiesto",
        ));
    }

    let temporary = tempfile::Builder::new()
        .prefix(".larm-export-")
        .tempdir_in(parent)
        .map_err(io_error)?;
    let temporary_archive = temporary.path().join("package.zip");
    let file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temporary_archive)
        .map_err(io_error)?;
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Stored)
        .last_modified_time(zip::DateTime::default());
    let mut archive = ZipWriter::new(file);
    for item in &files {
        control.check()?;
        archive
            .start_file(&item.archive_path, options)
            .map_err(io_error)?;
        let mut source_file = File::open(&item.source).map_err(io_error)?;
        let (bytes, sha256) =
            copy_bounded(&mut source_file, &mut archive, item.bytes, control, false)?;
        if bytes != item.bytes || sha256 != item.sha256 {
            return Err(error(
                ErrorCode::IntegrityFailed,
                "package_changed_during_export",
                "El paquete cambió durante la exportación",
            ));
        }
    }
    let packed_file = archive.finish().map_err(io_error)?;
    packed_file.sync_all().map_err(io_error)?;
    drop(packed_file);

    let mut packed_file = File::open(&temporary_archive).map_err(io_error)?;
    let (archive_bytes, archive_hash) = copy_bounded(
        &mut packed_file,
        &mut std::io::sink(),
        MAX_BYTES,
        control,
        false,
    )?;
    let plan = ArchivePlan::inspect(&temporary_archive, &archive_hash, control)?;
    if plan.manifest != manifest || plan.unpacked_bytes != total {
        return Err(error(
            ErrorCode::IntegrityFailed,
            "package_export_verification_failed",
            "El ZIP exportado no coincide con el paquete instalado",
        ));
    }
    verify_published(source, &manifest, control)?;
    control.check()?;
    check_managed_path(destination)?;
    if destination.exists() {
        return Err(error(
            ErrorCode::Conflict,
            "export_destination_exists",
            "El archivo de destino ya existe",
        ));
    }
    std::fs::rename(&temporary_archive, destination).map_err(io_error)?;
    control.check()?;
    Ok(ExportedPackage {
        destination: destination.to_path_buf(),
        archive_hash,
        archive_bytes,
        manifest,
    })
}

fn read_manifest(path: &Path, control: &PackageControl) -> Result<AdapterManifest, LarmError> {
    control.check()?;
    check_managed_path(path)?;
    let mut file = File::open(path).map_err(io_error)?;
    let mut data = Vec::new();
    copy_bounded(&mut file, &mut data, MANIFEST_BYTES, control, false)?;
    AdapterManifest::parse(
        std::str::from_utf8(&data).map_err(|_| invalid("package_manifest_encoding"))?,
    )
}
impl Drop for PublishedPackage {
    fn drop(&mut self) {
        if !self.committed && check_managed_path(&self.destination).is_ok() {
            if let Err(e) = std::fs::remove_dir_all(&self.destination) {
                tracing::error!(reason = "package_rollback_failed", error = %e.kind(), "no se pudo retirar un paquete provisional");
            }
        }
    }
}
