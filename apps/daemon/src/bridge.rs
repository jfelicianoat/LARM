//! Piezas concretas que compone el daemon: secretos en Windows Credential
//! Manager (`keyring`), SID del usuario y la configuración del puente Broker.
//! Los principales ya no se declaran aquí: viven en `credential_reference`
//! (`larm/principal/{id}`) con su token en Credential Manager (P009).

use larm_core::bridge::SecretStore;
use larm_runtime::external_adoption::AdoptionConfig;
use serde::Deserialize;

/// Fichero `--bridge-config` del puente Broker–LARM.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BridgeConfig {
    pub adoption: AdoptionConfig,
}

/// Secretos en Credential Manager (`service`, `username`).
pub struct KeyringSecrets;

impl SecretStore for KeyringSecrets {
    fn store(&self, service: &str, username: &str, secret: &str) -> Result<(), String> {
        keyring::Entry::new(service, username)
            .and_then(|entry| entry.set_password(secret))
            .map_err(|e| format!("credential manager: {e}"))
    }

    fn read(&self, service: &str, username: &str) -> Result<Option<String>, String> {
        match keyring::Entry::new(service, username).and_then(|entry| entry.get_password()) {
            Ok(secret) => Ok(Some(secret)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(format!("credential manager: {e}")),
        }
    }

    fn delete(&self, service: &str, username: &str) -> Result<(), String> {
        match keyring::Entry::new(service, username).and_then(|entry| entry.delete_credential()) {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(format!("credential manager: {e}")),
        }
    }
}

/// SID textual del usuario actual (`whoami /user`), sin `unsafe`.
pub fn current_user_sid() -> Option<String> {
    let root = std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into());
    let mut command =
        std::process::Command::new(std::path::Path::new(&root).join("System32/whoami.exe"));
    command.args(["/user", "/fo", "csv", "/nh"]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    let output = command.output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let sid = text
        .trim()
        .rsplit(',')
        .next()?
        .trim()
        .trim_matches('"')
        .to_string();
    sid.starts_with("S-1-").then_some(sid)
}

/// ¿Es una dirección loopback? LAN exige `remote_enabled` y autorización.
#[must_use]
pub fn is_loopback(bind: &str) -> bool {
    matches!(bind, "127.0.0.1" | "::1" | "localhost")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_only() {
        assert!(is_loopback("127.0.0.1"));
        assert!(!is_loopback("0.0.0.0"));
    }

    #[test]
    fn la_config_del_puente_ya_no_declara_principales() {
        let old = r#"{"adoption": {}, "principals": [{"name": "x", "scopes": ["read"]}]}"#;
        assert!(serde_json::from_str::<BridgeConfig>(old).is_err());
    }
}
