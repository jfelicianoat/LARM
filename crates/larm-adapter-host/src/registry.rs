//! P028: verificación pura del índice de registro firmado.
//! No realiza IO ni confiere confianza P022 a los paquetes enumerados.
use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Duration, SecondsFormat, Utc};
use ed25519_dalek::{Signature as Ed25519Signature, Verifier, VerifyingKey};
use larm_adapter_sdk::manifest::Signature;
use larm_domain::error::{ErrorCode, LarmError};
use serde::{Deserialize, Serialize};
use url::Url;

const MAX_SAFE_SEQUENCE: u64 = 9_007_199_254_740_991;
const MAX_PACKAGES: usize = 100_000;
const MAX_REVOCATIONS: usize = 100_000;
const MAX_INDEX_LIFETIME: Duration = Duration::days(30);
const MAX_CLOCK_SKEW: Duration = Duration::minutes(5);

fn error(code: ErrorCode, reason: &'static str, message: &'static str) -> LarmError {
    LarmError::new(code, message, reason)
}

fn invalid(reason: &'static str) -> LarmError {
    error(
        ErrorCode::ValidationFailed,
        reason,
        "El índice del registro no cumple el contrato",
    )
}

fn signature_invalid(reason: &'static str) -> LarmError {
    error(
        ErrorCode::Forbidden,
        reason,
        "No se pudo verificar la firma del registro",
    )
}

fn lower_hex(value: &str, bytes: usize) -> bool {
    value.len() == bytes * 2
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn key_id_valid(value: &str) -> bool {
    let mut bytes = value.bytes();
    bytes
        .next()
        .is_some_and(|first| first.is_ascii_alphanumeric())
        && value.len() <= 128
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn registry_id_valid(value: &str) -> bool {
    let mut bytes = value.bytes();
    bytes
        .next()
        .is_some_and(|first| first.is_ascii_alphanumeric())
        && value.len() <= 127
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn timestamp(value: &str) -> Result<DateTime<Utc>, LarmError> {
    let parsed = DateTime::parse_from_rfc3339(value)
        .map_err(|_| invalid("registry_timestamp_invalid"))?
        .with_timezone(&Utc);
    if parsed.to_rfc3339_opts(SecondsFormat::Millis, true) != value {
        return Err(invalid("registry_timestamp_invalid"));
    }
    Ok(parsed)
}

fn https_origin(value: &str, reason: &'static str) -> Result<String, LarmError> {
    let url = Url::parse(value).map_err(|_| invalid(reason))?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(invalid(reason));
    }
    Ok(url.origin().ascii_serialization())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistryIndexEnvelope {
    pub payload: RegistryIndexPayload,
    pub signature: Signature,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistryIndexPayload {
    pub schema_version: String,
    pub registry_id: String,
    pub sequence: u64,
    pub issued_at: String,
    pub expires_at: String,
    pub packages: Vec<RegistryPackageEntry>,
    pub revocations: Vec<RegistryRevocation>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistryPackageEntry {
    pub id: String,
    pub version: String,
    pub url: String,
    /// SHA-256 de los bytes del ZIP, diferente del hash canónico P022.
    pub sha256: String,
    pub package_sha256: String,
    /// Firma esperada del AdapterManifest; P022 vuelve a validarla al instalar.
    pub signature: Option<Signature>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum RegistryRevocationType {
    PackageSha256,
    PublisherKey,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistryRevocation {
    pub target_type: RegistryRevocationType,
    pub target: String,
    pub revoked_at: String,
    pub reason_code: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedRegistryIndex {
    pub payload: RegistryIndexPayload,
    pub payload_sha256: String,
    /// true sólo para replay idéntico de la secuencia ya confirmada.
    pub replay: bool,
}

/// Valida formato, política HTTPS, frescura, secuencia y firma Ed25519.
/// `pinned_keys` contiene claves públicas hex minúsculas fijadas por el operador.
/// `checkpoint`, si existe, es `(secuencia, payload_sha256)` ya persistido.
/// Esta función no realiza red ni modifica la confianza de ningún adaptador.
pub fn verify_index(
    envelope: &RegistryIndexEnvelope,
    pinned_keys: &BTreeMap<String, String>,
    registry_url: &str,
    allowed_origins: &BTreeSet<String>,
    now: DateTime<Utc>,
    checkpoint: Option<(u64, &str)>,
) -> Result<VerifiedRegistryIndex, LarmError> {
    let registry_origin = https_origin(registry_url, "registry_url_invalid")?;
    for origin in allowed_origins {
        let parsed = https_origin(origin, "registry_allowed_origin_invalid")?;
        if parsed != origin.as_str() {
            return Err(invalid("registry_allowed_origin_invalid"));
        }
    }

    validate_payload(&envelope.payload, &registry_origin, allowed_origins, now)?;
    let signature = &envelope.signature;
    if signature.algorithm != larm_adapter_sdk::manifest::SignatureAlgorithm::Ed25519
        || !key_id_valid(&signature.key_id)
        || !lower_hex(&signature.value, 64)
    {
        return Err(signature_invalid("registry_signature_invalid"));
    }
    let public_hex = pinned_keys
        .get(&signature.key_id)
        .ok_or_else(|| signature_invalid("registry_key_unknown"))?;
    if !lower_hex(public_hex, 32) {
        return Err(signature_invalid("registry_key_invalid"));
    }
    let public_bytes: [u8; 32] = hex::decode(public_hex)
        .map_err(|_| signature_invalid("registry_key_invalid"))?
        .try_into()
        .map_err(|_| signature_invalid("registry_key_invalid"))?;
    let key = VerifyingKey::from_bytes(&public_bytes)
        .map_err(|_| signature_invalid("registry_key_invalid"))?;
    let signature_bytes: [u8; 64] = hex::decode(&signature.value)
        .map_err(|_| signature_invalid("registry_signature_invalid"))?
        .try_into()
        .map_err(|_| signature_invalid("registry_signature_invalid"))?;
    let signature = Ed25519Signature::from_bytes(&signature_bytes);
    let payload_value =
        serde_json::to_value(&envelope.payload).map_err(|_| invalid("registry_payload_invalid"))?;
    let canonical = larm_domain::canonical::canonicalize(&payload_value)
        .map_err(|_| invalid("registry_payload_not_canonicalizable"))?;
    key.verify(canonical.as_bytes(), &signature)
        .map_err(|_| signature_invalid("registry_signature_invalid"))?;
    let payload_sha256 = larm_domain::canonical::sha256_hex(&payload_value)
        .map_err(|_| invalid("registry_payload_not_canonicalizable"))?;

    let replay = match checkpoint {
        Some((previous, _)) if envelope.payload.sequence < previous => {
            return Err(error(
                ErrorCode::Conflict,
                "registry_sequence_rollback",
                "El registro devolvió una secuencia anterior",
            ));
        }
        Some((previous, previous_hash)) if envelope.payload.sequence == previous => {
            if payload_sha256 != previous_hash {
                return Err(error(
                    ErrorCode::Conflict,
                    "registry_sequence_conflict",
                    "La secuencia del registro ya existe con otro contenido",
                ));
            }
            true
        }
        _ => false,
    };

    Ok(VerifiedRegistryIndex {
        payload: envelope.payload.clone(),
        payload_sha256,
        replay,
    })
}

fn validate_payload(
    payload: &RegistryIndexPayload,
    registry_origin: &str,
    allowed_origins: &BTreeSet<String>,
    now: DateTime<Utc>,
) -> Result<(), LarmError> {
    if payload.schema_version != "1.0"
        || !registry_id_valid(&payload.registry_id)
        || payload.sequence == 0
        || payload.sequence > MAX_SAFE_SEQUENCE
        || payload.packages.len() > MAX_PACKAGES
        || payload.revocations.len() > MAX_REVOCATIONS
    {
        return Err(invalid("registry_payload_invalid"));
    }
    let issued = timestamp(&payload.issued_at)?;
    let expires = timestamp(&payload.expires_at)?;
    if expires <= issued || expires - issued > MAX_INDEX_LIFETIME {
        return Err(invalid("registry_validity_invalid"));
    }
    if issued > now + MAX_CLOCK_SKEW {
        return Err(invalid("registry_issued_in_future"));
    }
    if expires <= now {
        return Err(error(
            ErrorCode::IntegrityFailed,
            "registry_index_expired",
            "El índice firmado ya ha vencido",
        ));
    }

    let mut identities = BTreeSet::new();
    for package in &payload.packages {
        if package.id.is_empty()
            || package.id.len() > 256
            || package.version.is_empty()
            || package.version.len() > 128
            || !lower_hex(&package.sha256, 32)
            || !lower_hex(&package.package_sha256, 32)
            || !identities.insert((package.id.to_lowercase(), package.version.to_lowercase()))
        {
            return Err(invalid("registry_package_entry_invalid"));
        }
        let package_origin = https_origin(&package.url, "registry_package_url_invalid")?;
        if package_origin != registry_origin && !allowed_origins.contains(&package_origin) {
            return Err(invalid("registry_package_origin_denied"));
        }
        if let Some(signature) = &package.signature {
            if signature.algorithm != larm_adapter_sdk::manifest::SignatureAlgorithm::Ed25519
                || !key_id_valid(&signature.key_id)
                || !lower_hex(&signature.value, 64)
            {
                return Err(invalid("registry_package_signature_invalid"));
            }
        }
    }

    let mut revocations = BTreeSet::new();
    for revocation in &payload.revocations {
        let target_valid = match revocation.target_type {
            RegistryRevocationType::PackageSha256 => lower_hex(&revocation.target, 32),
            RegistryRevocationType::PublisherKey => key_id_valid(&revocation.target),
        };
        if !target_valid
            || timestamp(&revocation.revoked_at)? > now + MAX_CLOCK_SKEW
            || !matches!(
                revocation.reason_code.as_str(),
                "COMPROMISED_KEY"
                    | "MALICIOUS_PACKAGE"
                    | "INVALID_PROVENANCE"
                    | "OPERATOR_REQUEST"
                    | "OTHER"
            )
            || !revocations.insert((revocation.target_type, revocation.target.clone()))
        {
            return Err(invalid("registry_revocation_invalid"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use chrono::TimeZone;
    use ed25519_dalek::{Signer, SigningKey};

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 24, 12, 0, 0)
            .single()
            .unwrap()
    }

    fn fixture(key: &SigningKey) -> RegistryIndexEnvelope {
        let payload = RegistryIndexPayload {
            schema_version: "1.0".into(),
            registry_id: "fixture-v1".into(),
            sequence: 1,
            issued_at: "2026-10-24T11:00:00.000Z".into(),
            expires_at: "2026-10-25T11:00:00.000Z".into(),
            packages: vec![RegistryPackageEntry {
                id: "acme.runtime".into(),
                version: "1.2.0".into(),
                url: "https://registry.example.invalid/packages/runtime.zip".into(),
                sha256: "1".repeat(64),
                package_sha256: "2".repeat(64),
                signature: None,
            }],
            revocations: vec![RegistryRevocation {
                target_type: RegistryRevocationType::PackageSha256,
                target: "3".repeat(64),
                revoked_at: "2026-10-24T10:00:00.000Z".into(),
                reason_code: "MALICIOUS_PACKAGE".into(),
            }],
        };
        let value = serde_json::to_value(&payload).unwrap();
        let canonical = larm_domain::canonical::canonicalize(&value).unwrap();
        let signature = key.sign(canonical.as_bytes());
        RegistryIndexEnvelope {
            payload,
            signature: Signature {
                algorithm: larm_adapter_sdk::manifest::SignatureAlgorithm::Ed25519,
                key_id: "registry-2026".into(),
                value: hex::encode(signature.to_bytes()),
            },
        }
    }

    fn pins(key: &SigningKey) -> BTreeMap<String, String> {
        BTreeMap::from([(
            "registry-2026".into(),
            hex::encode(key.verifying_key().to_bytes()),
        )])
    }

    fn origins() -> BTreeSet<String> {
        BTreeSet::new()
    }

    #[test]
    fn verifica_firma_ed25519_sobre_payload_rfc8785_y_hash_zip_separado() {
        let key = SigningKey::from_bytes(&[7; 32]);
        let index = fixture(&key);
        let verified = verify_index(
            &index,
            &pins(&key),
            "https://registry.example.invalid/index.json",
            &origins(),
            now(),
            None,
        )
        .unwrap();
        assert!(!verified.replay);
        assert_eq!(verified.payload.packages[0].sha256, "1".repeat(64));
        assert_eq!(verified.payload.packages[0].package_sha256, "2".repeat(64));
        assert_eq!(verified.payload_sha256.len(), 64);
    }

    #[test]
    fn firma_modificada_clave_desconocida_y_pins_malformados_fallan_cerrado() {
        let key = SigningKey::from_bytes(&[8; 32]);
        let mut index = fixture(&key);
        index.payload.sequence += 1;
        assert_eq!(
            verify_index(
                &index,
                &pins(&key),
                "https://registry.example.invalid/index.json",
                &origins(),
                now(),
                None,
            )
            .unwrap_err()
            .details
            .reason,
            "registry_signature_invalid"
        );
        let unknown_pins = BTreeMap::new();
        assert_eq!(
            verify_index(
                &fixture(&key),
                &unknown_pins,
                "https://registry.example.invalid/index.json",
                &origins(),
                now(),
                None,
            )
            .unwrap_err()
            .details
            .reason,
            "registry_key_unknown"
        );
        let mut bad_pins = pins(&key);
        bad_pins.insert("registry-2026".into(), "A".repeat(64));
        assert_eq!(
            verify_index(
                &fixture(&key),
                &bad_pins,
                "https://registry.example.invalid/index.json",
                &origins(),
                now(),
                None,
            )
            .unwrap_err()
            .details
            .reason,
            "registry_key_invalid"
        );
    }

    #[test]
    fn rechaza_urls_http_con_credenciales_query_fragment_y_origen_no_autorizado() {
        let key = SigningKey::from_bytes(&[10; 32]);
        let mut index = fixture(&key);
        for url in [
            "http://registry.example.invalid/index.json",
            "https://user:pass@registry.example.invalid/index.json",
            "https://registry.example.invalid/index.json?token=secret",
            "https://registry.example.invalid/index.json#fragment",
        ] {
            assert_eq!(
                verify_index(&index, &pins(&key), url, &origins(), now(), None)
                    .unwrap_err()
                    .details
                    .reason,
                "registry_url_invalid"
            );
        }
        index.payload.packages[0].url = "https://cdn.example.invalid/runtime.zip".into();
        assert_eq!(
            verify_index(
                &index,
                &pins(&key),
                "https://registry.example.invalid/index.json",
                &origins(),
                now(),
                None,
            )
            .unwrap_err()
            .details
            .reason,
            "registry_package_origin_denied"
        );
        let mut allowed = origins();
        allowed.insert("https://cdn.example.invalid".into());
        let value = serde_json::to_value(&index.payload).unwrap();
        let canonical = larm_domain::canonical::canonicalize(&value).unwrap();
        index.signature.value = hex::encode(key.sign(canonical.as_bytes()).to_bytes());
        assert!(verify_index(
            &index,
            &pins(&key),
            "https://registry.example.invalid/index.json",
            &allowed,
            now(),
            None,
        )
        .is_ok());
    }

    #[test]
    fn rechaza_vencimiento_secuencia_y_duplicados_y_acepta_replay_identico() {
        let key = SigningKey::from_bytes(&[11; 32]);
        let index = fixture(&key);
        let verified = verify_index(
            &index,
            &pins(&key),
            "https://registry.example.invalid/index.json",
            &origins(),
            now(),
            None,
        )
        .unwrap();
        let replay = verify_index(
            &index,
            &pins(&key),
            "https://registry.example.invalid/index.json",
            &origins(),
            now(),
            Some((1, &verified.payload_sha256)),
        )
        .unwrap();
        assert!(replay.replay);
        assert_eq!(
            verify_index(
                &index,
                &pins(&key),
                "https://registry.example.invalid/index.json",
                &origins(),
                now(),
                Some((2, &verified.payload_sha256)),
            )
            .unwrap_err()
            .details
            .reason,
            "registry_sequence_rollback"
        );
        let mut expired = fixture(&key);
        expired.payload.expires_at = "2026-10-24T11:59:59.000Z".into();
        assert_eq!(
            verify_index(
                &expired,
                &pins(&key),
                "https://registry.example.invalid/index.json",
                &origins(),
                now(),
                None,
            )
            .unwrap_err()
            .details
            .reason,
            "registry_index_expired"
        );
        let mut duplicate = fixture(&key);
        duplicate
            .payload
            .packages
            .push(duplicate.payload.packages[0].clone());
        assert_eq!(
            verify_index(
                &duplicate,
                &pins(&key),
                "https://registry.example.invalid/index.json",
                &origins(),
                now(),
                None,
            )
            .unwrap_err()
            .details
            .reason,
            "registry_package_entry_invalid"
        );
    }

    #[test]
    fn revocaciones_invalidas_y_fecha_futura_se_rechazan() {
        let key = SigningKey::from_bytes(&[12; 32]);
        let mut invalid_revocation = fixture(&key);
        invalid_revocation.payload.revocations[0].target = "A".repeat(64);
        assert_eq!(
            verify_index(
                &invalid_revocation,
                &pins(&key),
                "https://registry.example.invalid/index.json",
                &origins(),
                now(),
                None,
            )
            .unwrap_err()
            .details
            .reason,
            "registry_revocation_invalid"
        );
        let mut future = fixture(&key);
        future.payload.issued_at = "2026-10-24T12:06:00.000Z".into();
        assert_eq!(
            verify_index(
                &future,
                &pins(&key),
                "https://registry.example.invalid/index.json",
                &origins(),
                now(),
                None,
            )
            .unwrap_err()
            .details
            .reason,
            "registry_issued_in_future"
        );
    }
}

/// Fuente local determinista de ZIPs para inspección; no instala ni cambia confianza.
#[async_trait::async_trait]
pub trait RegistrySource: Send + Sync {
    async fn packages(&self) -> Result<Vec<crate::package::ArchivePlan>, LarmError>;
}

#[derive(Clone)]
pub struct LocalFolderRegistry {
    pub root: std::path::PathBuf,
}

#[async_trait::async_trait]
impl RegistrySource for LocalFolderRegistry {
    async fn packages(&self) -> Result<Vec<crate::package::ArchivePlan>, LarmError> {
        let root = self.root.clone();
        let control = crate::package::PackageControl::default();
        let worker_control = control.clone();
        let task = tokio::task::spawn_blocking(move || discover_local(root, &worker_control));
        match tokio::time::timeout(std::time::Duration::from_secs(30), task).await {
            Ok(Ok(result)) => result,
            Ok(Err(task_error)) => Err(LarmError::new(
                ErrorCode::Internal,
                "Fallo leyendo el registro local",
                "registry_task_failed",
            )
            .with_technical(task_error.to_string())),
            Err(_) => {
                control.cancel();
                Err(LarmError::new(
                    ErrorCode::Timeout,
                    "El registro local no respondió a tiempo",
                    "registry_timeout",
                ))
            }
        }
    }
}

fn discover_local(
    root: std::path::PathBuf,
    control: &crate::package::PackageControl,
) -> Result<Vec<crate::package::ArchivePlan>, LarmError> {
    use sha2::{Digest, Sha256};
    use std::io::Read;

    let io_error = |cause: std::io::Error| {
        LarmError::new(
            ErrorCode::ResourceUnavailable,
            "No se pudo leer el registro local",
            "registry_io_failed",
        )
        .with_technical(cause.to_string())
    };
    crate::package::check_managed_path(&root)?;
    let mut packages = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for item in std::fs::read_dir(&root).map_err(io_error)? {
        control.check()?;
        let item = item.map_err(io_error)?;
        if !item
            .path()
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("zip"))
        {
            continue;
        }
        crate::package::check_managed_path(&item.path())?;
        if !item.file_type().map_err(io_error)?.is_file() {
            continue;
        }
        if packages.len() >= crate::package::MAX_FILES {
            return Err(LarmError::new(
                ErrorCode::ValidationFailed,
                "Demasiados paquetes en el registro",
                "registry_package_limit",
            ));
        }
        let mut file = std::fs::File::open(item.path()).map_err(io_error)?;
        let mut digest = Sha256::new();
        let mut count = 0u64;
        let mut buffer = [0u8; 65_536];
        loop {
            control.check()?;
            let read = file.read(&mut buffer).map_err(io_error)?;
            if read == 0 {
                break;
            }
            count += read as u64;
            if count > crate::package::MAX_BYTES {
                return Err(LarmError::new(
                    ErrorCode::ValidationFailed,
                    "Paquete demasiado grande",
                    "package_size_limit",
                ));
            }
            digest.update(&buffer[..read]);
        }
        let plan = crate::package::ArchivePlan::inspect(
            &item.path(),
            &hex::encode(digest.finalize()),
            control,
        )?;
        if !seen.insert((
            plan.manifest.id.to_lowercase(),
            plan.manifest.version.to_lowercase(),
        )) {
            return Err(LarmError::new(
                ErrorCode::Conflict,
                "El registro contiene paquetes ambiguos para la misma versión",
                "registry_version_duplicate",
            ));
        }
        packages.push(plan);
    }
    packages.sort_by(|left, right| {
        (&left.manifest.id, &left.manifest.version)
            .cmp(&(&right.manifest.id, &right.manifest.version))
    });
    Ok(packages)
}
