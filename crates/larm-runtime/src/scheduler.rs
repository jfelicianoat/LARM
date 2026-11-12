//! Políticas deterministas para starts y auto-unload (P035; contrato 04).
//!
//! El scheduler sólo elige instancias locales que puede descargar; nunca
//! decide el modelo de una petición y nunca expulsa una lease activa, un perfil
//! PINNED ni una instancia configurada como warm.

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use larm_domain::{instances::InstanceState, runtime::RuntimeOwnership};

use crate::memory::DEFAULT_IDLE_TIMEOUT_SECONDS;

/// Prioridad de una instancia o solicitud de arranque.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum InstancePriority {
    Pinned,
    High,
    Normal,
    Background,
}

impl InstancePriority {
    #[must_use]
    pub const fn weight(self) -> u8 {
        match self {
            Self::Pinned => 100,
            Self::High => 80,
            Self::Normal => 40,
            Self::Background => 10,
        }
    }
}

/// Política configurable de descarga automática.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum AutoUnloadPolicy {
    Never,
    IdleTimeout,
    MemoryPressure,
    Lru,
}

/// Orden de arranque solicitado antes de iniciar procesos.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueuedStart {
    pub id: String,
    pub priority: InstancePriority,
    /// Timestamp RFC3339 UTC canónico; es lexicográficamente ordenable.
    pub created_at: String,
}

/// Slot libre del límite de procesos que pueden arrancar a la vez.
pub fn available_start_slots(max_starting: u32, starting_now: u32) -> Result<u32, &'static str> {
    if max_starting == 0 {
        return Err("max_starting_must_be_positive");
    }
    Ok(max_starting.saturating_sub(starting_now))
}

/// Prioridad descendente y FIFO por created_at/id dentro de cada grupo.
pub fn order_start_queue(queue: &mut [QueuedStart]) {
    queue.sort_by(|left, right| {
        right
            .priority
            .weight()
            .cmp(&left.priority.weight())
            .then_with(|| left.created_at.cmp(&right.created_at))
            .then_with(|| left.id.cmp(&right.id))
    });
}

/// Snapshot mínimo de scheduler para una instancia ya cargada.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScheduledInstance {
    pub id: String,
    pub state: InstanceState,
    pub ownership: RuntimeOwnership,
    pub priority: InstancePriority,
    pub active_lease_count: u32,
    pub configured_warm: bool,
    pub created_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
}

impl ScheduledInstance {
    fn is_idle_managed_candidate(&self) -> bool {
        self.ownership == RuntimeOwnership::LarmManaged
            && matches!(self.state, InstanceState::Ready | InstanceState::Degraded)
            && self.active_lease_count == 0
            && self.priority != InstancePriority::Pinned
            && !self.configured_warm
    }

    fn last_use(&self) -> DateTime<Utc> {
        self.last_used_at.unwrap_or(self.created_at)
    }
}

/// Devuelve el ID de la instancia elegible. La detención efectiva queda en el
/// lifecycle supervisado y vuelve a comprobar leases antes del stop.
#[must_use]
pub fn choose_auto_unload(
    policy: AutoUnloadPolicy,
    memory_pressure: bool,
    now: DateTime<Utc>,
    instances: &[ScheduledInstance],
) -> Option<&str> {
    if policy == AutoUnloadPolicy::Never
        || (matches!(
            policy,
            AutoUnloadPolicy::MemoryPressure | AutoUnloadPolicy::Lru
        ) && !memory_pressure)
    {
        return None;
    }
    let idle_before = now - Duration::seconds(DEFAULT_IDLE_TIMEOUT_SECONDS);
    let mut candidates = instances
        .iter()
        .filter(|instance| instance.is_idle_managed_candidate())
        .filter(|instance| {
            policy != AutoUnloadPolicy::IdleTimeout || instance.last_use() <= idle_before
        })
        .collect::<Vec<_>>();
    match policy {
        AutoUnloadPolicy::Never => return None,
        AutoUnloadPolicy::IdleTimeout | AutoUnloadPolicy::MemoryPressure => {
            candidates.sort_by(|left, right| {
                left.priority
                    .weight()
                    .cmp(&right.priority.weight())
                    .then_with(|| left.last_use().cmp(&right.last_use()))
                    .then_with(|| left.id.cmp(&right.id))
            });
        }
        AutoUnloadPolicy::Lru => {
            candidates.sort_by(|left, right| {
                left.last_use()
                    .cmp(&right.last_use())
                    .then_with(|| left.priority.weight().cmp(&right.priority.weight()))
                    .then_with(|| left.id.cmp(&right.id))
            });
        }
    }
    candidates.first().map(|instance| instance.id.as_str())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    fn time(seconds: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(seconds, 0).unwrap()
    }

    fn instance(id: &str, priority: InstancePriority, used_at: i64) -> ScheduledInstance {
        ScheduledInstance {
            id: id.into(),
            state: InstanceState::Ready,
            ownership: RuntimeOwnership::LarmManaged,
            priority,
            active_lease_count: 0,
            configured_warm: false,
            created_at: time(used_at - 60),
            last_used_at: Some(time(used_at)),
        }
    }

    #[test]
    fn ordena_prioridad_y_fifa_y_respeta_slots_de_arranque() {
        let mut queue = vec![
            QueuedStart {
                id: "normal".into(),
                priority: InstancePriority::Normal,
                created_at: "2026-10-22T10:00:00.000Z".into(),
            },
            QueuedStart {
                id: "high-late".into(),
                priority: InstancePriority::High,
                created_at: "2026-10-22T10:00:02.000Z".into(),
            },
            QueuedStart {
                id: "high-early".into(),
                priority: InstancePriority::High,
                created_at: "2026-10-22T10:00:01.000Z".into(),
            },
            QueuedStart {
                id: "background".into(),
                priority: InstancePriority::Background,
                created_at: "2026-10-22T09:00:00.000Z".into(),
            },
        ];
        order_start_queue(&mut queue);
        assert_eq!(
            queue
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            ["high-early", "high-late", "normal", "background"]
        );
        assert_eq!(available_start_slots(1, 0), Ok(1));
        assert_eq!(available_start_slots(1, 1), Ok(0));
        assert_eq!(
            available_start_slots(0, 0),
            Err("max_starting_must_be_positive")
        );
    }

    #[test]
    fn protege_leases_pinned_warm_y_procesos_externos() {
        let now = time(10_000);
        let mut leased = instance("leased", InstancePriority::Background, 1);
        leased.active_lease_count = 1;
        let pinned = instance("pinned", InstancePriority::Pinned, 1);
        let mut warm = instance("warm", InstancePriority::Normal, 1);
        warm.configured_warm = true;
        let mut external = instance("external", InstancePriority::Background, 1);
        external.ownership = RuntimeOwnership::External;
        let items = [leased, pinned, warm, external];
        assert_eq!(
            choose_auto_unload(AutoUnloadPolicy::MemoryPressure, true, now, &items),
            None
        );
    }

    #[test]
    fn memory_pressure_elige_menor_prioridad_y_usa_lru_como_desempate() {
        let now = time(10_000);
        let items = [
            instance("normal-old", InstancePriority::Normal, 2_000),
            instance("background-new", InstancePriority::Background, 8_000),
            instance("background-old", InstancePriority::Background, 1_000),
        ];
        assert_eq!(
            choose_auto_unload(AutoUnloadPolicy::MemoryPressure, true, now, &items),
            Some("background-old")
        );
        assert_eq!(
            choose_auto_unload(AutoUnloadPolicy::MemoryPressure, false, now, &items),
            None
        );
    }

    #[test]
    fn lru_prioriza_ultimo_uso_y_prioridad_en_empate() {
        let now = time(10_000);
        let items = [
            instance("high-tie", InstancePriority::High, 1_000),
            instance("normal-tie", InstancePriority::Normal, 1_000),
            instance("background-new", InstancePriority::Background, 5_000),
        ];
        assert_eq!(
            choose_auto_unload(AutoUnloadPolicy::Lru, true, now, &items),
            Some("normal-tie")
        );
        assert_eq!(
            choose_auto_unload(AutoUnloadPolicy::Never, true, now, &items),
            None
        );
    }

    #[test]
    fn idle_timeout_exige_trescientos_segundos_y_estado_idle() {
        let now = time(10_000);
        let mut recent = instance("recent", InstancePriority::Normal, now.timestamp() - 299);
        let mut old = instance("old", InstancePriority::Normal, now.timestamp() - 301);
        let mut busy = instance(
            "busy",
            InstancePriority::Background,
            now.timestamp() - 1_000,
        );
        busy.state = InstanceState::Busy;
        recent.created_at = now;
        old.created_at = now;
        let items = [recent, old, busy];
        assert_eq!(
            choose_auto_unload(AutoUnloadPolicy::IdleTimeout, false, now, &items),
            Some("old")
        );
    }
}
