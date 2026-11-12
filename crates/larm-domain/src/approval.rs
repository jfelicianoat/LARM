//! Aprobaciones ligadas a hash (P009, contratos 03 y 07).
//!
//! Un scope no equivale a aprobar un borrado, una licencia o la ejecución de
//! código: la operación exige además un `approval_id` ligado a su
//! `operation_digest`, a sus `resource_ids`, al `code_hash` cuando hay código,
//! y al actor que aprobó. Caduca a los `security.approval_ttl_seconds` (900 s)
//! y se consume en un solo uso.

use crate::error::{ErrorCode, LarmError};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// Petición `POST /approvals` (schema `Approval`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovalRequest {
    pub operation_digest: String,
    pub resource_ids: Vec<String>,
    pub code_hash: Option<String>,
    pub permissions: serde_json::Map<String, serde_json::Value>,
    pub approved: bool,
}

/// Aprobación concedida y persistida.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovalRecord {
    pub id: String,
    pub actor_id: String,
    pub operation_digest: String,
    pub resource_ids: BTreeSet<String>,
    pub code_hash: Option<String>,
    pub permissions: serde_json::Map<String, serde_json::Value>,
    pub created_at: String,
    pub expires_at: String,
    pub consumed_at: Option<String>,
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn invalid(reason: &str, message: &str) -> LarmError {
    LarmError::new(ErrorCode::ValidationFailed, message, reason)
}

impl ApprovalRequest {
    /// # Errors
    /// `VALIDATION_FAILED` si los hashes no son sha256 en minúsculas, faltan
    /// recursos o `approved` es `false` (rechazar no crea aprobación).
    pub fn validate(&self) -> Result<(), LarmError> {
        if !is_sha256(&self.operation_digest) {
            return Err(invalid(
                "operation_digest_invalid",
                "operation_digest no es sha256",
            ));
        }
        if self.code_hash.as_deref().is_some_and(|h| !is_sha256(h)) {
            return Err(invalid("code_hash_invalid", "code_hash no es sha256"));
        }
        if self.resource_ids.is_empty() || self.resource_ids.iter().any(String::is_empty) {
            return Err(invalid(
                "resource_ids_missing",
                "La aprobación debe nombrar sus recursos",
            ));
        }
        if !self.approved {
            return Err(invalid(
                "approval_not_granted",
                "approved=false no concede nada",
            ));
        }
        Ok(())
    }

    /// Concede la aprobación a `actor_id` durante `ttl`.
    ///
    /// # Errors
    /// Ver [`ApprovalRequest::validate`].
    pub fn grant(
        &self,
        id: impl Into<String>,
        actor_id: impl Into<String>,
        now: chrono::DateTime<chrono::Utc>,
        ttl: chrono::Duration,
    ) -> Result<ApprovalRecord, LarmError> {
        self.validate()?;
        Ok(ApprovalRecord {
            id: id.into(),
            actor_id: actor_id.into(),
            operation_digest: self.operation_digest.clone(),
            resource_ids: self.resource_ids.iter().cloned().collect(),
            code_hash: self.code_hash.clone(),
            permissions: self.permissions.clone(),
            created_at: crate::bridge::timestamp(now),
            expires_at: crate::bridge::timestamp(now + ttl),
            consumed_at: None,
        })
    }
}

/// Lo que la operación presenta al usar una aprobación.
#[derive(Debug, Clone)]
pub struct ApprovalUse<'a> {
    pub actor_id: &'a str,
    pub operation_digest: &'a str,
    pub resource_ids: &'a [String],
    pub code_hash: Option<&'a str>,
}

impl ApprovalRecord {
    /// Comprueba que la aprobación cubre exactamente esta operación.
    ///
    /// # Errors
    /// `APPROVAL_REQUIRED` (no reintentable) indicando en `details.reason` por qué no vale.
    pub fn check(
        &self,
        usage: &ApprovalUse<'_>,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<(), LarmError> {
        let refuse = |reason: &str, message: &str| {
            Err(LarmError::new(ErrorCode::ApprovalRequired, message, reason)
                .with_resource(self.id.clone()))
        };
        if self.consumed_at.is_some() {
            return refuse("approval_consumed", "La aprobación ya se usó");
        }
        let expires = chrono::DateTime::parse_from_rfc3339(&self.expires_at)
            .map(|t| t.with_timezone(&chrono::Utc))
            .map_err(|_| invalid("approval_corrupt", "Aprobación con caducidad ilegible"))?;
        if now >= expires {
            return refuse("approval_expired", "La aprobación ha caducado");
        }
        if usage.actor_id != self.actor_id {
            return refuse("approval_actor_mismatch", "La aprobación es de otro actor");
        }
        if usage.operation_digest != self.operation_digest {
            return refuse("approval_digest_mismatch", "La operación no es la aprobada");
        }
        let presented: BTreeSet<String> = usage.resource_ids.iter().cloned().collect();
        if presented != self.resource_ids {
            return refuse(
                "approval_resources_mismatch",
                "Los recursos no son los aprobados",
            );
        }
        if usage.code_hash != self.code_hash.as_deref() {
            return refuse("approval_code_hash_mismatch", "El código no es el aprobado");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    const D: &str = "a3c80076e1c79a5594e0595a8a146759ad6760d4b88b684cbd49415a63c22db9";
    const C: &str = "9465e63a22add5354d9bb4b99e90117043c7124007664907259bd16d043bb031";

    fn request() -> ApprovalRequest {
        ApprovalRequest {
            operation_digest: D.into(),
            resource_ids: vec!["v1".into(), "v2".into()],
            code_hash: Some(C.into()),
            permissions: serde_json::Map::new(),
            approved: true,
        }
    }

    fn now() -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339("2026-10-11T10:00:00.000Z")
            .unwrap()
            .with_timezone(&chrono::Utc)
    }

    fn record() -> ApprovalRecord {
        request()
            .grant("ap", "operator", now(), chrono::Duration::seconds(900))
            .unwrap()
    }

    fn usage<'a>(ids: &'a [String]) -> ApprovalUse<'a> {
        ApprovalUse {
            actor_id: "operator",
            operation_digest: D,
            resource_ids: ids,
            code_hash: Some(C),
        }
    }

    fn reason(r: Result<(), LarmError>) -> String {
        let e = r.unwrap_err();
        assert_eq!(e.code, ErrorCode::ApprovalRequired);
        assert!(!e.retryable);
        e.details.reason
    }

    #[test]
    fn concede_con_ttl_y_valida_el_uso_exacto() {
        let r = record();
        assert_eq!(r.expires_at, "2026-10-11T10:15:00.000Z");
        let ids = vec!["v2".to_string(), "v1".to_string()]; // el orden no importa
        r.check(&usage(&ids), now() + chrono::Duration::seconds(899))
            .unwrap();
    }

    #[test]
    fn caducada_consumida_o_distinta_se_rechaza() {
        let ids = vec!["v1".to_string(), "v2".to_string()];
        let r = record();
        assert_eq!(
            reason(r.check(&usage(&ids), now() + chrono::Duration::seconds(900))),
            "approval_expired"
        );
        let mut used = record();
        used.consumed_at = Some("2026-10-11T10:01:00.000Z".into());
        assert_eq!(reason(used.check(&usage(&ids), now())), "approval_consumed");
        let other = ApprovalUse {
            actor_id: "mcp",
            ..usage(&ids)
        };
        assert_eq!(reason(r.check(&other, now())), "approval_actor_mismatch");
        let digest = "0".repeat(64);
        let wrong = ApprovalUse {
            operation_digest: &digest,
            ..usage(&ids)
        };
        assert_eq!(reason(r.check(&wrong, now())), "approval_digest_mismatch");
        let fewer = vec!["v1".to_string()];
        assert_eq!(
            reason(r.check(&usage(&fewer), now())),
            "approval_resources_mismatch"
        );
        let nocode = ApprovalUse {
            code_hash: None,
            ..usage(&ids)
        };
        assert_eq!(
            reason(r.check(&nocode, now())),
            "approval_code_hash_mismatch"
        );
    }

    #[test]
    fn peticiones_invalidas_se_rechazan() {
        type Mutation = fn(&mut ApprovalRequest);
        let cases: [(Mutation, &str); 5] = [
            (
                |r| r.operation_digest = "ABC".into(),
                "operation_digest_invalid",
            ),
            (
                |r| r.operation_digest = D.to_uppercase(),
                "operation_digest_invalid",
            ),
            (|r| r.code_hash = Some("x".into()), "code_hash_invalid"),
            (|r| r.resource_ids.clear(), "resource_ids_missing"),
            (|r| r.approved = false, "approval_not_granted"),
        ];
        for (mutate, why) in cases {
            let mut r = request();
            mutate(&mut r);
            assert_eq!(r.validate().unwrap_err().details.reason, why);
        }
        let extra = serde_json::json!({"operation_digest": D, "resource_ids": ["v"], "code_hash": null,
            "permissions": {}, "approved": true, "actor": "yo"});
        assert!(
            serde_json::from_value::<ApprovalRequest>(extra).is_err(),
            "el actor no lo pone el cliente"
        );
    }
}
