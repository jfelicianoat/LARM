//! Permisos, firma y confianza de adaptadores (P022; spec §12, §97, §131;
//! contrato 03).
//!
//! - **Permisos concretos**: raíces de lectura y escritura, orígenes de red,
//!   ejecutables y referencias de secretos. Se comprueban en cada operación
//!   sensible: un enum sin comprobación no vale.
//! - **Delta**: lo que un manifiesto nuevo pide de más frente a lo ya
//!   aceptado. Cualquier elevación necesita aprobación.
//! - **Firma**: Ed25519 sobre el RFC 8785 del manifiesto con
//!   `signature: null`, verificada con claves fijadas localmente por el
//!   operador. Una clave o un paquete revocados bloquean.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use ed25519_dalek::{Signature as EdSignature, Signer, SigningKey, Verifier, VerifyingKey};
use larm_domain::error::{ErrorCode, LarmError};
use serde::{Deserialize, Serialize};

use crate::manifest::{AdapterManifest, Permissions, Signature, SignatureAlgorithm};

fn denied(reason: &str, message: impl Into<String>) -> LarmError {
    LarmError::new(ErrorCode::Forbidden, message, reason)
}

/// Raíces simbólicas que el host resuelve a rutas reales.
#[derive(Debug, Clone, Default)]
pub struct RootMap(pub Vec<(String, PathBuf)>);

impl RootMap {
    fn resolve(&self, root: &str) -> Option<PathBuf> {
        if let Some((_, p)) = self.0.iter().find(|(k, _)| k == root) {
            return Some(p.clone());
        }
        let p = PathBuf::from(root);
        p.is_absolute().then_some(p)
    }
}

/// Comparación por componentes y sin distinguir mayúsculas (Windows).
fn components(path: &Path) -> Vec<String> {
    path.components()
        .filter_map(|c| match c {
            std::path::Component::Normal(s) => Some(s.to_string_lossy().to_lowercase()),
            std::path::Component::Prefix(p) => Some(
                p.as_os_str()
                    .to_string_lossy()
                    .to_lowercase()
                    .replace(r"\\?\", ""),
            ),
            _ => None,
        })
        .collect()
}

fn within(root: &Path, path: &Path) -> bool {
    let (r, p) = (components(root), components(path));
    r.len() <= p.len() && r.iter().zip(&p).all(|(a, b)| a == b)
}

/// Ruta real sin `..` ni enlaces: la existente se canonicaliza; una que aún
/// no existe (escritura) se resuelve por su carpeta padre.
fn real(path: &Path) -> Option<PathBuf> {
    if path
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return None;
    }
    if let Ok(p) = std::fs::canonicalize(path) {
        return Some(p);
    }
    let parent = path.parent()?;
    let name = path.file_name()?;
    std::fs::canonicalize(parent).ok().map(|p| p.join(name))
}

/// Permisos concedidos a un worker, ya resueltos.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Grants {
    pub read_roots: Vec<PathBuf>,
    pub write_roots: Vec<PathBuf>,
    pub network_origins: BTreeSet<String>,
    pub process_allowlist: BTreeSet<String>,
    pub secret_refs: BTreeSet<String>,
}

/// Lo que una operación va a tocar (lo declara el host al llamar).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Access {
    pub reads: Vec<PathBuf>,
    pub writes: Vec<PathBuf>,
    pub urls: Vec<String>,
    pub processes: Vec<String>,
    pub secrets: Vec<String>,
}

fn origin_of(url: &str) -> Option<String> {
    let (scheme, rest) = url.split_once("://")?;
    if !matches!(scheme, "http" | "https") {
        return None;
    }
    let authority = rest.split(['/', '?', '#']).next()?;
    if authority.is_empty() || authority.contains('@') {
        return None;
    }
    let lower = authority.to_lowercase();
    let with_port = if lower.contains(':') {
        lower
    } else {
        format!("{lower}:{}", if scheme == "https" { 443 } else { 80 })
    };
    Some(format!("{scheme}://{with_port}"))
}

impl Grants {
    /// Resuelve los permisos del manifiesto. Una raíz que no se puede
    /// resolver no concede nada.
    #[must_use]
    pub fn resolve(permissions: &Permissions, roots: &RootMap) -> Self {
        let paths = |list: &[String]| {
            list.iter()
                .filter_map(|r| roots.resolve(r))
                .filter_map(|p| real(&p))
                .collect()
        };
        Self {
            read_roots: paths(&permissions.read_roots),
            write_roots: paths(&permissions.write_roots),
            network_origins: permissions
                .network_origins
                .iter()
                .filter_map(|o| origin_of(o))
                .collect(),
            process_allowlist: permissions
                .process_allowlist
                .iter()
                .map(|p| p.replace('\\', "/").to_lowercase())
                .collect(),
            secret_refs: permissions.secret_refs.iter().cloned().collect(),
        }
    }

    /// Comprueba una operación antes de enviarla al worker.
    ///
    /// # Errors
    /// `FORBIDDEN` con `read_denied`, `write_denied`, `network_denied`,
    /// `process_denied` o `secret_denied`.
    pub fn check(&self, access: &Access) -> Result<(), LarmError> {
        for p in &access.reads {
            let ok = real(p).is_some_and(|rp| {
                self.read_roots
                    .iter()
                    .chain(&self.write_roots)
                    .any(|r| within(r, &rp))
            });
            if !ok {
                return Err(denied(
                    "read_denied",
                    "El adaptador no tiene permiso para leer esa ruta",
                ));
            }
        }
        for p in &access.writes {
            if !real(p).is_some_and(|rp| self.write_roots.iter().any(|r| within(r, &rp))) {
                return Err(denied(
                    "write_denied",
                    "El adaptador no tiene permiso para escribir en esa ruta",
                ));
            }
        }
        for u in &access.urls {
            if !origin_of(u).is_some_and(|o| self.network_origins.contains(&o)) {
                return Err(denied(
                    "network_denied",
                    "El adaptador no tiene permiso para ese origen de red",
                ));
            }
        }
        for x in &access.processes {
            if !self
                .process_allowlist
                .contains(&x.replace('\\', "/").to_lowercase())
            {
                return Err(denied(
                    "process_denied",
                    "El adaptador no puede lanzar ese ejecutable",
                ));
            }
        }
        for s in &access.secrets {
            if !self.secret_refs.contains(s) {
                return Err(denied(
                    "secret_denied",
                    "El adaptador no tiene concedido ese secreto",
                ));
            }
        }
        Ok(())
    }
}

/// Lo que un manifiesto pide de más frente a lo aceptado.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct PermissionDelta {
    pub read_roots: Vec<String>,
    pub write_roots: Vec<String>,
    pub network_origins: Vec<String>,
    pub process_allowlist: Vec<String>,
    pub secret_refs: Vec<String>,
}

impl PermissionDelta {
    #[must_use]
    pub fn between(accepted: &Permissions, requested: &Permissions) -> Self {
        let added = |a: &[String], r: &[String]| -> Vec<String> {
            let have: BTreeSet<&String> = a.iter().collect();
            let mut v: Vec<String> = r.iter().filter(|x| !have.contains(x)).cloned().collect();
            v.sort();
            v.dedup();
            v
        };
        Self {
            read_roots: added(&accepted.read_roots, &requested.read_roots),
            write_roots: added(&accepted.write_roots, &requested.write_roots),
            network_origins: added(&accepted.network_origins, &requested.network_origins),
            process_allowlist: added(&accepted.process_allowlist, &requested.process_allowlist),
            secret_refs: added(&accepted.secret_refs, &requested.secret_refs),
        }
    }

    /// ¿Pide algo nuevo? Entonces hace falta aprobación.
    #[must_use]
    pub fn is_elevation(&self) -> bool {
        !(self.read_roots.is_empty()
            && self.write_roots.is_empty()
            && self.network_origins.is_empty()
            && self.process_allowlist.is_empty()
            && self.secret_refs.is_empty())
    }
}

/// Estado de firma (§131).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum SignatureState {
    Official,
    SignedThirdParty,
    Unsigned,
    /// Adaptador local del operador (declarativo o propio): sin firma de
    /// publicador, se trata como no firmado para ejecutar.
    Custom,
}

/// Clave de publicador fijada por el operador.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PinnedKey {
    pub key_id: String,
    pub publisher: String,
    /// Clave pública Ed25519 en hex (32 bytes).
    pub public_key: String,
    pub official: bool,
    pub revoked: bool,
}

/// Claves fijadas y revocaciones.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrustStore {
    pub keys: Vec<PinnedKey>,
    /// `package_sha256` revocados.
    pub revoked_packages: Vec<String>,
}

/// Payload que se firma: el manifiesto con `signature: null`, en RFC 8785.
///
/// # Errors
/// Manifiesto no serializable.
pub fn signing_payload(manifest: &AdapterManifest) -> Result<String, LarmError> {
    let mut m = manifest.clone();
    m.signature = None;
    let value = serde_json::to_value(&m).map_err(|e| {
        LarmError::new(
            ErrorCode::Internal,
            "Manifiesto no serializable",
            "serialize",
        )
        .with_technical(e.to_string())
    })?;
    larm_domain::canonical::canonicalize(&value).map_err(|e| {
        LarmError::new(ErrorCode::Internal, "No canonicalizable", "canonical").with_technical(e)
    })
}

/// Firma un manifiesto (herramienta del publicador y pruebas).
///
/// # Errors
/// Manifiesto no serializable.
pub fn sign(
    manifest: &AdapterManifest,
    key_id: &str,
    key: &SigningKey,
) -> Result<Signature, LarmError> {
    let payload = signing_payload(manifest)?;
    Ok(Signature {
        algorithm: SignatureAlgorithm::Ed25519,
        key_id: key_id.into(),
        value: hex::encode(key.sign(payload.as_bytes()).to_bytes()),
    })
}

impl TrustStore {
    /// Estado de firma del manifiesto.
    ///
    /// # Errors
    /// `FORBIDDEN` `signature_invalid` (firma que no verifica, clave
    /// desconocida o de otro publicador) o `revoked` (clave o paquete
    /// revocados: bloquea nuevas ejecuciones).
    pub fn verify(
        &self,
        manifest: &AdapterManifest,
        custom: bool,
    ) -> Result<SignatureState, LarmError> {
        if self.revoked_packages.contains(&manifest.package_sha256) {
            return Err(denied("revoked", "El paquete del adaptador está revocado"));
        }
        let Some(sig) = &manifest.signature else {
            return Ok(if custom {
                SignatureState::Custom
            } else {
                SignatureState::Unsigned
            });
        };
        let key = self
            .keys
            .iter()
            .find(|k| k.key_id == sig.key_id)
            .ok_or_else(|| {
                denied(
                    "signature_invalid",
                    "Firmado con una clave que el operador no ha fijado",
                )
            })?;
        if key.revoked {
            return Err(denied("revoked", "La clave del publicador está revocada"));
        }
        if key.publisher != manifest.publisher {
            return Err(denied(
                "signature_invalid",
                "La clave es de otro publicador",
            ));
        }
        let public: [u8; 32] = hex::decode(&key.public_key)
            .ok()
            .and_then(|b| b.try_into().ok())
            .ok_or_else(|| denied("signature_invalid", "Clave pública fijada mal formada"))?;
        let bytes: [u8; 64] = hex::decode(&sig.value)
            .ok()
            .and_then(|b| b.try_into().ok())
            .ok_or_else(|| denied("signature_invalid", "Firma mal formada"))?;
        let verifying = VerifyingKey::from_bytes(&public)
            .map_err(|_| denied("signature_invalid", "Clave pública no válida"))?;
        verifying
            .verify(
                signing_payload(manifest)?.as_bytes(),
                &EdSignature::from_bytes(&bytes),
            )
            .map_err(|_| denied("signature_invalid", "La firma no corresponde al manifiesto"))?;
        Ok(if key.official {
            SignatureState::Official
        } else {
            SignatureState::SignedThirdParty
        })
    }
}

/// Confianza persistida en `adapter_installation.trust`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Trust {
    Untrusted,
    Verified,
    Revoked,
}

/// Resultado de evaluar un manifiesto: estado de firma (§131) y confianza.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Assessment {
    /// `None` cuando está revocado: no se llega a valorar la firma.
    pub signature_state: Option<SignatureState>,
    pub trust: Trust,
}

impl TrustStore {
    /// Como [`TrustStore::verify`], pero una revocación no es un error: el
    /// adaptador queda `REVOKED` (inspeccionable, no ejecutable).
    ///
    /// # Errors
    /// `FORBIDDEN` `signature_invalid`: una firma que no verifica no se
    /// instala.
    pub fn assess(
        &self,
        manifest: &AdapterManifest,
        custom: bool,
    ) -> Result<Assessment, LarmError> {
        match self.verify(manifest, custom) {
            Ok(state) => Ok(Assessment {
                signature_state: Some(state),
                trust: match state {
                    SignatureState::Official | SignatureState::SignedThirdParty => Trust::Verified,
                    SignatureState::Unsigned | SignatureState::Custom => Trust::Untrusted,
                },
            }),
            Err(e) if e.details.reason == "revoked" => Ok(Assessment {
                signature_state: None,
                trust: Trust::Revoked,
            }),
            Err(e) => Err(e),
        }
    }
}

/// Hash del paquete: sha256 de la lista ordenada `ruta\0sha256\n` de todos
/// sus ficheros, con rutas relativas en `/` y minúsculas. Se excluye
/// `exclude` (el manifiesto, que contiene este mismo hash).
///
/// # Errors
/// `RESOURCE_UNAVAILABLE` `io_failed` si no se puede leer el paquete.
pub fn package_hash(dir: &Path, exclude: &[&str]) -> Result<String, LarmError> {
    package_hash_checked(dir, exclude, &mut |_, _, _| Ok(()))
}

/// Misma regla canónica, con checkpoints de IO para límites, cancelación y
/// fencing del host. `entries` y `bytes` son acumulados realmente observados.
pub fn package_hash_checked(
    dir: &Path,
    exclude: &[&str],
    checkpoint: &mut dyn FnMut(&Path, usize, u64) -> Result<(), LarmError>,
) -> Result<String, LarmError> {
    use sha2::{Digest, Sha256};
    let io = |e: std::io::Error| {
        LarmError::new(
            ErrorCode::ResourceUnavailable,
            "No se pudo leer el paquete del adaptador",
            "io_failed",
        )
        .with_technical(e.to_string())
    };
    let mut files = Vec::new();
    let mut entries = 0usize;
    let mut bytes = 0u64;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        checkpoint(&d, entries, bytes)?;
        for entry in std::fs::read_dir(&d).map_err(io)? {
            let entry = entry.map_err(io)?;
            checkpoint(&entry.path(), entries, bytes)?;
            let kind = entry.file_type().map_err(io)?;
            if kind.is_file() {
                entries = entries.saturating_add(1);
            }
            checkpoint(&entry.path(), entries, bytes)?;
            if kind.is_symlink() {
                return Err(denied(
                    "package_symlink",
                    "El paquete contiene enlaces simbólicos",
                ));
            }
            if kind.is_dir() {
                stack.push(entry.path());
            } else if kind.is_file() {
                let rel = entry
                    .path()
                    .strip_prefix(dir)
                    .map(|p| p.to_string_lossy().replace('\\', "/").to_lowercase())
                    .unwrap_or_default();
                if !exclude.iter().any(|x| x.to_lowercase() == rel) {
                    files.push((rel, entry.path()));
                }
            } else {
                return Err(denied(
                    "package_special_file",
                    "El paquete contiene un fichero no regular",
                ));
            }
        }
    }
    files.sort();
    let mut list = Sha256::new();
    // One bounded buffer for the whole scan. Checkpoints still fence every
    // read before and after IO; larger batches avoid repeating path walks for
    // every 64 KiB of a large local package. No bytes or digests are cached.
    let mut buf = vec![0u8; 1 << 20];
    for (rel, path) in files {
        let mut file = std::fs::File::open(&path).map_err(io)?;
        let mut h = Sha256::new();
        loop {
            checkpoint(&path, entries, bytes)?;
            let n = std::io::Read::read(&mut file, &mut buf).map_err(io)?;
            if n == 0 {
                break;
            }
            bytes = bytes.checked_add(n as u64).ok_or_else(|| {
                denied(
                    "package_size_overflow",
                    "El paquete supera el tamaño permitido",
                )
            })?;
            checkpoint(&path, entries, bytes)?;
            h.update(&buf[..n]);
        }
        list.update(format!("{rel}\0{}\n", hex::encode(h.finalize())).as_bytes());
    }
    Ok(hex::encode(list.finalize()))
}

/// `operation_digest` de la aprobación TRUSTED_ONLY: liga adaptador,
/// versión, hash del paquete, publicador y permisos exactos.
///
/// # Errors
/// Manifiesto no serializable.
pub fn execution_digest(manifest: &AdapterManifest) -> Result<String, LarmError> {
    use sha2::{Digest, Sha256};
    let value = serde_json::json!({
        "operation": "adapter.execute",
        "adapter_id": manifest.id,
        "version": manifest.version,
        "package_sha256": manifest.package_sha256,
        "publisher": manifest.publisher,
        "permissions": manifest.permissions,
    });
    let canonical = larm_domain::canonical::canonicalize(&value).map_err(|e| {
        LarmError::new(ErrorCode::Internal, "No canonicalizable", "canonical").with_technical(e)
    })?;
    Ok(hex::encode(Sha256::digest(canonical.as_bytes())))
}

/// `operation_digest` de aceptar una elevación de permisos.
///
/// # Errors
/// No serializable.
pub fn elevation_digest(
    manifest: &AdapterManifest,
    delta: &PermissionDelta,
) -> Result<String, LarmError> {
    use sha2::{Digest, Sha256};
    let value = serde_json::json!({
        "operation": "adapter.permissions.elevate",
        "adapter_id": manifest.id,
        "version": manifest.version,
        "package_sha256": manifest.package_sha256,
        "delta": delta,
    });
    let canonical = larm_domain::canonical::canonicalize(&value).map_err(|e| {
        LarmError::new(ErrorCode::Internal, "No canonicalizable", "canonical").with_technical(e)
    })?;
    Ok(hex::encode(Sha256::digest(canonical.as_bytes())))
}

/// `resource_ids` de las aprobaciones de un adaptador.
#[must_use]
pub fn adapter_resource(manifest: &AdapterManifest) -> String {
    format!("adapter:{}@{}", manifest.id, manifest.version)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    fn manifest() -> AdapterManifest {
        AdapterManifest::parse(include_str!(
            "../../../tests/fixtures/adapters/demo-benchmark.manifest.json"
        ))
        .unwrap()
    }

    fn store(key: &SigningKey, official: bool) -> TrustStore {
        TrustStore {
            keys: vec![PinnedKey {
                key_id: "larm-2026".into(),
                publisher: "LARM".into(),
                public_key: hex::encode(key.verifying_key().to_bytes()),
                official,
                revoked: false,
            }],
            revoked_packages: vec![],
        }
    }

    #[test]
    fn firma_ed25519_verificada_y_manipulacion() {
        let key = SigningKey::from_bytes(&[7u8; 32]);
        let mut m = manifest();
        m.signature = Some(sign(&m, "larm-2026", &key).unwrap());
        assert_eq!(
            store(&key, true).verify(&m, false).unwrap(),
            SignatureState::Official
        );
        assert_eq!(
            store(&key, false).verify(&m, false).unwrap(),
            SignatureState::SignedThirdParty
        );
        // Cualquier cambio del manifiesto invalida la firma.
        let mut tampered = m.clone();
        tampered
            .permissions
            .network_origins
            .push("https://evil.example".into());
        assert_eq!(
            store(&key, true)
                .verify(&tampered, false)
                .unwrap_err()
                .details
                .reason,
            "signature_invalid"
        );
        // Clave desconocida, de otro publicador o revocada.
        let other = SigningKey::from_bytes(&[9u8; 32]);
        assert_eq!(
            store(&other, true)
                .verify(&m, false)
                .unwrap_err()
                .details
                .reason,
            "signature_invalid"
        );
        let mut s = store(&key, true);
        s.keys[0].publisher = "Otro".into();
        assert_eq!(
            s.verify(&m, false).unwrap_err().details.reason,
            "signature_invalid"
        );
        let mut s = store(&key, true);
        s.keys[0].revoked = true;
        assert_eq!(s.verify(&m, false).unwrap_err().details.reason, "revoked");
        let mut s = store(&key, true);
        s.revoked_packages.push(m.package_sha256.clone());
        assert_eq!(s.verify(&m, false).unwrap_err().details.reason, "revoked");
        // Sin firma.
        let unsigned = manifest();
        assert_eq!(
            store(&key, true).verify(&unsigned, false).unwrap(),
            SignatureState::Unsigned
        );
        assert_eq!(
            store(&key, true).verify(&unsigned, true).unwrap(),
            SignatureState::Custom
        );
        // El payload es determinista y no incluye la firma.
        assert_eq!(
            signing_payload(&m).unwrap(),
            signing_payload(&unsigned).unwrap()
        );
    }

    #[test]
    fn delta_de_permisos() {
        let base = manifest().permissions;
        let mut more = base.clone();
        more.network_origins.push("https://huggingface.co".into());
        more.secret_refs.push("larm/source/hf".into());
        let d = PermissionDelta::between(&base, &more);
        assert!(d.is_elevation());
        // Cada lista, por separado, es una elevación.
        for field in 0..5 {
            let mut one = base.clone();
            let list = match field {
                0 => &mut one.read_roots,
                1 => &mut one.write_roots,
                2 => &mut one.network_origins,
                3 => &mut one.process_allowlist,
                _ => &mut one.secret_refs,
            };
            list.push("nuevo".into());
            assert!(
                PermissionDelta::between(&base, &one).is_elevation(),
                "lista {field}"
            );
        }
        assert_eq!(
            d.network_origins,
            vec!["https://huggingface.co".to_string()]
        );
        assert!(
            !PermissionDelta::between(&more, &base).is_elevation(),
            "pedir menos no es elevar"
        );
        assert!(!PermissionDelta::between(&base, &base).is_elevation());
    }

    #[test]
    fn comprobacion_de_operaciones() {
        let dir = tempfile::tempdir().unwrap();
        let models = dir.path().join("models");
        let runtime = dir.path().join("runtime");
        std::fs::create_dir_all(&models).unwrap();
        std::fs::create_dir_all(&runtime).unwrap();
        std::fs::write(models.join("m.gguf"), b"x").unwrap();
        std::fs::write(dir.path().join("fuera.txt"), b"x").unwrap();
        let permissions = Permissions {
            read_roots: vec!["$models".into()],
            write_roots: vec!["$runtime".into()],
            network_origins: vec!["https://huggingface.co".into()],
            process_allowlist: vec!["bin/llama-server.exe".into()],
            secret_refs: vec!["larm/source/hf".into()],
        };
        let roots = RootMap(vec![
            ("$models".into(), models.clone()),
            ("$runtime".into(), runtime.clone()),
        ]);
        let g = Grants::resolve(&permissions, &roots);
        let ok = Access {
            reads: vec![models.join("m.gguf")],
            writes: vec![runtime.join("nuevo.bin")],
            urls: vec![
                "https://HuggingFace.co/api/models?x=1".into(),
                "https://huggingface.co:443/x".into(),
            ],
            processes: vec![r"bin\llama-server.exe".into()],
            secrets: vec!["larm/source/hf".into()],
        };
        assert!(g.check(&ok).is_ok());
        let reason = |a: Access| g.check(&a).unwrap_err().details.reason;
        assert_eq!(
            reason(Access {
                reads: vec![dir.path().join("fuera.txt")],
                ..Access::default()
            }),
            "read_denied"
        );
        assert_eq!(
            reason(Access {
                reads: vec![models.join("..").join("fuera.txt")],
                ..Access::default()
            }),
            "read_denied"
        );
        assert_eq!(
            reason(Access {
                writes: vec![models.join("w.bin")],
                ..Access::default()
            }),
            "write_denied"
        );
        assert_eq!(
            reason(Access {
                urls: vec!["https://huggingface.co.evil.com/".into()],
                ..Access::default()
            }),
            "network_denied"
        );
        assert_eq!(
            reason(Access {
                urls: vec!["http://huggingface.co/".into()],
                ..Access::default()
            }),
            "network_denied"
        );
        assert_eq!(
            reason(Access {
                urls: vec!["https://user@huggingface.co/".into()],
                ..Access::default()
            }),
            "network_denied"
        );
        assert_eq!(
            reason(Access {
                processes: vec!["cmd.exe".into()],
                ..Access::default()
            }),
            "process_denied"
        );
        assert_eq!(
            reason(Access {
                secrets: vec!["larm/source/github".into()],
                ..Access::default()
            }),
            "secret_denied"
        );
        // Una carpeta hermana con el mismo prefijo no está dentro.
        let sibling = dir.path().join("models2");
        std::fs::create_dir_all(&sibling).unwrap();
        std::fs::write(sibling.join("m.gguf"), b"x").unwrap();
        assert_eq!(
            reason(Access {
                reads: vec![sibling.join("m.gguf")],
                ..Access::default()
            }),
            "read_denied"
        );
        // `..` se rechaza aunque el destino quedara dentro.
        assert_eq!(
            reason(Access {
                writes: vec![runtime.join("sub").join("..").join("nuevo.bin")],
                ..Access::default()
            }),
            "write_denied"
        );
        // Un origen declarado con usuario no concede nada.
        let with_user = Grants::resolve(
            &Permissions {
                network_origins: vec!["https://user@huggingface.co".into()],
                ..Permissions::default()
            },
            &roots,
        );
        assert!(with_user.network_origins.is_empty());
        assert!(with_user
            .check(&Access {
                urls: vec!["https://user@huggingface.co/".into()],
                ..Access::default()
            })
            .is_err());
        // Una raíz simbólica que el host no conoce no concede nada.
        let none = Grants::resolve(&permissions, &RootMap::default());
        assert!(none.read_roots.is_empty());
    }
}
