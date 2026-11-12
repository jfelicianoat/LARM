//! Validación conservadora del formato `packages.lock.json` de NuGet.
//!
//! El hash del documento completo se sella en `InstallSpec`; esta validación
//! comprueba que la clausura contiene versiones resueltas y hashes de paquete
//! antes de permitir que el plan la acepte.

use base64::Engine;
use larm_domain::error::{ErrorCode, LarmError};
use serde_json::Value;

fn invalid(reason: &str) -> LarmError {
    LarmError::new(
        ErrorCode::ValidationFailed,
        "Lock de dependencias NuGet no válido",
        reason,
    )
}

fn package_id(value: &str) -> bool {
    let bytes = value.as_bytes();
    !bytes.is_empty()
        && bytes[0].is_ascii_alphanumeric()
        && bytes[bytes.len() - 1].is_ascii_alphanumeric()
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
}

/// NuGet lock files store resolved package versions, not version ranges.
fn pinned_version(value: &str) -> bool {
    if value.is_empty() || !value.is_ascii() || value.trim() != value {
        return false;
    }
    let (without_metadata, metadata) = value
        .split_once('+')
        .map_or((value, None), |(version, metadata)| {
            (version, Some(metadata))
        });
    if metadata.is_some_and(|metadata| !version_identifiers(metadata)) {
        return false;
    }
    let (core, prerelease) = without_metadata
        .split_once('-')
        .map_or((without_metadata, None), |(core, prerelease)| {
            (core, Some(prerelease))
        });
    let components: Vec<_> = core.split('.').collect();
    if !(1..=4).contains(&components.len())
        || components
            .iter()
            .any(|part| part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit()))
        || prerelease.is_some_and(|suffix| !version_identifiers(suffix))
    {
        return false;
    }
    true
}

fn version_identifiers(value: &str) -> bool {
    !value.is_empty()
        && value.split('.').all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        })
}

fn text(value: &str) -> bool {
    !value.trim().is_empty() && !value.chars().any(char::is_control)
}

fn content_hash(value: &str) -> bool {
    base64::engine::general_purpose::STANDARD
        .decode(value)
        .is_ok_and(|bytes| bytes.len() == 64)
}

fn validate_entry(id: &str, entry: &Value) -> Result<(), LarmError> {
    if !package_id(id) {
        return Err(invalid("runtime_nuget_package_id_invalid"));
    }
    let entry = entry
        .as_object()
        .ok_or_else(|| invalid("runtime_nuget_package_entry_invalid"))?;
    let kind = entry
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("runtime_nuget_package_type_invalid"))?;
    if !["direct", "transitive", "centraltransitive"]
        .iter()
        .any(|allowed| kind.eq_ignore_ascii_case(allowed))
    {
        // Project references do not identify immutable NuGet package bytes.
        return Err(invalid("runtime_nuget_package_type_unsupported"));
    }
    if entry
        .get("resolved")
        .and_then(Value::as_str)
        .is_none_or(|value| !pinned_version(value))
    {
        return Err(invalid("runtime_nuget_resolved_version_invalid"));
    }
    if entry
        .get("contentHash")
        .and_then(Value::as_str)
        .is_none_or(|value| !content_hash(value))
    {
        return Err(invalid("runtime_nuget_content_hash_invalid"));
    }

    if matches!(
        kind.to_ascii_lowercase().as_str(),
        "direct" | "centraltransitive"
    ) && entry
        .get("requested")
        .and_then(Value::as_str)
        .is_none_or(|requested| !text(requested))
    {
        return Err(invalid("runtime_nuget_requested_version_missing"));
    }

    if let Some(dependencies) = entry.get("dependencies") {
        let dependencies = dependencies
            .as_object()
            .ok_or_else(|| invalid("runtime_nuget_dependencies_invalid"))?;
        for (dependency_id, dependency_version) in dependencies {
            if !package_id(dependency_id)
                || dependency_version
                    .as_str()
                    .is_none_or(|version| !pinned_version(version))
            {
                return Err(invalid("runtime_nuget_dependencies_invalid"));
            }
        }
    }
    Ok(())
}

/// Accepts the versioned NuGet lock-file formats currently supported by LARM
/// (1 through 3) and rejects empty graphs, project entries, ranges and missing
/// package hashes. The original JSON remains intact and is hashed separately.
pub fn validate(value: &Value) -> Result<(), LarmError> {
    let root = value
        .as_object()
        .ok_or_else(|| invalid("runtime_nuget_lock_invalid"))?;
    if root
        .get("version")
        .and_then(Value::as_f64)
        .filter(|version| version.fract() == 0.0 && (1.0..=3.0).contains(version))
        .is_none()
    {
        return Err(invalid("runtime_nuget_lock_version_unsupported"));
    }
    let targets = root
        .get("dependencies")
        .and_then(Value::as_object)
        .filter(|targets| !targets.is_empty())
        .ok_or_else(|| invalid("runtime_nuget_lock_dependencies_missing"))?;
    let mut package_count = 0usize;
    for (target, packages) in targets {
        if !text(target) {
            return Err(invalid("runtime_nuget_target_invalid"));
        }
        let packages = packages
            .as_object()
            .filter(|packages| !packages.is_empty())
            .ok_or_else(|| invalid("runtime_nuget_target_empty"))?;
        for (id, entry) in packages {
            validate_entry(id, entry)?;
            package_count += 1;
        }
    }
    if package_count == 0 {
        return Err(invalid("runtime_nuget_lock_dependencies_missing"));
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use serde_json::json;

    fn valid_lock() -> Value {
        let hash = base64::engine::general_purpose::STANDARD.encode([7_u8; 64]);
        json!({
            "version": 1,
            "dependencies": {
                "net8.0": {
                    "Contoso.Runtime": {
                        "type": "Direct",
                        "requested": "[2.4.1, 2.4.1]",
                        "resolved": "2.4.1",
                        "contentHash": hash,
                        "dependencies": {"Contoso.Core": "1.9.0"}
                    },
                    "Contoso.Core": {
                        "type": "Transitive",
                        "resolved": "1.9.0",
                        "contentHash": hash
                    }
                }
            }
        })
    }

    #[test]
    fn accepts_real_lock_shape_with_pinned_packages_and_sha512_hashes() {
        assert!(validate(&valid_lock()).is_ok());
    }

    #[test]
    fn rejects_missing_or_unsupported_lock_shapes() {
        for value in [
            json!(null),
            json!({"version": 4, "dependencies": {"net8.0": {"Pkg": {}}}}),
            json!({"version": 1, "dependencies": {}}),
            json!({"version": 1, "dependencies": {"net8.0": {}}}),
        ] {
            assert!(validate(&value).is_err());
        }
    }

    #[test]
    fn rejects_project_references_unpinned_versions_and_missing_content_hashes() {
        let mut value = valid_lock();
        value["dependencies"]["net8.0"]["Contoso.Runtime"]["type"] = json!("Project");
        assert_eq!(
            validate(&value).unwrap_err().details.reason,
            "runtime_nuget_package_type_unsupported"
        );

        let mut value = valid_lock();
        value["dependencies"]["net8.0"]["Contoso.Runtime"]["resolved"] = json!("2.*");
        assert_eq!(
            validate(&value).unwrap_err().details.reason,
            "runtime_nuget_resolved_version_invalid"
        );

        let mut value = valid_lock();
        value["dependencies"]["net8.0"]["Contoso.Runtime"]
            .as_object_mut()
            .unwrap()
            .remove("contentHash");
        assert_eq!(
            validate(&value).unwrap_err().details.reason,
            "runtime_nuget_content_hash_invalid"
        );
    }

    #[test]
    fn rejects_package_ids_and_dependency_versions_that_could_be_paths_or_ranges() {
        let mut value = valid_lock();
        let package = value["dependencies"]["net8.0"]
            .as_object_mut()
            .unwrap()
            .remove("Contoso.Runtime")
            .unwrap();
        value["dependencies"]["net8.0"]["../outside"] = package;
        assert_eq!(
            validate(&value).unwrap_err().details.reason,
            "runtime_nuget_package_id_invalid"
        );

        let mut value = valid_lock();
        value["dependencies"]["net8.0"]["Contoso.Runtime"]["dependencies"]["Contoso.Core"] =
            json!("[1.0,2.0)");
        assert_eq!(
            validate(&value).unwrap_err().details.reason,
            "runtime_nuget_dependencies_invalid"
        );
    }
}
