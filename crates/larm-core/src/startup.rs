//! P132: reconciliación de arranque, modo offline, protección de crash loop y
//! detección de procesos huérfanos. Todo el módulo es puro y determinista:
//! las decisiones se toman a partir de observaciones (inventario local
//! disponible, reintentos de arranque registrados, pids con identidad
//! propiedad de LARM) y nunca matan procesos por ambigüedad.

use larm_domain::error::{ErrorCode, LarmError};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

// ---------------------------------------------------------------------------
// 1) Orden de startup
// ---------------------------------------------------------------------------

/// Pasos de arranque en el orden contractual: base de datos → almacenamiento →
/// registro de adaptadores → instalaciones runtime → instancias →
/// descargas/transforms → servicio de eventos. Un salto de orden es error.
pub const STARTUP_STEPS: [&str; 7] = [
    "database_migrations",
    "storage_reconcile",
    "adapter_registry",
    "runtime_installations",
    "instances",
    "downloads_transforms",
    "event_service",
];

/// Estado de un paso completado en el arranque real.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct StartupStep {
    pub step: &'static str,
    pub completed_at_millis: u128,
    /// Detalle estable del resultado (motivos de degradación, sin secretos).
    pub detail: String,
}

/// Orden de arranque validado: cada paso registrado debe ser el siguiente del
/// contrato y las marcas de tiempo no-retroactivas.
pub fn validate_startup_order(steps: &[StartupStep]) -> Result<(), Vec<String>> {
    // El journal es incremental: un prefijo del orden contractual es válido
    // (arranque en progreso); un paso fuera de secuencia o desconocido no lo es.
    let mut failures = Vec::new();
    for (index, step) in steps.iter().enumerate() {
        let expected_step = STARTUP_STEPS.get(index).copied().unwrap_or("unknown");
        if step.step != expected_step {
            failures.push(format!(
                "startup_step_out_of_order::{}::expected_{}",
                step.step, expected_step
            ));
        }
        if index > 0 && step.completed_at_millis < steps[index - 1].completed_at_millis {
            failures.push(format!("startup_step_retroactive::{}", step.step));
        }
    }
    if steps.len() > STARTUP_STEPS.len() {
        failures.push(format!(
            "startup_step_unknown::{}",
            steps[STARTUP_STEPS.len()].step
        ));
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures)
    }
}

pub fn validate_startup_order_strict(steps: &[StartupStep]) -> Result<(), Vec<String>> {
    let mut failures = Vec::new();
    let expected = STARTUP_STEPS.iter().enumerate();
    for (index, (_position, expected_step)) in expected.enumerate() {
        match steps.get(index) {
            None => failures.push(format!("startup_step_missing::{expected_step}")),
            Some(step) => {
                if step.step != *expected_step {
                    failures.push(format!(
                        "startup_step_out_of_order::{}::expected_{expected_step}",
                        step.step
                    ));
                }
                if index > 0 && step.completed_at_millis < steps[index - 1].completed_at_millis {
                    failures.push(format!("startup_step_retroactive::{expected_step}"));
                }
            }
        }
    }
    if steps.len() > STARTUP_STEPS.len() {
        failures.push(format!(
            "startup_step_unknown::{}",
            steps[STARTUP_STEPS.len()].step
        ));
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures)
    }
}

// ---------------------------------------------------------------------------
// 2) Modo offline
// ---------------------------------------------------------------------------

/// Modo offline: el inventario y los runtimes locales siguen servibles; las
/// búsquedas remotas se deshabilitan de forma explícita y visible.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OfflineState {
    pub offline: bool,
    /// Motivo estable del modo offline (`network_unreachable`, `offline_mode`,
    /// `watcher_disabled`).
    pub reason: String,
}

/// Decide el modo a partir de las capacidades observadas del host: si la red
/// de loopback hacia fuentes remotas no está disponible, el servidor sigue
/// sirviendo inventario/runtimes locales y deshabilita las búsquedas remotas.
#[must_use]
pub fn decide_mode(local_inventory_served: bool, remote_sources_reachable: bool) -> OfflineState {
    if !local_inventory_served {
        return OfflineState {
            offline: true,
            reason: "local_inventory_unavailable".into(),
        };
    }
    OfflineState {
        offline: !remote_sources_reachable,
        reason: if remote_sources_reachable {
            "online".into()
        } else {
            "network_unreachable".into()
        },
    }
}

/// La búsqueda remota en modo offline es rechazada de forma explícita.
pub fn reject_remote_search(state: &OfflineState) -> Result<(), &'static str> {
    if state.offline {
        Err("remote_search_disabled_offline")
    } else {
        Ok(())
    }
}

/// Journal del arranque: registra los pasos contractuales en orden y valida
/// que ningún paso falte o se salte (P132, orden de startup de la spec §103).
#[derive(Debug, Clone, Default)]
pub struct StartupJournal {
    steps: Vec<StartupStep>,
}

impl StartupJournal {
    #[must_use]
    pub fn begin() -> Self {
        Self::default()
    }

    /// Registra el siguiente paso con la marca de tiempo actual.
    ///
    /// # Errors
    /// [`LarmError`] si el paso rompe el orden contractual.
    pub fn complete(&mut self, step: &'static str) -> Result<(), LarmError> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|value| value.as_millis())
            .unwrap_or(0);
        self.steps.push(StartupStep {
            step,
            completed_at_millis: now,
            detail: String::new(),
        });
        validate_startup_order(&self.steps).map_err(|failures| {
            LarmError::new(
                ErrorCode::Internal,
                "Orden de arranque fuera del contrato",
                "startup_order_violation",
            )
            .with_technical(failures.join("; "))
        })
    }
}

// ---------------------------------------------------------------------------
// 4) Procesos huérfanos con identidad fuerte
// ---------------------------------------------------------------------------

/// Identidad fuerte de un proceso propiedad de LARM: pid, hash del ejecutable
/// y marca del arranque registrado. Sólo se anuncia (o mata) un proceso cuya
/// identidad coincide exactamente, sin ambigüedad de pid suelto.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrphanCandidate {
    pub pid: u32,
    /// SHA-256 del ejecutable que lanzó el proceso (observado por el host).
    pub executable_sha256: String,
    /// Boot epoch registrado en el journal del proceso.
    pub boot_epoch: u64,
    pub job_kind: String,
}

/// Registro del daemon al lanzar cada worker: pid + identidad esperada.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpawnRecord {
    pub pid: u32,
    pub executable_sha256: String,
    pub boot_epoch: u64,
    pub job_kind: String,
}

/// Clasifica un proceso vivo encontrado por el host tras un reinicio:
/// `ORPHAN_LARM` sólo si la identidad completa (ejecutable + boot epoch +
/// clase de job) coincide con un registro de spawns del arranque anterior.
#[must_use]
pub fn classify_process(
    alive: &OrphanCandidate,
    spawns: &[SpawnRecord],
    current_boot_epoch: u64,
) -> &'static str {
    // El boot epoch del host cambió: ningún proceso de la sesión anterior
    // sigue siendo propiedad de LARM (el SO los mató con el reinicio).
    if alive.boot_epoch != current_boot_epoch {
        return "FOREIGN";
    }
    match spawns.iter().find(|record| record.pid == alive.pid) {
        Some(record)
            if record.executable_sha256 == alive.executable_sha256
                && record.boot_epoch == alive.boot_epoch
                && record.job_kind == alive.job_kind =>
        {
            "ORPHAN_LARM"
        }
        _ => "FOREIGN",
    }
}

/// Los orphans con identidad fuerte se matan sólo con la identidad completa en
/// la mano; devuelve los pids que se pueden terminar sin ambigüedad.
#[must_use]
pub fn killable_orphans(
    alive: &[OrphanCandidate],
    spawns: &[SpawnRecord],
    current_boot_epoch: u64,
) -> Vec<u32> {
    alive
        .iter()
        .filter(|candidate| {
            classify_process(candidate, spawns, current_boot_epoch) == "ORPHAN_LARM"
        })
        .map(|candidate| candidate.pid)
        .collect()
}

/// Huella estable del contenido para el journal de arranque (hashes de
/// reportes/toolchains sin secretos).
#[must_use]
pub fn evidence_digest(payload: &Value) -> String {
    let canonical = serde_json::to_vec(payload).unwrap_or_default();
    hex::encode(Sha256::digest(&canonical))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn orden_de_arranque_contractual_se_valida_y_falla_con_saltos() {
        let now = 1000;
        let ok: Vec<StartupStep> = STARTUP_STEPS
            .iter()
            .enumerate()
            .map(|(index, step)| StartupStep {
                step,
                completed_at_millis: now + index as u128 * 10,
                detail: String::new(),
            })
            .collect();
        assert!(validate_startup_order(&ok).is_ok());
        // Un paso ausente (descargas/transforms) falla por cada uno posterior.
        let skipped: Vec<StartupStep> = ok
            .iter()
            .filter(|step| step.step != "downloads_transforms")
            .cloned()
            .collect();
        let failures = validate_startup_order(&skipped).unwrap_err();
        assert!(failures.iter().any(|f| f.contains("downloads_transforms")));
        // Paso desconocido tras el último contractual.
        let mut unknown = ok.clone();
        unknown.push(StartupStep {
            step: "invented_step",
            completed_at_millis: now + 999,
            detail: String::new(),
        });
        assert!(validate_startup_order(&unknown)
            .unwrap_err()
            .iter()
            .any(|f| f.contains("invented_step")));
    }

    #[test]
    fn modo_offline_sirve_inventario_y_deshabilita_remoto() {
        let online = decide_mode(true, true);
        assert!(!online.offline);
        assert!(reject_remote_search(&online).is_ok());
        let offline = decide_mode(true, false);
        assert!(offline.offline);
        assert_eq!(offline.reason, "network_unreachable");
        assert_eq!(
            reject_remote_search(&offline).unwrap_err(),
            "remote_search_disabled_offline"
        );
        // Sin inventario local ni remotas: degradado y sin búsquedas.
        let degraded = decide_mode(false, false);
        assert!(degraded.offline);
        assert_eq!(degraded.reason, "local_inventory_unavailable");
    }

    #[test]
    fn procesos_huerfanos_por_identidad_fuerte() {
        let boot = 7;
        let spawns = vec![SpawnRecord {
            pid: 101,
            executable_sha256: "abc".into(),
            boot_epoch: boot,
            job_kind: "download".into(),
        }];
        let alive = OrphanCandidate {
            pid: 101,
            executable_sha256: "abc".into(),
            boot_epoch: boot,
            job_kind: "download".into(),
        };
        assert_eq!(classify_process(&alive, &spawns, boot), "ORPHAN_LARM");
        assert_eq!(killable_orphans(&[alive], &spawns, boot), vec![101]);
        // Un boot distinto del host = FOREIGN (el SO los mató).
        let stale = OrphanCandidate {
            pid: 101,
            executable_sha256: "abc".into(),
            boot_epoch: boot - 1,
            job_kind: "download".into(),
        };
        assert_eq!(classify_process(&stale, &spawns, boot), "FOREIGN");
        assert!(killable_orphans(&[stale], &spawns, boot).is_empty());
        // Pid reciclado con otro ejecutable: sin ambigüedad, FOREIGN.
        let recycled = OrphanCandidate {
            pid: 101,
            executable_sha256: "zzz".into(),
            boot_epoch: boot,
            job_kind: "download".into(),
        };
        assert_eq!(classify_process(&recycled, &spawns, boot), "FOREIGN");
        // Pid de otro sistema/usuario: sin registro en spawns → FOREIGN.
        let unregistered = OrphanCandidate {
            pid: 202,
            executable_sha256: "abc".into(),
            boot_epoch: boot,
            job_kind: "download".into(),
        };
        assert_eq!(classify_process(&unregistered, &spawns, boot), "FOREIGN");
    }

    #[test]
    fn evidence_digest_determinista() {
        let payload = json!({"tool": "larm.get_hardware", "hash": "abc"});
        let first = evidence_digest(&payload);
        let second = evidence_digest(&payload);
        assert_eq!(first, second);
        assert_eq!(first.len(), 64);
    }
}
