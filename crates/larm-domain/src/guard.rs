//! Resource Guard (P019; spec §37–§39, §64; contrato 02).
//!
//! - **Compartido** (`SHARED`) para usar artefactos (admisión de una lease,
//!   lectura): varios a la vez.
//! - **Exclusivo** (`EXCLUSIVE`) para mover, borrar o transformar: nadie más.
//! - Todo escritor toma sus locks en un **orden global** (volumen, variante,
//!   artefacto, pool; dentro de cada tipo, por id), todos o ninguno, así dos
//!   operaciones nunca se esperan en ciclo.
//! - `generation` es el token de fencing: el titular la presenta antes de
//!   cada escritura y una generación ya liberada o reconciliada no escribe.
//! - Un lock solo se retira cuando el journal o la lease **confirman** que su
//!   titular terminó; nunca por un timeout arbitrario.

use serde::{Deserialize, Serialize};

/// Tipo de recurso, en el orden global de adquisición.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ResourceKind {
    Volume,
    Variant,
    Artifact,
    Pool,
}

impl ResourceKind {
    #[must_use]
    pub fn prefix(self) -> &'static str {
        match self {
            Self::Volume => "volume",
            Self::Variant => "variant",
            Self::Artifact => "artifact",
            Self::Pool => "pool",
        }
    }
}

/// Recurso protegido. Su `resource_id` en `resource_lock` es `tipo:id`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Resource {
    pub kind: ResourceKind,
    pub id: String,
}

impl Resource {
    #[must_use]
    pub fn new(kind: ResourceKind, id: impl Into<String>) -> Self {
        Self {
            kind,
            id: id.into(),
        }
    }

    /// Clave persistida (`volume:…`, `artifact:…`).
    #[must_use]
    pub fn key(&self) -> String {
        format!("{}:{}", self.kind.prefix(), self.id)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum LockMode {
    Shared,
    Exclusive,
}

impl LockMode {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Shared => "SHARED",
            Self::Exclusive => "EXCLUSIVE",
        }
    }
}

/// Petición de locks normalizada: orden global, sin duplicados y, si un
/// recurso se pide en los dos modos, el exclusivo manda.
#[must_use]
pub fn normalize(requests: &[(Resource, LockMode)]) -> Vec<(Resource, LockMode)> {
    let mut map: std::collections::BTreeMap<Resource, LockMode> = std::collections::BTreeMap::new();
    for (resource, mode) in requests {
        let entry = map.entry(resource.clone()).or_insert(*mode);
        if *mode == LockMode::Exclusive {
            *entry = LockMode::Exclusive;
        }
    }
    map.into_iter().collect()
}

/// ¿Es compatible pedir `wanted` si ya hay un lock ajeno en `held`?
#[must_use]
pub fn compatible(wanted: LockMode, held: LockMode) -> bool {
    wanted == LockMode::Shared && held == LockMode::Shared
}

/// Paso de journal que se escribe en la misma transacción que los locks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JournalStep {
    pub id: String,
    pub kind: String,
    pub phase: String,
    pub plan_hash: String,
    pub resource_ids: Vec<String>,
    pub payload: serde_json::Value,
    /// Si no está vacío, el paso solo se escribe si el journal ya existe en
    /// una de estas fases; si no, la transacción entera se rechaza con
    /// `CONFLICT` `journal_phase_conflict` (nadie retoma un plan ya usado).
    pub from_phases: Vec<String>,
}

/// Fases de journal tras las que un titular ya no actúa.
pub const TERMINAL_PHASES: [&str; 5] = [
    "COMPLETED",
    "FAILED",
    "CANCELLED",
    "FAILED_RECOVERABLE",
    "CONSUMED",
];
/// Estados de lease terminales (schema `runtime_lease.state`).
pub const TERMINAL_LEASE_STATES: [&str; 3] = ["EXPIRED", "RELEASED", "FAILED"];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orden_global_y_exclusivo_manda() {
        let art = Resource::new(ResourceKind::Artifact, "b");
        let vol = Resource::new(ResourceKind::Volume, "z");
        let var = Resource::new(ResourceKind::Variant, "a");
        let art2 = Resource::new(ResourceKind::Artifact, "a");
        let n = normalize(&[
            (art.clone(), LockMode::Shared),
            (vol.clone(), LockMode::Shared),
            (var.clone(), LockMode::Exclusive),
            (art2.clone(), LockMode::Shared),
            (art.clone(), LockMode::Exclusive),
        ]);
        assert_eq!(
            n,
            vec![
                (vol, LockMode::Shared),
                (var, LockMode::Exclusive),
                (art2, LockMode::Shared),
                (art, LockMode::Exclusive)
            ]
        );
        // Mismo conjunto en otro orden: misma adquisición.
        let reversed: Vec<_> = n.iter().rev().cloned().collect();
        assert_eq!(normalize(&reversed), n);
    }

    #[test]
    fn compatibilidad_de_modos() {
        assert!(compatible(LockMode::Shared, LockMode::Shared));
        assert!(!compatible(LockMode::Shared, LockMode::Exclusive));
        assert!(!compatible(LockMode::Exclusive, LockMode::Shared));
        assert!(!compatible(LockMode::Exclusive, LockMode::Exclusive));
        assert_eq!(Resource::new(ResourceKind::Volume, "g").key(), "volume:g");
    }
}
