//! Bootstrap del usuario por named pipe (P009, contrato 03).
//!
//! `\\.\pipe\LARM-{sha256(SID)[0:16]}-bootstrap` solo lo puede abrir el SID
//! del usuario que ejecuta el daemon (y SYSTEM). Cada conexión recibe una
//! línea JSON con un token nuevo del principal `desktop` y el pipe se cierra:
//! entregar un token rota el anterior, así que solo la última sesión vale
//! (handoff de un uso). El token nunca va por URL, línea de comandos ni log.

use larm_core::security::Principals;
use larm_domain::auth::{ApiScope, PrincipalKind};
use larm_domain::ports::MutationContext;
use sha2::{Digest, Sha256};

/// Nombre del principal de la aplicación de escritorio.
pub const DESKTOP_PRINCIPAL: &str = "desktop";

/// Nombre del pipe de bootstrap para un SID.
#[must_use]
pub fn pipe_name(sid: &str) -> String {
    let digest = hex::encode(Sha256::digest(sid.as_bytes()));
    format!(r"\\.\pipe\LARM-{}-bootstrap", &digest[..16])
}

/// Token nuevo del principal de escritorio (lo crea la primera vez).
///
/// El escritorio es la consola del propio operador: recibe `admin` por
/// concesión explícita del usuario dueño del SID, que es el único que puede
/// abrir el pipe.
///
/// # Errors
/// Errores del registro de principales.
pub async fn issue_desktop_token(
    principals: &Principals,
    correlation_id: &str,
) -> Result<String, larm_domain::error::LarmError> {
    let ctx = MutationContext {
        actor_id: DESKTOP_PRINCIPAL.into(),
        correlation_id: correlation_id.into(),
    };
    match principals.find(DESKTOP_PRINCIPAL).await? {
        Some(existing) => principals.rotate(&existing.id, &ctx).await,
        None => principals
            .enroll(
                PrincipalKind::Desktop,
                DESKTOP_PRINCIPAL,
                vec![ApiScope::Admin],
                vec![],
                None,
                &ctx,
            )
            .await
            .map(|(_, token)| token),
    }
}

#[cfg(windows)]
pub async fn serve(
    sid: String,
    principals: std::sync::Arc<Principals>,
    api_base: String,
) -> std::io::Result<()> {
    use tokio::io::AsyncWriteExt;
    let name = pipe_name(&sid);
    let mut server = crate::winsec::create_pipe(&name, &sid, true, 4096)?;
    tracing::info!(pipe = %name, "bootstrap del escritorio disponible (solo este usuario)");
    loop {
        server.connect().await?;
        let mut connected = server;
        server = crate::winsec::create_pipe(&name, &sid, false, 4096)?;
        let correlation = larm_domain::correlation::CorrelationId::new().to_string();
        let line = match issue_desktop_token(&principals, &correlation).await {
            Ok(token) => serde_json::json!({
                "principal": DESKTOP_PRINCIPAL,
                "token": token,
                "api_base": api_base,
            }),
            Err(error) => {
                tracing::error!(detail = %error.technical_detail(), "no se pudo emitir el token de escritorio");
                serde_json::json!({"code": error.code, "message": error.message, "retryable": error.retryable})
            }
        };
        tracing::info!(correlation_id = %correlation, "token de escritorio entregado por el pipe de bootstrap");
        let _ = connected.write_all(format!("{line}\n").as_bytes()).await;
        let _ = connected.flush().await;
        let _ = connected.disconnect();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nombre_estable_por_sid_y_distinto_del_de_credenciales() {
        let a = pipe_name("S-1-5-21-1-2-3-1001");
        assert_eq!(a, pipe_name("S-1-5-21-1-2-3-1001"));
        assert_ne!(a, pipe_name("S-1-5-21-1-2-3-1002"));
        assert!(a.ends_with("-bootstrap"));
        assert!(!a.contains("S-1-5"), "el SID no aparece en claro");
    }
}
