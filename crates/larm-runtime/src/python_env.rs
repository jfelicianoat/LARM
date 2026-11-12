//! Identidad y ubicaciones deterministas para entornos Python gestionados (P071).
//!
//! Este módulo no instala paquetes ni ejecuta procesos. El host cualificado
//! aporta el directorio runtime y delega la creación al worker aislado.

use larm_domain::{
    canonical::sha256_hex,
    error::{ErrorCode, LarmError},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};

fn invalid(reason: &'static str) -> LarmError {
    LarmError::new(
        ErrorCode::ValidationFailed,
        "Identidad de entorno Python no válida",
        reason,
    )
}

fn valid_python_version(value: &str) -> bool {
    let mut parts = value.split('.');
    let Some(major) = parts.next() else {
        return false;
    };
    let Some(minor) = parts.next() else {
        return false;
    };
    let Some(patch) = parts.next() else {
        return false;
    };
    parts.next().is_none()
        && [major, minor, patch].iter().all(|part| {
            !part.is_empty()
                && part.bytes().all(|byte| byte.is_ascii_digit())
                && (part == &"0" || !part.starts_with('0'))
                && part.parse::<u16>().is_ok()
        })
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn valid_id(value: &str) -> bool {
    larm_domain::bridge::is_uuid_v7(value)
}

/// Stable key for one runtime version, backend, Python build and dependency lock.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PythonEnvironmentIdentity {
    pub runtime_id: String,
    pub runtime_version_id: String,
    pub backend_id: String,
    pub python_version: String,
    pub dependencies_lock_sha256: String,
}

impl PythonEnvironmentIdentity {
    /// Build an identity from the exact lock payload that the runtime profile stores.
    pub fn from_lock(
        runtime_id: impl Into<String>,
        runtime_version_id: impl Into<String>,
        backend_id: impl Into<String>,
        python_version: impl Into<String>,
        dependencies_lock: &Value,
    ) -> Result<Self, LarmError> {
        let lock_is_valid = match dependencies_lock {
            Value::String(value) => !value.trim().is_empty(),
            Value::Array(values) => !values.is_empty(),
            Value::Object(values) => !values.is_empty(),
            _ => false,
        };
        if !lock_is_valid {
            return Err(invalid("python_environment_dependencies_lock_missing"));
        }
        let lock_hash = sha256_hex(dependencies_lock)
            .map_err(|_| invalid("python_environment_dependencies_lock_invalid"))?;
        let identity = Self {
            runtime_id: runtime_id.into(),
            runtime_version_id: runtime_version_id.into(),
            backend_id: backend_id.into(),
            python_version: python_version.into(),
            dependencies_lock_sha256: lock_hash,
        };
        identity.validate()?;
        Ok(identity)
    }

    pub fn validate(&self) -> Result<(), LarmError> {
        if !valid_id(&self.runtime_id)
            || !valid_id(&self.runtime_version_id)
            || !valid_id(&self.backend_id)
        {
            return Err(invalid("python_environment_identity_id_invalid"));
        }
        if !valid_python_version(&self.python_version) {
            return Err(invalid("python_environment_python_version_invalid"));
        }
        if !valid_sha256(&self.dependencies_lock_sha256) {
            return Err(invalid("python_environment_dependencies_lock_hash_invalid"));
        }
        Ok(())
    }

    /// Hash the complete identity using LARM's canonical JSON implementation.
    pub fn identity_sha256(&self) -> Result<String, LarmError> {
        self.validate()?;
        let value = serde_json::to_value(self)
            .map_err(|_| invalid("python_environment_identity_invalid"))?;
        sha256_hex(&value).map_err(|_| invalid("python_environment_identity_invalid"))
    }

    /// Return a managed relative path derived only from validated IDs and hashes.
    pub fn relative_path(&self) -> Result<PathBuf, LarmError> {
        let digest = self.identity_sha256()?;
        Ok(PathBuf::from("python-envs")
            .join(&self.runtime_id)
            .join(&self.runtime_version_id)
            .join(&self.backend_id)
            .join(&digest[..2])
            .join(digest))
    }

    /// Resolve under the host-selected managed runtime store; callers cannot supply a target.
    pub fn managed_path(&self, runtime_store: &Path) -> Result<PathBuf, LarmError> {
        if !runtime_store.is_absolute() {
            return Err(invalid("python_environment_store_path_not_absolute"));
        }
        Ok(runtime_store.join(self.relative_path()?))
    }
}

/// Platform-specific interpreter path inside a managed environment prefix.
#[must_use]
pub fn python_executable(environment_prefix: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        environment_prefix.join("Scripts").join("python.exe")
    }
    #[cfg(not(windows))]
    {
        environment_prefix.join("bin").join("python")
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use serde_json::json;

    const RUNTIME: &str = "0192aaaa-0000-7000-8000-000000000001";
    const VERSION: &str = "0192aaaa-0000-7000-8000-000000000002";
    const BACKEND: &str = "0192aaaa-0000-7000-8000-000000000003";

    fn identity(lock: &Value) -> PythonEnvironmentIdentity {
        PythonEnvironmentIdentity::from_lock(RUNTIME, VERSION, BACKEND, "3.12.4", lock).unwrap()
    }

    #[test]
    fn environment_identity_includes_python_and_exact_lock() {
        let a = identity(&json!({"torch":"2.4.0", "transformers":"4.45.0"}));
        let b = identity(&json!({"torch":"2.4.0", "transformers":"4.46.0"}));
        assert_ne!(a.identity_sha256().unwrap(), b.identity_sha256().unwrap());

        let other_python = PythonEnvironmentIdentity {
            python_version: "3.11.9".into(),
            ..a.clone()
        };
        assert_ne!(
            a.identity_sha256().unwrap(),
            other_python.identity_sha256().unwrap()
        );
    }

    #[test]
    fn worker_and_core_share_the_same_environment_digest() {
        let key = PythonEnvironmentIdentity {
            runtime_id: RUNTIME.into(),
            runtime_version_id: VERSION.into(),
            backend_id: BACKEND.into(),
            python_version: "3.12.4".into(),
            dependencies_lock_sha256: "a".repeat(64),
        };
        assert_eq!(
            key.identity_sha256().unwrap(),
            "8b80d5d12e3940ef2cd7da3d59740e9e6dfe2e451e9f1977ec0ec4f47a64eb4d"
        );
    }

    #[test]
    fn path_is_derived_and_confined_to_the_runtime_store() {
        let key = identity(&json!([{"name":"torch", "version":"2.4.0"}]));
        let relative = key.relative_path().unwrap();
        assert_eq!(relative.components().count(), 6);
        assert!(!relative.to_string_lossy().contains(".."));
        assert_eq!(
            key.managed_path(Path::new("C:/LARM/runtime-store"))
                .unwrap(),
            Path::new("C:/LARM/runtime-store").join(relative)
        );
        assert!(key.managed_path(Path::new("relative/store")).is_err());
    }

    #[test]
    fn invalid_ids_versions_and_missing_locks_are_rejected() {
        assert!(PythonEnvironmentIdentity::from_lock(
            "not-an-id",
            VERSION,
            BACKEND,
            "3.12.4",
            &json!({"torch":"2.4.0"})
        )
        .is_err());
        assert!(PythonEnvironmentIdentity::from_lock(
            RUNTIME,
            VERSION,
            BACKEND,
            "03.12.4",
            &json!({"torch":"2.4.0"})
        )
        .is_err());
        assert!(PythonEnvironmentIdentity::from_lock(
            RUNTIME,
            VERSION,
            BACKEND,
            "3.12.4",
            &Value::Null
        )
        .is_err());
        assert!(PythonEnvironmentIdentity::from_lock(
            RUNTIME,
            VERSION,
            BACKEND,
            "3.12.4",
            &json!(true)
        )
        .is_err());
    }

    #[test]
    fn interpreter_path_uses_managed_prefix_layout() {
        let path = python_executable(Path::new("C:/LARM/env"));
        #[cfg(windows)]
        assert_eq!(path, Path::new("C:/LARM/env/Scripts/python.exe"));
        #[cfg(not(windows))]
        assert_eq!(path, Path::new("C:/LARM/env/bin/python"));
    }
}
