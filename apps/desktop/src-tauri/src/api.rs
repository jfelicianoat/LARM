//! Transporte de la API para el frontend (P006, contrato 07).
//!
//! El WebView nunca recibe el token ni la URL: llama a `api_request` y este
//! backend obtiene el token del principal `desktop` por el pipe de bootstrap
//! (solo el SID del usuario puede abrirlo), hace la petición HTTP a la API
//! loopback y devuelve `{status, body}`. Ante un 401 (token rotado por otra
//! sesión) vuelve a pedir token una vez.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::Mutex;

/// Petición del frontend (misma forma que `RawRequest` en TypeScript).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawRequest {
    pub method: String,
    pub path: String,
    #[serde(default)]
    pub body: Option<serde_json::Value>,
    #[serde(default)]
    pub headers: Option<std::collections::BTreeMap<String, String>>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct RawResponse {
    pub status: u16,
    pub body: serde_json::Value,
    #[serde(default)]
    pub headers: std::collections::BTreeMap<String, String>,
}

#[derive(Clone)]
struct Session {
    base: String,
    token: String,
}

/// Estado del transporte (gestionado por Tauri).
pub struct ApiTransport {
    http: reqwest::Client,
    session: Mutex<Option<Session>>,
}

impl Default for ApiTransport {
    fn default() -> Self {
        Self {
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(60))
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .unwrap_or_default(),
            session: Mutex::new(None),
        }
    }
}

/// Cabeceras que el frontend puede enviar; `authorization` nunca.
const ALLOWED_HEADERS: &[&str] = &["idempotency-key", "if-match", "x-correlation-id"];

/// Valida método y ruta antes de cualquier IO.
///
/// # Errors
/// Texto del motivo si el método no está permitido o la ruta sale de `/api/v1/`.
pub fn validate(request: &RawRequest) -> Result<reqwest::Method, String> {
    let method = match request.method.as_str() {
        "GET" => reqwest::Method::GET,
        "POST" => reqwest::Method::POST,
        "PUT" => reqwest::Method::PUT,
        "PATCH" => reqwest::Method::PATCH,
        "DELETE" => reqwest::Method::DELETE,
        other => return Err(format!("método no permitido: {other}")),
    };
    let p = &request.path;
    let safe = p.starts_with("/api/v1/")
        && !p.contains("..")
        && !p.contains("//")
        && !p.contains('\\')
        && p.chars().all(|c| c.is_ascii_graphic());
    if !safe {
        return Err(format!("ruta no permitida: {p:.80}"));
    }
    if let Some(h) = &request.headers {
        if let Some(bad) = h
            .keys()
            .find(|k| !ALLOWED_HEADERS.contains(&k.to_ascii_lowercase().as_str()))
        {
            return Err(format!("cabecera no permitida: {bad}"));
        }
    }
    Ok(method)
}

/// Solo bases loopback HTTP: el token no puede salir de la máquina.
///
/// # Errors
/// Texto del motivo si la base no es `http://127.0.0.1:<puerto>` o `http://[::1]:<puerto>`.
pub fn validate_base(base: &str) -> Result<(), String> {
    let rest = base
        .strip_prefix("http://")
        .ok_or("api_base no es http loopback")?;
    let host = rest.rsplit_once(':').map(|(h, _)| h).unwrap_or(rest);
    if host == "127.0.0.1" || host == "[::1]" {
        Ok(())
    } else {
        Err(format!("api_base no loopback: {base:.60}"))
    }
}

/// Nombre del pipe de bootstrap para un SID (igual que el daemon).
#[must_use]
pub fn bootstrap_pipe(sid: &str) -> String {
    let digest = hex::encode(Sha256::digest(sid.as_bytes()));
    format!(r"\\.\pipe\LARM-{}-bootstrap", &digest[..16])
}

fn current_user_sid() -> Result<String, String> {
    let out = std::process::Command::new("whoami")
        .args(["/user", "/fo", "csv", "/nh"])
        .output()
        .map_err(|e| format!("whoami: {e}"))?;
    let text = String::from_utf8_lossy(&out.stdout);
    let sid = text
        .trim()
        .rsplit(',')
        .next()
        .unwrap_or_default()
        .trim()
        .trim_matches('"')
        .to_string();
    if sid.starts_with("S-1-") {
        Ok(sid)
    } else {
        Err("no se pudo obtener el SID del usuario".into())
    }
}

#[derive(Deserialize)]
struct Handoff {
    token: Option<String>,
    api_base: Option<String>,
    code: Option<String>,
}

#[cfg(windows)]
async fn bootstrap() -> Result<Session, String> {
    use tokio::net::windows::named_pipe::ClientOptions;
    let name = bootstrap_pipe(&current_user_sid()?);
    let mut attempts = 0;
    let pipe = loop {
        match ClientOptions::new().write(false).open(&name) {
            Ok(p) => break p,
            // Todas las instancias ocupadas: reintentar brevemente.
            Err(e) if e.raw_os_error() == Some(231) && attempts < 20 => {
                attempts += 1;
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Err(e) => return Err(format!("pipe de bootstrap no disponible: {e}")),
        }
    };
    let mut line = String::new();
    tokio::time::timeout(
        Duration::from_secs(10),
        BufReader::new(pipe).read_line(&mut line),
    )
    .await
    .map_err(|_| "el pipe de bootstrap no respondió".to_string())?
    .map_err(|e| format!("leyendo el pipe de bootstrap: {e}"))?;
    let handoff: Handoff =
        serde_json::from_str(&line).map_err(|_| "respuesta de bootstrap ilegible".to_string())?;
    match (handoff.token, handoff.api_base) {
        (Some(token), Some(base)) => {
            validate_base(&base)?;
            Ok(Session { base, token })
        }
        _ => Err(format!(
            "el daemon rechazó el bootstrap: {}",
            handoff.code.unwrap_or_default()
        )),
    }
}

#[cfg(not(windows))]
async fn bootstrap() -> Result<Session, String> {
    Err("el bootstrap por named pipe solo existe en Windows".into())
}

impl ApiTransport {
    async fn session(&self, renew: bool) -> Result<Session, String> {
        let mut guard = self.session.lock().await;
        if renew || guard.is_none() {
            *guard = Some(bootstrap().await?);
        }
        guard.clone().ok_or_else(|| "sin sesión".to_string())
    }

    async fn send(
        &self,
        session: &Session,
        method: &reqwest::Method,
        request: &RawRequest,
    ) -> Result<RawResponse, String> {
        let mut builder = self
            .http
            .request(method.clone(), format!("{}{}", session.base, request.path))
            .bearer_auth(&session.token);
        for (k, v) in request.headers.iter().flatten() {
            builder = builder.header(k.as_str(), v.as_str());
        }
        if let Some(body) = &request.body {
            builder = builder.json(body);
        }
        let response = builder
            .send()
            .await
            .map_err(|e| format!("API no disponible: {}", e.without_url()))?;
        let status = response.status().as_u16();
        let headers = response
            .headers()
            .get("etag")
            .and_then(|value| value.to_str().ok())
            .map(|value| {
                std::collections::BTreeMap::from([("etag".to_string(), value.to_string())])
            })
            .unwrap_or_default();
        let bytes = response
            .bytes()
            .await
            .map_err(|e| format!("respuesta incompleta: {}", e.without_url()))?;
        let body = if bytes.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or_else(|_| {
                serde_json::Value::String(String::from_utf8_lossy(&bytes).into_owned())
            })
        };
        Ok(RawResponse {
            status,
            body,
            headers,
        })
    }

    /// Ejecuta la petición; un 401 renueva el token una vez.
    ///
    /// # Errors
    /// Texto del fallo de transporte (el frontend lo trata como daemon inalcanzable).
    pub async fn request(&self, request: RawRequest) -> Result<RawResponse, String> {
        let method = validate(&request)?;
        let session = self.session(false).await?;
        let first = self.send(&session, &method, &request).await?;
        if first.status != 401 {
            return Ok(first);
        }
        let renewed = self.session(true).await?;
        self.send(&renewed, &method, &request).await
    }
}

/// Comando del frontend.
#[tauri::command]
pub async fn api_request(
    state: tauri::State<'_, ApiTransport>,
    request: RawRequest,
) -> Result<RawResponse, String> {
    state.request(request).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(method: &str, path: &str) -> RawRequest {
        RawRequest {
            method: method.into(),
            path: path.into(),
            body: None,
            headers: None,
        }
    }

    #[test]
    fn solo_metodos_y_rutas_de_la_api() {
        assert!(validate(&req("GET", "/api/v1/health/live")).is_ok());
        assert!(validate(&req(
            "PUT",
            &format!("/api/v1/runtime-profile-locks/{}", "a".repeat(64))
        ))
        .is_ok());
        assert!(validate(&req("DELETE", "/api/v1/credentials/x")).is_ok());
        for (m, p) in [
            ("TRACE", "/api/v1/x"),
            ("GET", "/admin"),
            ("GET", "/api/v1/../x"),
            ("GET", "/api/v1//x"),
            ("GET", "/api/v1/x y"),
            ("GET", "http://evil/api/v1/x"),
            ("GET", "/api/v1/a\\b"),
        ] {
            assert!(validate(&req(m, p)).is_err(), "{m} {p}");
        }
    }

    #[test]
    fn el_frontend_no_puede_poner_authorization() {
        let mut r = req("GET", "/api/v1/credentials");
        r.headers = Some([("Authorization".to_string(), "Bearer robado".to_string())].into());
        assert!(validate(&r).is_err());
        r.headers = Some([("If-Match".to_string(), "1".to_string())].into());
        assert!(validate(&r).is_ok());
    }

    #[test]
    fn base_solo_loopback() {
        assert!(validate_base("http://127.0.0.1:8790").is_ok());
        assert!(validate_base("http://[::1]:8790").is_ok());
        for bad in [
            "https://127.0.0.1:8790",
            "http://192.168.1.5:8790",
            "http://evil.example:80",
            "http://127.0.0.1.evil:1",
        ] {
            assert!(validate_base(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn pipe_igual_que_el_daemon() {
        // Mismo cálculo que apps/daemon/src/bootstrap.rs.
        let name = bootstrap_pipe("S-1-5-21-1-2-3-1001");
        assert!(name.starts_with(r"\\.\pipe\LARM-") && name.ends_with("-bootstrap"));
        assert_eq!(
            name.len(),
            r"\\.\pipe\LARM-".len() + 16 + "-bootstrap".len()
        );
    }
}
