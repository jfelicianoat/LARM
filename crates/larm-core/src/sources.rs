//! P043: validar configuración sin conectar, leer directorios ni resolver secretos.
use larm_adapter_sdk::source::{validate_source_binding, SearchSupport};
use larm_domain::error::{ErrorCode, LarmError};
use larm_domain::ports::{AdapterInstallationRepository, MutationContext, SourceCatalogRepository};
use larm_domain::sources::{
    source_invalid, AuthMode, ModelSource, ModelSourceAdapter, ModelSourceCreate,
};
use serde_json::{Map, Value};
use std::sync::Arc;

pub struct SourceCatalog {
    pub repo: Arc<dyn SourceCatalogRepository>,
    pub installations: Arc<dyn AdapterInstallationRepository>,
}

/// Normaliza presentación de URL; no acredita conectividad, permisos, licencia,
/// autenticación ni cualificación del adaptador. Rutas offline son configurables.
pub fn locator(source_type: &str, text: &str, auth: &AuthMode) -> Result<String, LarmError> {
    if text.is_empty() || text.chars().any(char::is_control) {
        return Err(source_invalid("base_url"));
    }
    if matches!(source_type, "LOCAL" | "UNC_PRIVATE") {
        let path = if text.starts_with("file:") {
            let url = url::Url::parse(text).map_err(|_| source_invalid("base_url"))?;
            if !url.username().is_empty()
                || url.password().is_some()
                || url.query().is_some()
                || url.fragment().is_some()
            {
                return Err(source_invalid("base_url"));
            }
            url.to_file_path()
                .map_err(|_| source_invalid("base_url"))?
                .to_string_lossy()
                .into_owned()
        } else {
            text.to_string()
        };
        let unc = path.starts_with(r"\\");
        if path.starts_with(r"\\?\")
            || path.starts_with(r"\\.\")
            || (unc && path.contains(':'))
            || (!unc && path.get(2..).is_some_and(|p| p.contains(':')))
        {
            return Err(source_invalid("base_url"));
        }
        let drive = path.as_bytes().first().is_some_and(u8::is_ascii_alphabetic)
            && path.as_bytes().get(1) == Some(&b':')
            && matches!(path.as_bytes().get(2), Some(b'\\' | b'/'));
        if (!unc && !drive)
            || (source_type == "UNC_PRIVATE" && !unc)
            || path.split(['\\', '/']).any(|p| p == ".." || p == ".")
        {
            return Err(source_invalid("base_url"));
        }
        if unc
            && path
                .trim_start_matches('\\')
                .split(['\\', '/'])
                .filter(|c| !c.is_empty())
                .count()
                < 2
        {
            return Err(source_invalid("base_url"));
        }
        return Ok(path);
    }
    let url = url::Url::parse(text).map_err(|_| source_invalid("base_url"))?;
    let ssh = url.scheme() == "ssh" && source_type == "GIT_LFS";
    if !matches!(url.scheme(), "http" | "https") && !ssh
        || url.host_str().is_none()
        || url.fragment().is_some()
        || url.password().is_some()
        || (!ssh && !url.username().is_empty())
    {
        return Err(source_invalid("base_url"));
    }
    for (key, _) in url.query_pairs() {
        let key = key.to_ascii_lowercase().replace(['-', '_'], "");
        if [
            "token",
            "accesstoken",
            "refreshtoken",
            "apikey",
            "password",
            "secret",
            "signature",
            "sig",
            "xamzsignature",
            "xamzcredential",
            "xamzsecuritytoken",
            "xgoogsignature",
            "xgoogcredential",
        ]
        .contains(&key.as_str())
        {
            return Err(source_invalid("secret_in_url"));
        }
    }
    if url.scheme() == "http" && *auth != AuthMode::None {
        let loopback = url.host_str().is_some_and(|host| {
            host == "localhost"
                || host
                    .trim_matches(['[', ']'])
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback())
        });
        if !loopback {
            return Err(LarmError::new(
                ErrorCode::Forbidden,
                "Una fuente con autenticación requiere un canal seguro",
                "source_auth_transport_insecure",
            ));
        }
    }
    Ok(url.to_string())
}
impl SourceCatalog {
    pub async fn register_adapter(
        &self,
        id: &str,
        source_type: &str,
        description: Value,
        ctx: &MutationContext,
    ) -> Result<ModelSourceAdapter, LarmError> {
        if !larm_domain::bridge::is_uuid_v7(id) || source_type.is_empty() {
            return Err(source_invalid("adapter_identity"));
        }
        let support: SearchSupport =
            serde_json::from_value(description).map_err(|_| source_invalid("descriptor"))?;
        let installed = self.installations.get(id).await?.ok_or_else(|| {
            LarmError::new(ErrorCode::NotFound, "Instalación desconocida", "not_found")
        })?;
        let manifest =
            larm_adapter_sdk::manifest::AdapterManifest::parse(&installed.manifest.to_string())?;
        validate_source_binding(&manifest, &support)?;
        let mut metadata = Map::new();
        metadata.insert(
            "source_protocol_v1".into(),
            serde_json::to_value(&support).map_err(|_| source_invalid("descriptor"))?,
        );
        self.repo
            .register_source_adapter(id, source_type, &metadata, ctx)
            .await
    }
    async fn normalized(
        &self,
        request: &ModelSourceCreate,
    ) -> Result<ModelSourceCreate, LarmError> {
        request.validate()?;
        let binding = self
            .repo
            .source_adapter(&request.adapter_id)
            .await?
            .ok_or_else(|| {
                LarmError::new(
                    ErrorCode::NotFound,
                    "Adaptador de fuente desconocido",
                    "not_found",
                )
            })?;
        let mut request = request.clone();
        request.base_url = locator(&binding.source_type, &request.base_url, &request.auth_mode)?;
        Ok(request)
    }
    pub async fn create(
        &self,
        request: &ModelSourceCreate,
        key: &str,
        body_hash: &str,
        ctx: &MutationContext,
    ) -> Result<ModelSource, LarmError> {
        request.validate()?;
        if let Some(original) = self.repo.source_replay(key, body_hash, ctx).await? {
            return Ok(original);
        }
        self.repo
            .create_source(&self.normalized(request).await?, key, body_hash, ctx)
            .await
    }
    pub async fn update(
        &self,
        id: &str,
        version: i64,
        request: &ModelSourceCreate,
        ctx: &MutationContext,
    ) -> Result<ModelSource, LarmError> {
        self.repo
            .update_source(id, version, &self.normalized(request).await?, ctx)
            .await
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    #[test]
    fn locator_configurable_sin_io_y_sin_credenciales_embebidas() {
        assert_eq!(
            locator(
                "HUGGING_FACE",
                "https://Example.org:443/models",
                &AuthMode::Token
            )
            .unwrap(),
            "https://example.org/models"
        );
        for url in [
            "https://user:pass@example.org",
            "https://user@example.org",
            "https://example.org?token=x",
            "https://example.org?X-Amz-Signature=x",
            "ftp://example.org/data",
            "https://example.org/#fragment",
            "relative",
        ] {
            assert!(locator("HTTP", url, &AuthMode::None).is_err(), "{url}");
        }
        assert!(locator("HTTP", "http://example.org", &AuthMode::Basic).is_err());
        assert!(locator("HTTP", "http://127.0.0.1:8100", &AuthMode::Basic).is_ok());
        assert!(locator("GIT_LFS", "ssh://git@example.org/repo", &AuthMode::Ssh).is_ok());
    }
    #[test]
    fn raices_local_y_unc_no_se_convierten_en_urls_ni_se_sondean() {
        assert!(locator("LOCAL", r"C:\NoExiste\modelos", &AuthMode::None).is_ok());
        assert!(locator(
            "UNC_PRIVATE",
            r"\\servidor\share\modelos",
            &AuthMode::WindowsIntegrated
        )
        .is_ok());
        for root in [r"relativo\modelos", r"C:\root\..\otra", r"\\servidor"] {
            assert!(locator("LOCAL", root, &AuthMode::None).is_err());
        }
        assert!(locator("UNC_PRIVATE", r"C:\modelos", &AuthMode::None).is_err());
    }
}
