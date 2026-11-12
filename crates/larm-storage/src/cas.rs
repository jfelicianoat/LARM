//! Content Addressed Store y hash de ficheros (P017; spec §39, §40, §95;
//! contrato 02).
//!
//! - SHA-256 en streaming. La caché por tamaño + mtime solo evita volver a
//!   leer un fichero que no ha cambiado; nunca es la identidad final: una
//!   verificación siempre relee los bytes.
//! - Layout `root/blobs/sha256/ab/<sha256>`. Un blob es inmutable: se
//!   escribe por copia a staging, se verifica su hash y se coloca de solo
//!   lectura sin sobrescribir. Si ya existe con ese hash, no se duplica.
//! - Vista para runtimes (§40): los nombres originales enlazados a los
//!   blobs con hard links cuando están en el mismo volumen y el sistema de
//!   ficheros los admite; si no, la vista lista referencias separadas a los
//!   blobs (nunca copia un blob compartido ni lo modifica).

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const BUFFER: usize = 8 << 20;

/// SHA-256 y tamaño leyendo el fichero en bloques.
///
/// # Errors
/// E/S.
pub fn sha256_file(path: &Path) -> std::io::Result<(String, u64)> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; BUFFER];
    let mut total = 0u64;
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        total += n as u64;
    }
    Ok((hex::encode(hasher.finalize()), total))
}

/// Huella barata de un fichero: tamaño y mtime en nanosegundos.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stamp {
    pub size: u64,
    pub mtime_ns: u128,
}

/// Huella actual del fichero.
///
/// # Errors
/// E/S o mtime anterior a 1970.
pub fn stamp(path: &Path) -> std::io::Result<Stamp> {
    let meta = std::fs::metadata(path)?;
    let mtime_ns = meta
        .modified()?
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(std::io::Error::other)?
        .as_nanos();
    Ok(Stamp {
        size: meta.len(),
        mtime_ns,
    })
}

/// Entrada de la caché de hash (se guarda en `metadata.hash_cache`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HashCache {
    pub stamp: Stamp,
    pub sha256: String,
}

/// Hash del fichero y si salió de la caché.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hashed {
    pub sha256: String,
    pub size: u64,
    pub cache: HashCache,
    pub from_cache: bool,
}

/// Usa la caché solo si tamaño y mtime coinciden exactamente; si no, relee.
///
/// # Errors
/// E/S.
pub fn hash_with_cache(path: &Path, cache: Option<&HashCache>) -> std::io::Result<Hashed> {
    let before = stamp(path)?;
    if let Some(c) = cache.filter(|c| c.stamp == before) {
        return Ok(Hashed {
            sha256: c.sha256.clone(),
            size: before.size,
            cache: c.clone(),
            from_cache: true,
        });
    }
    let (sha256, size) = sha256_file(path)?;
    // Si cambió mientras se leía, la huella no sirve para la próxima vez.
    let after = stamp(path)?;
    if after != before || after.size != size {
        return Err(std::io::Error::other(
            "el fichero cambió mientras se calculaba su hash",
        ));
    }
    Ok(Hashed {
        cache: HashCache {
            stamp: after,
            sha256: sha256.clone(),
        },
        sha256,
        size,
        from_cache: false,
    })
}

fn is_sha256(v: &str) -> bool {
    v.len() == 64
        && v.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Ruta relativa (POSIX) del blob dentro de la raíz.
///
/// # Errors
/// Hash no sha256 en minúsculas.
pub fn blob_relative(sha256: &str) -> std::io::Result<String> {
    if !is_sha256(sha256) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "hash no sha256",
        ));
    }
    Ok(format!("blobs/sha256/{}/{sha256}", &sha256[..2]))
}

fn native(root: &Path, relative: &str) -> PathBuf {
    relative
        .split('/')
        .fold(root.to_path_buf(), |p, c| p.join(c))
}

/// Cómo quedó un fichero en el CAS.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Ingested {
    /// Copiado y verificado.
    Copied,
    /// Ya había un blob con ese hash: no se duplica.
    AlreadyPresent,
}

/// Artefacto que ya tiene una ubicación inmutable en el CAS.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromotedArtifact {
    pub relative_path: String,
    pub sha256: String,
    pub size_bytes: u64,
    pub state: PromotionState,
}

/// Resultado de colocar un fichero verificado en el CAS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromotionState {
    /// El staging se renombró atómicamente; no se duplicaron sus bytes.
    Promoted,
    /// El blob verificado ya estaba en el CAS.
    AlreadyPresent,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PromotionReceipt {
    job_id: String,
    artifact_id: String,
    plan_hash: String,
    sha256: String,
    size_bytes: u64,
}

fn is_reparse(meta: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        meta.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        meta.file_type().is_symlink()
    }
}

fn checked_dir(path: &Path) -> std::io::Result<()> {
    let meta = std::fs::symlink_metadata(path)?;
    if !meta.is_dir() || is_reparse(&meta) {
        return Err(std::io::Error::other(
            "la ruta del CAS no es un directorio seguro",
        ));
    }
    Ok(())
}

fn ensure_child_dir(parent: &Path, name: &str) -> std::io::Result<PathBuf> {
    checked_dir(parent)?;
    let path = parent.join(name);
    match std::fs::create_dir(&path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error),
    }
    checked_dir(&path)?;
    Ok(path)
}

fn regular_file(path: &Path) -> std::io::Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.is_file() && !is_reparse(&meta) => Ok(true),
        Ok(_) => Err(std::io::Error::other(
            "el artefacto del CAS no es un fichero regular",
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

fn promotion_paths(
    root: &Path,
    job_id: &str,
    artifact_id: &str,
) -> std::io::Result<(PathBuf, PathBuf)> {
    if !root.is_absolute()
        || !larm_domain::bridge::is_uuid_v7(job_id)
        || !larm_domain::bridge::is_uuid_v7(artifact_id)
    {
        return Err(std::io::Error::other(
            "identidad o raíz de promoción no válida",
        ));
    }
    checked_dir(root)?;
    let staging = ensure_child_dir(root, "staging")?;
    let job_dir = staging.join(job_id);
    checked_dir(&job_dir)?;
    Ok((
        job_dir.join(format!("{artifact_id}.partial")),
        job_dir.join(format!("{artifact_id}.promote.json")),
    ))
}

fn write_promotion_receipt(path: &Path, receipt: &PromotionReceipt) -> std::io::Result<()> {
    if regular_file(path)? {
        let existing: PromotionReceipt = serde_json::from_slice(&std::fs::read(path)?)
            .map_err(|_| std::io::Error::other("recibo de promoción ilegible"))?;
        if &existing != receipt {
            return Err(std::io::Error::other(
                "el recibo de promoción pertenece a otro plan",
            ));
        }
        return Ok(());
    }
    let tmp = path.with_extension("promote.tmp");
    if regular_file(&tmp)? {
        std::fs::remove_file(&tmp)?;
    }
    let bytes = serde_json::to_vec(receipt)
        .map_err(|_| std::io::Error::other("no se pudo serializar el recibo de promoción"))?;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(tmp, path)?;
    Ok(())
}

fn load_promotion_receipt(path: &Path) -> std::io::Result<Option<PromotionReceipt>> {
    if !regular_file(path)? {
        return Ok(None);
    }
    serde_json::from_slice(&std::fs::read(path)?)
        .map(Some)
        .map_err(|_| std::io::Error::other("recibo de promoción ilegible"))
}

fn verify_blob(path: &Path, sha256: &str, size_bytes: u64) -> std::io::Result<()> {
    if !regular_file(path)? {
        return Err(std::io::Error::other("blob del CAS ausente o inseguro"));
    }
    let (actual_hash, actual_size) = sha256_file(path)?;
    if actual_hash != sha256 || actual_size != size_bytes {
        return Err(std::io::Error::other(
            "blob del CAS no coincide con su recibo",
        ));
    }
    Ok(())
}

fn recover_promotion(
    root: &Path,
    job_id: &str,
    artifact_id: &str,
    plan_hash: &str,
) -> std::io::Result<Option<PromotedArtifact>> {
    recover_promotion_with_rename(root, job_id, artifact_id, plan_hash, |from, to| {
        std::fs::rename(from, to)
    })
}

fn recover_promotion_with_rename(
    root: &Path,
    job_id: &str,
    artifact_id: &str,
    plan_hash: &str,
    rename: impl FnOnce(&Path, &Path) -> std::io::Result<()>,
) -> std::io::Result<Option<PromotedArtifact>> {
    if !root.is_absolute()
        || !larm_domain::bridge::is_uuid_v7(job_id)
        || !larm_domain::bridge::is_uuid_v7(artifact_id)
    {
        return Err(std::io::Error::other(
            "identidad o raíz de recuperación no válida",
        ));
    }
    checked_dir(root)?;
    let staging = root.join("staging");
    match std::fs::symlink_metadata(&staging) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
        Ok(meta) if meta.is_dir() && !is_reparse(&meta) => {}
        Ok(_) => return Err(std::io::Error::other("staging no es un directorio seguro")),
    }
    let job_dir = staging.join(job_id);
    match std::fs::symlink_metadata(&job_dir) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
        Ok(meta) if meta.is_dir() && !is_reparse(&meta) => {}
        Ok(_) => {
            return Err(std::io::Error::other(
                "el staging del job no es un directorio seguro",
            ))
        }
    }
    let staged = job_dir.join(format!("{artifact_id}.partial"));
    let receipt_path = job_dir.join(format!("{artifact_id}.promote.json"));
    let Some(receipt) = load_promotion_receipt(&receipt_path)? else {
        return Ok(None);
    };
    if receipt.job_id != job_id
        || receipt.artifact_id != artifact_id
        || receipt.plan_hash != plan_hash
        || !larm_domain::adapter_updates::is_sha256(&receipt.plan_hash)
        || !larm_domain::adapter_updates::is_sha256(&receipt.sha256)
        || receipt.size_bytes > larm_domain::units::DECIMAL_U64_MAX
    {
        return Err(std::io::Error::other(
            "el recibo de promoción no corresponde al job",
        ));
    }
    if !larm_domain::adapter_updates::is_sha256(&receipt.plan_hash)
        || !larm_domain::adapter_updates::is_sha256(&receipt.sha256)
        || receipt.size_bytes > larm_domain::units::DECIMAL_U64_MAX
    {
        return Err(std::io::Error::other("el recibo de promoción no es válido"));
    }
    let blobs = ensure_child_dir(root, "blobs")?;
    let sha_root = ensure_child_dir(&blobs, "sha256")?;
    let prefix = ensure_child_dir(&sha_root, &receipt.sha256[..2])?;
    let relative_path = blob_relative(&receipt.sha256)?;
    let destination = prefix.join(&receipt.sha256);
    let staged_exists = regular_file(&staged)?;
    let destination_exists = regular_file(&destination)?;
    let state = if destination_exists {
        verify_blob(&destination, &receipt.sha256, receipt.size_bytes)?;
        PromotionState::AlreadyPresent
    } else if staged_exists {
        verify_blob(&staged, &receipt.sha256, receipt.size_bytes)?;
        match rename(&staged, &destination) {
            Ok(()) => {
                verify_blob(&destination, &receipt.sha256, receipt.size_bytes)?;
                PromotionState::Promoted
            }
            Err(error) => {
                // Otra descarga puede haber publicado y protegido el mismo
                // blob tras nuestra observación. No convertir cualquier fallo
                // de rename en éxito: exigir archivo regular y hash/tamaño
                // completos; el parcial propio se retira sólo tras el commit.
                if !regular_file(&destination)? {
                    return Err(std::io::Error::new(
                        error.kind(),
                        format!("renombrando staging al CAS: {error}"),
                    ));
                }
                verify_blob(&destination, &receipt.sha256, receipt.size_bytes)?;
                PromotionState::AlreadyPresent
            }
        }
    } else {
        return Err(std::io::Error::other(
            "no hay staging ni blob para recuperar la promoción",
        ));
    };
    let mut permissions = std::fs::metadata(&destination)?.permissions();
    if !permissions.readonly() {
        permissions.set_readonly(true);
        std::fs::set_permissions(&destination, permissions).map_err(|error| {
            std::io::Error::new(error.kind(), format!("protegiendo blob promovido: {error}"))
        })?;
    }
    Ok(Some(PromotedArtifact {
        relative_path,
        sha256: receipt.sha256,
        size_bytes: receipt.size_bytes,
        state,
    }))
}

/// Promueve un staging verificado mediante rename en el mismo volumen. Antes
/// del rename persiste un recibo que permite recuperar el blob si el daemon
/// cae entre el movimiento y el commit de SQLite.
pub fn promote_staged(
    root: &Path,
    staged: &Path,
    job_id: &str,
    artifact_id: &str,
    plan_hash: &str,
    sha256: &str,
    size_bytes: u64,
) -> std::io::Result<PromotedArtifact> {
    if !larm_domain::adapter_updates::is_sha256(plan_hash)
        || !larm_domain::adapter_updates::is_sha256(sha256)
        || size_bytes > larm_domain::units::DECIMAL_U64_MAX
    {
        return Err(std::io::Error::other(
            "hash o tamaño de promoción no válido",
        ));
    }
    let (expected_staged, receipt_path) = promotion_paths(root, job_id, artifact_id)?;
    if staged != expected_staged {
        return Err(std::io::Error::other("staging fuera del job de descarga"));
    }
    verify_blob(staged, sha256, size_bytes).map_err(|error| {
        std::io::Error::new(
            error.kind(),
            format!("verificando staging antes del CAS: {error}"),
        )
    })?;
    write_promotion_receipt(
        &receipt_path,
        &PromotionReceipt {
            job_id: job_id.into(),
            artifact_id: artifact_id.into(),
            plan_hash: plan_hash.into(),
            sha256: sha256.into(),
            size_bytes,
        },
    )
    .map_err(|error| {
        std::io::Error::new(
            error.kind(),
            format!("guardando recibo de promoción: {error}"),
        )
    })?;
    recover_promotion(root, job_id, artifact_id, plan_hash)?
        .ok_or_else(|| std::io::Error::other("no se pudo recuperar la promoción"))
}

/// Recupera una promoción interrumpida. `None` significa que el artefacto aún
/// no había iniciado la fase de ingreso al CAS.
pub fn recover_staged_promotion(
    root: &Path,
    job_id: &str,
    artifact_id: &str,
    plan_hash: &str,
) -> std::io::Result<Option<PromotedArtifact>> {
    recover_promotion(root, job_id, artifact_id, plan_hash)
}

/// Retira el temporal y recibo solo después de que SQLite confirme el job.
pub fn finish_staged_promotion(
    root: &Path,
    job_id: &str,
    artifact_id: &str,
    plan_hash: &str,
) -> std::io::Result<()> {
    let (staged, receipt_path) = promotion_paths(root, job_id, artifact_id)?;
    let Some(receipt) = load_promotion_receipt(&receipt_path)? else {
        return Ok(());
    };
    if receipt.job_id != job_id
        || receipt.artifact_id != artifact_id
        || receipt.plan_hash != plan_hash
        || !larm_domain::adapter_updates::is_sha256(&receipt.plan_hash)
        || !larm_domain::adapter_updates::is_sha256(&receipt.sha256)
        || receipt.size_bytes > larm_domain::units::DECIMAL_U64_MAX
    {
        return Err(std::io::Error::other(
            "el recibo de promoción no corresponde al job o contiene datos no válidos",
        ));
    }
    let blobs = root.join("blobs");
    let sha_root = blobs.join("sha256");
    let prefix = sha_root.join(&receipt.sha256[..2]);
    checked_dir(&blobs)?;
    checked_dir(&sha_root)?;
    checked_dir(&prefix)?;
    verify_blob(
        &prefix.join(&receipt.sha256),
        &receipt.sha256,
        receipt.size_bytes,
    )?;
    if regular_file(&staged)? {
        std::fs::remove_file(&staged)?;
    }
    std::fs::remove_file(receipt_path)?;
    if std::fs::read_dir(
        staged
            .parent()
            .ok_or_else(|| std::io::Error::other("staging sin padre"))?,
    )?
    .next()
    .is_none()
    {
        std::fs::remove_dir(
            staged
                .parent()
                .ok_or_else(|| std::io::Error::other("staging sin padre"))?,
        )?;
        let staging = root.join("staging");
        if std::fs::read_dir(&staging)?.next().is_none() {
            std::fs::remove_dir(staging)?;
        }
    }
    Ok(())
}

/// Mete una copia verificada de `source` en el CAS de `root`. El blob queda
/// de solo lectura. Si ya existe uno con ese hash (y tamaño), no se copia.
///
/// # Errors
/// E/S, hash distinto del esperado (la copia se descarta) o un blob
/// existente con otro tamaño (CAS dañado: no se toca).
pub fn ingest(
    root: &Path,
    source: &Path,
    sha256: &str,
    job_id: &str,
) -> std::io::Result<(String, Ingested)> {
    let relative = blob_relative(sha256)?;
    let dest = native(root, &relative);
    let expected_size = std::fs::metadata(source)?.len();
    if let Ok(meta) = std::fs::metadata(&dest) {
        if meta.len() != expected_size {
            return Err(std::io::Error::other(
                "el CAS tiene un blob con ese hash y otro tamaño",
            ));
        }
        return Ok((relative, Ingested::AlreadyPresent));
    }
    let staging = root.join("staging").join(job_id);
    std::fs::create_dir_all(&staging)?;
    let tmp = staging.join(format!("{sha256}.partial"));
    let copied = (|| -> std::io::Result<String> {
        let mut src = std::fs::File::open(source)?;
        let mut out = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)?;
        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; BUFFER];
        loop {
            let n = src.read(&mut buf)?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
            out.write_all(&buf[..n])?;
        }
        out.sync_all()?;
        Ok(hex::encode(hasher.finalize()))
    })();
    let result = match copied {
        Ok(hash) if hash == sha256 && sha256_file(&tmp)?.0 == sha256 => {
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)?;
            }
            if std::fs::symlink_metadata(&dest).is_ok() {
                // Otro proceso lo colocó mientras copiábamos: vale el suyo.
                Ok((relative, Ingested::AlreadyPresent))
            } else {
                std::fs::rename(&tmp, &dest)?;
                let mut perms = std::fs::metadata(&dest)?.permissions();
                perms.set_readonly(true);
                std::fs::set_permissions(&dest, perms)?;
                Ok((relative, Ingested::Copied))
            }
        }
        Ok(_) => Err(std::io::Error::other(
            "la copia no coincide con el hash esperado",
        )),
        Err(e) => Err(e),
    };
    let _ = std::fs::remove_file(&tmp);
    let _ = std::fs::remove_dir(&staging);
    let _ = std::fs::remove_dir(root.join("staging"));
    result
}

/// Cómo quedó cada nombre de una vista para runtimes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ViewEntry {
    /// Hard link al blob (mismo volumen, sistema de ficheros que lo admite).
    Linked { name: String },
    /// Sin enlace posible: el runtime recibe la ruta del blob.
    Reference {
        name: String,
        blob: String,
        reason: String,
    },
}

/// Construye la vista `view_dir/<nombre>` de los blobs del CAS de `root`.
/// Intenta un hard link por fichero; si no se puede (otro volumen, FAT,
/// recurso de red), deja una referencia separada al blob. Escribe
/// `view.json` con el resultado. Nunca copia ni modifica un blob.
///
/// # Errors
/// E/S al crear la carpeta o el manifiesto, o un nombre que ya existe en la
/// vista con otro contenido.
pub fn runtime_view(
    root: &Path,
    view_dir: &Path,
    files: &[(String, String)],
) -> std::io::Result<Vec<ViewEntry>> {
    std::fs::create_dir_all(view_dir)?;
    let mut out = Vec::with_capacity(files.len());
    for (name, sha256) in files {
        let blob = native(root, &blob_relative(sha256)?);
        if !blob.is_file() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("falta el blob de {name}"),
            ));
        }
        let target = native(view_dir, name);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if target.exists() {
            if sha256_file(&target)?.0 == *sha256 {
                out.push(ViewEntry::Linked { name: name.clone() });
                continue;
            }
            return Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                format!("{name} ya existe en la vista con otro contenido"),
            ));
        }
        match std::fs::hard_link(&blob, &target) {
            Ok(()) => out.push(ViewEntry::Linked { name: name.clone() }),
            Err(e) => out.push(ViewEntry::Reference {
                name: name.clone(),
                blob: blob.display().to_string(),
                reason: e.kind().to_string(),
            }),
        }
    }
    let manifest = serde_json::to_vec_pretty(&out).map_err(std::io::Error::other)?;
    std::fs::write(view_dir.join("view.json"), manifest)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    #[test]
    fn hash_en_streaming_y_cache() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("a.bin");
        let data: Vec<u8> = (0..(BUFFER + 123)).map(|i| (i % 251) as u8).collect();
        std::fs::write(&f, &data).unwrap();
        let expected = hex::encode(Sha256::digest(&data));
        let first = hash_with_cache(&f, None).unwrap();
        assert_eq!(
            (first.sha256.as_str(), first.size, first.from_cache),
            (expected.as_str(), data.len() as u64, false)
        );
        let again = hash_with_cache(&f, Some(&first.cache)).unwrap();
        assert!(again.from_cache, "misma huella: no se relee");
        // Cambia el contenido con el mismo tamaño: la huella (mtime) cambia.
        std::thread::sleep(std::time::Duration::from_millis(20));
        let mut changed = data.clone();
        changed[0] ^= 1;
        std::fs::write(&f, &changed).unwrap();
        let fresh = hash_with_cache(&f, Some(&first.cache)).unwrap();
        assert!(!fresh.from_cache);
        assert_ne!(fresh.sha256, expected);
        // Una caché falsa con huella distinta nunca se usa.
        let fake = HashCache {
            stamp: Stamp {
                size: 1,
                mtime_ns: 1,
            },
            sha256: "0".repeat(64),
        };
        assert_eq!(
            hash_with_cache(&f, Some(&fake)).unwrap().sha256,
            fresh.sha256
        );
    }

    #[test]
    fn cas_inmutable_y_sin_duplicar() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("cas");
        let src = dir.path().join("modelo.gguf");
        std::fs::write(&src, b"GGUF contenido").unwrap();
        let sha = sha256_file(&src).unwrap().0;
        let (rel, how) = ingest(&root, &src, &sha, "job-1").unwrap();
        assert_eq!(rel, format!("blobs/sha256/{}/{sha}", &sha[..2]));
        assert_eq!(how, Ingested::Copied);
        let blob = native(&root, &rel);
        assert!(
            std::fs::metadata(&blob).unwrap().permissions().readonly(),
            "inmutable"
        );
        assert!(!root.join("staging").exists(), "sin provisionales");
        // Mismo contenido desde otro sitio: no se duplica.
        let other = dir.path().join("copia.gguf");
        std::fs::copy(&src, &other).unwrap();
        assert_eq!(
            ingest(&root, &other, &sha, "job-2").unwrap().1,
            Ingested::AlreadyPresent
        );
        // Hash esperado falso: se descarta la copia y no queda blob.
        let err = ingest(&root, &src, &"a".repeat(64), "job-3").unwrap_err();
        assert!(err.to_string().contains("no coincide"));
        assert!(!native(&root, &blob_relative(&"a".repeat(64)).unwrap()).exists());
        assert!(blob_relative("ABC").is_err());
    }

    #[test]
    fn vista_con_enlaces_y_reutilizable() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("cas");
        let src = dir.path().join("w.gguf");
        std::fs::write(&src, b"pesos").unwrap();
        let sha = sha256_file(&src).unwrap().0;
        ingest(&root, &src, &sha, "j").unwrap();
        let view = dir.path().join("vista");
        let entries = runtime_view(&root, &view, &[("modelo/w.gguf".into(), sha.clone())]).unwrap();
        // En el mismo volumen NTFS del temporal, enlace; el manifiesto lo dice.
        assert_eq!(
            entries,
            vec![ViewEntry::Linked {
                name: "modelo/w.gguf".into()
            }]
        );
        assert_eq!(
            std::fs::read(view.join("modelo").join("w.gguf")).unwrap(),
            b"pesos"
        );
        assert!(view.join("view.json").exists());
        // Repetir es idempotente; un blob inexistente es un error.
        assert_eq!(
            runtime_view(&root, &view, &[("modelo/w.gguf".into(), sha)])
                .unwrap()
                .len(),
            1
        );
        assert!(runtime_view(&root, &view, &[("x".into(), "b".repeat(64))]).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn promociones_concurrentes_reutilizan_blob_que_aparece_antes_del_rename() {
        let owned = tempfile::tempdir().unwrap();
        let root = owned.path().join("cas");
        std::fs::create_dir(&root).unwrap();
        let jobs = [
            "0190f5c0-7a00-7000-8000-000000000001",
            "0190f5c0-7a00-7000-8000-000000000003",
        ];
        let artifacts = [
            "0190f5c0-7a00-7000-8000-000000000002",
            "0190f5c0-7a00-7000-8000-000000000004",
        ];
        let content = b"two independently verified downloads, one immutable blob";
        let plan_hash = "a".repeat(64);
        let sha = hex::encode(sha2::Sha256::digest(content));
        for (job, artifact) in jobs.iter().zip(artifacts.iter()) {
            let dir = root.join("staging").join(job);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join(format!("{artifact}.partial")), content).unwrap();
            write_promotion_receipt(
                &dir.join(format!("{artifact}.promote.json")),
                &PromotionReceipt {
                    job_id: (*job).into(),
                    artifact_id: (*artifact).into(),
                    plan_hash: plan_hash.clone(),
                    sha256: sha.clone(),
                    size_bytes: content.len() as u64,
                },
            )
            .unwrap();
        }
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let (done, published) = std::sync::mpsc::channel();
        let first_root = root.clone();
        let first_hash = plan_hash.clone();
        let first_barrier = barrier.clone();
        let first = std::thread::spawn(move || {
            let result = recover_promotion_with_rename(
                &first_root,
                jobs[0],
                artifacts[0],
                &first_hash,
                |from, to| {
                    first_barrier.wait();
                    std::fs::rename(from, to)
                },
            );
            done.send(()).unwrap();
            result
        });
        let second_root = root.clone();
        let second_hash = plan_hash.clone();
        let second = std::thread::spawn(move || {
            recover_promotion_with_rename(
                &second_root,
                jobs[1],
                artifacts[1],
                &second_hash,
                |from, to| {
                    // Both operations observed the destination absent. Let the
                    // first finish protecting its blob before the second rename.
                    barrier.wait();
                    published
                        .recv_timeout(std::time::Duration::from_secs(5))
                        .unwrap();
                    std::fs::rename(from, to)
                },
            )
        });
        assert_eq!(
            first.join().unwrap().unwrap().unwrap().state,
            PromotionState::Promoted
        );
        let second = second.join().unwrap();
        assert!(
            second.is_ok(),
            "a concurrent valid blob must be reused: {second:?}"
        );
        assert_eq!(
            second.unwrap().unwrap().state,
            PromotionState::AlreadyPresent
        );
        assert_eq!(
            std::fs::read(root.join(blob_relative(&sha).unwrap())).unwrap(),
            content
        );
        assert!(root
            .join("staging")
            .join(jobs[1])
            .join(format!("{}.partial", artifacts[1]))
            .exists());
        for (job, artifact) in jobs.iter().zip(artifacts.iter()) {
            finish_staged_promotion(&root, job, artifact, &plan_hash).unwrap();
        }
    }

    #[test]
    fn colision_de_rename_no_acepta_blob_corrupto_ni_retira_parcial() {
        let owned = tempfile::tempdir().unwrap();
        let root = owned.path().join("cas");
        let job = "0190f5c0-7a00-7000-8000-000000000001";
        let artifact = "0190f5c0-7a00-7000-8000-000000000002";
        let dir = root.join("staging").join(job);
        std::fs::create_dir_all(&dir).unwrap();
        let content = b"verified input";
        let staged = dir.join(format!("{artifact}.partial"));
        std::fs::write(&staged, content).unwrap();
        let sha = sha256_file(&staged).unwrap().0;
        let plan_hash = "a".repeat(64);
        let receipt = dir.join(format!("{artifact}.promote.json"));
        write_promotion_receipt(
            &receipt,
            &PromotionReceipt {
                job_id: job.into(),
                artifact_id: artifact.into(),
                plan_hash: plan_hash.clone(),
                sha256: sha,
                size_bytes: content.len() as u64,
            },
        )
        .unwrap();
        let result =
            recover_promotion_with_rename(&root, job, artifact, &plan_hash, |_, destination| {
                std::fs::write(destination, b"corrupt input!")?;
                Err(std::io::Error::new(
                    std::io::ErrorKind::AlreadyExists,
                    "injected publication collision",
                ))
            });
        assert!(result.is_err(), "a competing corrupt blob cannot be reused");
        assert_eq!(std::fs::read(&staged).unwrap(), content);
        assert!(receipt.exists());
    }

    #[test]
    fn promocion_por_rename_se_recupera_tras_caida_y_limpia_tras_commit() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("cas");
        std::fs::create_dir(&root).unwrap();
        let job = "0190f5c0-7a00-7000-8000-000000000001";
        let artifact = "0190f5c0-7a00-7000-8000-000000000002";
        let plan_hash = "a".repeat(64);
        let job_dir = root.join("staging").join(job);
        std::fs::create_dir_all(&job_dir).unwrap();
        let staged = job_dir.join(format!("{artifact}.partial"));
        let content = b"bytes already admitted in staging";
        std::fs::write(&staged, content).unwrap();
        let sha = sha256_file(&staged).unwrap().0;

        let promoted = promote_staged(
            &root,
            &staged,
            job,
            artifact,
            &plan_hash,
            &sha,
            content.len() as u64,
        )
        .unwrap();
        assert_eq!(promoted.relative_path, blob_relative(&sha).unwrap());
        assert_eq!(promoted.state, PromotionState::Promoted);
        assert!(
            !staged.exists(),
            "rename libera el staging sin copiar bytes"
        );
        assert_eq!(
            std::fs::read(native(&root, &promoted.relative_path)).unwrap(),
            content
        );
        assert!(job_dir.join(format!("{artifact}.promote.json")).exists());

        let recovered = recover_staged_promotion(&root, job, artifact, &plan_hash)
            .unwrap()
            .unwrap();
        assert_eq!(recovered.sha256, sha);
        assert_eq!(recovered.size_bytes, content.len() as u64);
        assert_eq!(recovered.state, PromotionState::AlreadyPresent);
        assert!(recover_staged_promotion(&root, job, artifact, &"b".repeat(64)).is_err());

        finish_staged_promotion(&root, job, artifact, &plan_hash).unwrap();
        assert!(!job_dir.exists());
        assert!(!root.join("staging").exists());
        assert!(native(&root, &promoted.relative_path).exists());
    }

    #[test]
    fn recibo_durable_reconstruye_promocion_interrumpida_antes_del_rename() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("cas");
        std::fs::create_dir(&root).unwrap();
        let job = "0190f5c0-7a00-7000-8000-000000000011";
        let artifact = "0190f5c0-7a00-7000-8000-000000000012";
        let plan_hash = "c".repeat(64);
        let job_dir = root.join("staging").join(job);
        std::fs::create_dir_all(&job_dir).unwrap();
        let staged = job_dir.join(format!("{artifact}.partial"));
        let content = b"recoverable staged content";
        std::fs::write(&staged, content).unwrap();
        let sha = sha256_file(&staged).unwrap().0;
        let receipt_path = job_dir.join(format!("{artifact}.promote.json"));
        write_promotion_receipt(
            &receipt_path,
            &PromotionReceipt {
                job_id: job.into(),
                artifact_id: artifact.into(),
                plan_hash: plan_hash.clone(),
                sha256: sha.clone(),
                size_bytes: content.len() as u64,
            },
        )
        .unwrap();

        let recovered = recover_staged_promotion(&root, job, artifact, &plan_hash)
            .unwrap()
            .unwrap();
        assert_eq!(recovered.state, PromotionState::Promoted);
        assert_eq!(recovered.sha256, sha);
        assert!(!staged.exists());
        verify_blob(
            &native(&root, &recovered.relative_path),
            &sha,
            content.len() as u64,
        )
        .unwrap();
    }

    #[test]
    fn limpieza_rechaza_recibo_corrupto_antes_de_derivar_ruta_cas() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("cas");
        let job = "0190f5c0-7a00-7000-8000-000000000031";
        let artifact = "0190f5c0-7a00-7000-8000-000000000032";
        let job_dir = root.join("staging").join(job);
        std::fs::create_dir_all(&job_dir).unwrap();
        std::fs::write(
            job_dir.join(format!("{artifact}.promote.json")),
            serde_json::json!({
                "job_id": job,
                "artifact_id": artifact,
                "plan_hash": "a".repeat(64),
                "sha256": "x",
                "size_bytes": 1,
            })
            .to_string(),
        )
        .unwrap();

        assert!(finish_staged_promotion(&root, job, artifact, &"a".repeat(64)).is_err());
    }

    #[test]
    fn promocion_rechaza_hash_incorrecto_y_no_sigue_enlaces() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("cas");
        std::fs::create_dir(&root).unwrap();
        let job = "0190f5c0-7a00-7000-8000-000000000021";
        let artifact = "0190f5c0-7a00-7000-8000-000000000022";
        let job_dir = root.join("staging").join(job);
        std::fs::create_dir_all(&job_dir).unwrap();
        let staged = job_dir.join(format!("{artifact}.partial"));
        std::fs::write(&staged, b"real bytes").unwrap();
        assert!(promote_staged(
            &root,
            &staged,
            job,
            artifact,
            &"d".repeat(64),
            &"e".repeat(64),
            10
        )
        .is_err());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(dir.path().join("outside"), &staged).unwrap();
            assert!(promote_staged(
                &root,
                &staged,
                job,
                artifact,
                &"d".repeat(64),
                &"e".repeat(64),
                10
            )
            .is_err());
        }
        assert!(!root.join("blobs").exists());
    }
}
