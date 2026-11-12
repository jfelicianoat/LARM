//! StorageLocation (P014; spec §27, §29, §83, §112–§114, §136; contratos 02,
//! 07, 08 y 11 H09): registro, DTO de escritura, nivel de disco, espacio
//! utilizable compartido por volumen y políticas de colocación automática.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::custom_rules::{CustomRules, CustomRulesError, RuleContext, RuleDisposition};
use crate::storage::PerformanceClass;
use crate::units::DecimalU64;

/// Estados de una ubicación (spec §29, schema `storage_location.state`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum LocationState {
    Online,
    Offline,
    ReadOnly,
    LowSpace,
    Degraded,
    Missing,
    Error,
    Unknown,
}

impl LocationState {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Online => "ONLINE",
            Self::Offline => "OFFLINE",
            Self::ReadOnly => "READ_ONLY",
            Self::LowSpace => "LOW_SPACE",
            Self::Degraded => "DEGRADED",
            Self::Missing => "MISSING",
            Self::Error => "ERROR",
            Self::Unknown => "UNKNOWN",
        }
    }
}

/// Registro `storage_location` (schema cerrado).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StorageLocation {
    pub id: String,
    pub created_at: String,
    pub updated_at: String,
    pub row_version: i64,
    pub metadata: Map<String, Value>,
    pub name: String,
    pub root_path: String,
    pub canonical_path: String,
    pub volume_guid: Option<String>,
    pub drive_letter: Option<String>,
    pub unc_path: Option<String>,
    pub filesystem: String,
    pub storage_type: String,
    pub capacity_bytes: DecimalU64,
    pub free_bytes: DecimalU64,
    pub reserved_bytes: DecimalU64,
    pub priority: u32,
    pub auto_placement: bool,
    pub state: LocationState,
    pub last_seen_at: Option<String>,
    pub volume_key: String,
    pub performance_class: PerformanceClass,
}

/// DTO de escritura (schema `storage_locationCreate`; contrato 07: el
/// servidor calcula GUID, ruta canónica, capacidad, espacio, estado y fechas).
/// `PATCH` usa el mismo DTO completo.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StorageLocationCreate {
    pub name: String,
    pub root_path: String,
    pub reserved_bytes: DecimalU64,
    pub priority: u32,
    pub auto_placement: bool,
    /// Pista de clase: solo se usa si la observada es UNKNOWN.
    pub performance_class: PerformanceClass,
}

/// Prefijo de `volume_key` para un volumen cuya identidad no se pudo
/// verificar: bloquea la colocación automática (contrato 02).
pub const UNKNOWN_VOLUME_PREFIX: &str = "unknown:";

/// Clase de operación de un sondeo de ubicación en `operation_journal`.
pub const PROBE_JOB_KIND: &str = "storage.probe";

/// Umbrales de disco bajo (H09), ya validados (`critical < warning`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiskThresholds {
    Percent { warning_bp: u64, critical_bp: u64 },
    Bytes { warning: u64, critical: u64 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum DiskLevel {
    Normal,
    Warning,
    Critical,
}

/// Nivel por volumen (H09): comparación estricta (la igualdad no cruza) con
/// aritmética comprobada; sin capacidad conocida no hay nivel.
#[must_use]
pub fn disk_level(free: u64, capacity: u64, thresholds: DiskThresholds) -> Option<DiskLevel> {
    if capacity == 0 {
        return None;
    }
    let below =
        |limit_bp: u64| u128::from(free) * 10_000 < u128::from(capacity) * u128::from(limit_bp);
    let (critical, warning) = match thresholds {
        DiskThresholds::Percent {
            warning_bp,
            critical_bp,
        } => (below(critical_bp), below(warning_bp)),
        DiskThresholds::Bytes { warning, critical } => (free < critical, free < warning),
    };
    Some(if critical {
        DiskLevel::Critical
    } else if warning {
        DiskLevel::Warning
    } else {
        DiskLevel::Normal
    })
}

/// Espacio utilizable de una raíz: `free - reserved`, nunca negativo.
#[must_use]
pub fn usable_bytes(free: u64, reserved: u64) -> u64 {
    free.saturating_sub(reserved)
}

/// Presupuesto de un volumen compartido por varias raíces: la reserva del
/// volumen es la **mayor** de sus raíces, no la suma (contrato 02).
#[must_use]
pub fn volume_budget(free: u64, reservations: &[u64]) -> u64 {
    usable_bytes(free, reservations.iter().copied().max().unwrap_or(0))
}

/// Políticas de colocación (spec §113, contrato 02).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlacementPolicy {
    AskAlways,
    PreferredLocation(Option<String>),
    MostFreeSpace,
    FastestStorage,
    /// Tipo de modelo (`LLM`, `DIFFUSION`, `AUDIO`) → id de ubicación.
    ByModelType(std::collections::BTreeMap<String, Option<String>>),
    /// Solo modelo de datos por ahora (P014): nunca elige sola.
    CustomRules,
}

/// Petición de colocación.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlacementRequest {
    /// Bytes nuevos que hay que alojar (ya con margen, de P015).
    pub required_bytes: u64,
    /// Mayor fichero individual, para el límite de FAT32.
    pub largest_file_bytes: u64,
    pub model_type: Option<String>,
}

/// Resultado de la colocación automática.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Placement {
    Chosen(String),
    /// Hay que preguntar al usuario; `reason` dice por qué no se eligió sola.
    Ask {
        reason: &'static str,
    },
    /// Ninguna ubicación cumple los requisitos.
    NoEligible {
        reasons: Vec<(String, &'static str)>,
    },
}

/// Ubicación candidata con el presupuesto de su volumen ya calculado.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub id: String,
    pub state: LocationState,
    pub auto_placement: bool,
    pub volume_key: String,
    pub filesystem: String,
    pub performance_class: PerformanceClass,
    pub priority: u32,
    /// Utilizable del volumen tras la mayor reserva de sus raíces y las
    /// reservas de otros trabajos.
    pub usable_bytes: u64,
}

const FAT32_MAX_FILE: u64 = (1 << 32) - 1;

fn speed_rank(class: PerformanceClass) -> u8 {
    match class {
        PerformanceClass::Nvme => 0,
        PerformanceClass::Ssd => 1,
        PerformanceClass::Hdd => 2,
        PerformanceClass::Network => 3,
        PerformanceClass::Removable => 4,
        PerformanceClass::Unknown => 5,
    }
}

/// Hard gates comunes a todas las políticas.
fn gate(c: &Candidate, request: &PlacementRequest) -> Result<(), &'static str> {
    if c.state != LocationState::Online {
        return Err("state_not_online");
    }
    if !c.auto_placement {
        return Err("auto_placement_disabled");
    }
    if c.volume_key.starts_with(UNKNOWN_VOLUME_PREFIX) {
        return Err("volume_identity_unknown");
    }
    if c.filesystem.eq_ignore_ascii_case("FAT32") && request.largest_file_bytes > FAT32_MAX_FILE {
        return Err("fat32_file_too_large");
    }
    if c.usable_bytes < request.required_bytes {
        return Err("insufficient_space");
    }
    Ok(())
}

/// Elige ubicación según la política. Los empates se deciden por
/// `priority` desc e `id` asc, tras los mismos hard gates.
#[must_use]
pub fn place(
    policy: &PlacementPolicy,
    candidates: &[Candidate],
    request: &PlacementRequest,
) -> Placement {
    let mut eligible: Vec<&Candidate> = Vec::new();
    let mut reasons = Vec::new();
    for c in candidates {
        match gate(c, request) {
            Ok(()) => eligible.push(c),
            Err(r) => reasons.push((c.id.clone(), r)),
        }
    }
    let tie =
        |a: &&Candidate, b: &&Candidate| b.priority.cmp(&a.priority).then_with(|| a.id.cmp(&b.id));
    let pick_by_id = |id: &Option<String>| -> Placement {
        match id {
            None => Placement::Ask {
                reason: "no_location_configured",
            },
            Some(id) if eligible.iter().any(|c| &c.id == id) => Placement::Chosen(id.clone()),
            Some(_) => Placement::Ask {
                reason: "configured_location_not_eligible",
            },
        }
    };
    match policy {
        PlacementPolicy::AskAlways => Placement::Ask {
            reason: "ask_always",
        },
        PlacementPolicy::CustomRules => Placement::Ask {
            reason: "custom_rules_not_implemented",
        },
        PlacementPolicy::PreferredLocation(id) => pick_by_id(id),
        PlacementPolicy::ByModelType(map) => {
            match request.model_type.as_ref().and_then(|t| map.get(t)) {
                Some(id) => pick_by_id(id),
                None => Placement::Ask {
                    reason: "model_type_unassigned",
                },
            }
        }
        PlacementPolicy::MostFreeSpace | PlacementPolicy::FastestStorage => {
            if eligible.is_empty() {
                return Placement::NoEligible { reasons };
            }
            let fastest = matches!(policy, PlacementPolicy::FastestStorage);
            eligible.sort_by(|a, b| {
                let primary = if fastest {
                    speed_rank(a.performance_class).cmp(&speed_rank(b.performance_class))
                } else {
                    b.usable_bytes.cmp(&a.usable_bytes)
                };
                primary.then_with(|| tie(a, b))
            });
            Placement::Chosen(eligible[0].id.clone())
        }
    }
}

/// Aplica CUSTOM_RULES después de los mismos hard gates que las demás
/// políticas. Un candidato denegado o que incumple `require` nunca se elige.
///
/// # Errors
/// Rechaza el documento inválido, contradicciones o un id de ubicación ajeno
/// al formato UUIDv7 del contrato.
pub fn place_with_custom_rules(
    rules: &CustomRules,
    context: &RuleContext,
    candidates: &[Candidate],
    request: &PlacementRequest,
) -> Result<Placement, CustomRulesError> {
    rules.validate()?;
    if rules.rules.is_empty() {
        return Ok(Placement::Ask {
            reason: "custom_rules_not_configured",
        });
    }

    let mut eligible = Vec::new();
    let mut reasons = Vec::new();
    let mut any_rule_matched = false;
    for candidate in candidates {
        if let Err(reason) = gate(candidate, request) {
            reasons.push((candidate.id.clone(), reason));
            continue;
        }

        let preview = rules.preview(context, Some(&candidate.id))?;
        any_rule_matched |=
            !preview.matched_rule_ids.is_empty() || !preview.blocking_rule_ids.is_empty();
        match preview.disposition {
            RuleDisposition::Deny => reasons.push((candidate.id.clone(), "custom_rule_denied")),
            RuleDisposition::RequirementNotMet => {
                reasons.push((candidate.id.clone(), "custom_rule_requirement_not_met"));
            }
            _ if preview
                .effective_limit_bytes
                .is_some_and(|limit| request.required_bytes > limit) =>
            {
                reasons.push((candidate.id.clone(), "custom_rule_limit_exceeded"));
            }
            _ => eligible.push(candidate),
        }
    }

    if eligible.is_empty() {
        return Ok(Placement::NoEligible { reasons });
    }
    if !any_rule_matched {
        return Ok(Placement::Ask {
            reason: "custom_rules_no_match",
        });
    }
    eligible.sort_by(|a, b| b.priority.cmp(&a.priority).then_with(|| a.id.cmp(&b.id)));
    Ok(Placement::Chosen(eligible[0].id.clone()))
}

/// Nivel de disco de cada volumen físico (P018, H09): una entrada por
/// `volume_key` aunque tenga varias raíces, con el último espacio observado.
#[must_use]
pub fn levels_by_volume(
    locations: &[StorageLocation],
    thresholds: DiskThresholds,
) -> std::collections::BTreeMap<String, DiskLevel> {
    let mut freshest: std::collections::BTreeMap<&str, &StorageLocation> =
        std::collections::BTreeMap::new();
    for l in locations {
        let newer = freshest
            .get(l.volume_key.as_str())
            .is_none_or(|current| l.last_seen_at > current.last_seen_at);
        if newer {
            freshest.insert(&l.volume_key, l);
        }
    }
    freshest
        .into_iter()
        .filter_map(|(key, l)| {
            disk_level(l.free_bytes.get(), l.capacity_bytes.get(), thresholds)
                .map(|level| (key.to_string(), level))
        })
        .collect()
}

/// Volúmenes cuyo nivel cambió entre dos cálculos: una alerta por volumen y
/// transición, nunca por raíz.
#[must_use]
pub fn level_transitions(
    before: &std::collections::BTreeMap<String, DiskLevel>,
    after: &std::collections::BTreeMap<String, DiskLevel>,
) -> Vec<(String, Option<DiskLevel>, DiskLevel)> {
    after
        .iter()
        .filter(|(k, v)| before.get(*k) != Some(*v))
        .map(|(k, v)| (k.clone(), before.get(k).copied(), *v))
        .collect()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;

    const PCT: DiskThresholds = DiskThresholds::Percent {
        warning_bp: 1500,
        critical_bp: 500,
    };

    #[test]
    fn niveles_de_disco_h09() {
        assert_eq!(disk_level(50, 100, PCT), Some(DiskLevel::Normal));
        assert_eq!(
            disk_level(15, 100, PCT),
            Some(DiskLevel::Normal),
            "igualdad no cruza"
        );
        assert_eq!(disk_level(14, 100, PCT), Some(DiskLevel::Warning));
        assert_eq!(disk_level(5, 100, PCT), Some(DiskLevel::Warning));
        assert_eq!(disk_level(4, 100, PCT), Some(DiskLevel::Critical));
        assert_eq!(disk_level(0, 0, PCT), None, "sin capacidad no hay nivel");
        // Sin desbordamiento con volúmenes enormes.
        assert_eq!(
            disk_level(u64::MAX / 2, u64::MAX, PCT),
            Some(DiskLevel::Normal)
        );
        let bytes = DiskThresholds::Bytes {
            warning: 100,
            critical: 10,
        };
        assert_eq!(disk_level(9, 1000, bytes), Some(DiskLevel::Critical));
        assert_eq!(disk_level(10, 1000, bytes), Some(DiskLevel::Warning));
        assert_eq!(disk_level(100, 1000, bytes), Some(DiskLevel::Normal));
    }

    #[test]
    fn espacio_utilizable_y_presupuesto_compartido() {
        assert_eq!(usable_bytes(100, 30), 70);
        assert_eq!(usable_bytes(10, 30), 0, "nunca negativo");
        assert_eq!(
            volume_budget(100, &[10, 30, 20]),
            70,
            "mayor reserva, no la suma"
        );
        assert_eq!(volume_budget(100, &[]), 100);
    }

    fn cand(id: &str, usable: u64, class: PerformanceClass, priority: u32) -> Candidate {
        Candidate {
            id: id.into(),
            state: LocationState::Online,
            auto_placement: true,
            volume_key: format!("guid-{id}"),
            filesystem: "NTFS".into(),
            performance_class: class,
            priority,
            usable_bytes: usable,
        }
    }

    fn req(bytes: u64) -> PlacementRequest {
        PlacementRequest {
            required_bytes: bytes,
            largest_file_bytes: bytes,
            model_type: Some("LLM".into()),
        }
    }

    #[test]
    fn politicas_de_colocacion() {
        use PerformanceClass::*;
        let cs = vec![
            cand("a", 500, Hdd, 0),
            cand("b", 900, Network, 0),
            cand("c", 300, Nvme, 1),
            cand("d", 300, Nvme, 5),
        ];
        assert_eq!(
            place(&PlacementPolicy::AskAlways, &cs, &req(10)),
            Placement::Ask {
                reason: "ask_always"
            }
        );
        assert_eq!(
            place(&PlacementPolicy::MostFreeSpace, &cs, &req(10)),
            Placement::Chosen("b".into())
        );
        assert_eq!(
            place(&PlacementPolicy::FastestStorage, &cs, &req(10)),
            Placement::Chosen("d".into()),
            "empate NVMe: mayor prioridad"
        );
        assert_eq!(
            place(&PlacementPolicy::FastestStorage, &cs, &req(400)),
            Placement::Chosen("a".into()),
            "NVMe sin espacio"
        );
        assert_eq!(
            place(
                &PlacementPolicy::PreferredLocation(Some("c".into())),
                &cs,
                &req(10)
            ),
            Placement::Chosen("c".into())
        );
        assert_eq!(
            place(
                &PlacementPolicy::PreferredLocation(Some("c".into())),
                &cs,
                &req(400)
            ),
            Placement::Ask {
                reason: "configured_location_not_eligible"
            }
        );
        let map = std::collections::BTreeMap::from([
            ("LLM".to_string(), Some("a".to_string())),
            ("AUDIO".to_string(), None),
        ]);
        assert_eq!(
            place(&PlacementPolicy::ByModelType(map.clone()), &cs, &req(10)),
            Placement::Chosen("a".into())
        );
        let audio = PlacementRequest {
            model_type: Some("AUDIO".into()),
            ..req(10)
        };
        assert_eq!(
            place(&PlacementPolicy::ByModelType(map), &cs, &audio),
            Placement::Ask {
                reason: "no_location_configured"
            }
        );
        assert_eq!(
            place(&PlacementPolicy::CustomRules, &cs, &req(10)),
            Placement::Ask {
                reason: "custom_rules_not_implemented"
            }
        );
        match place(&PlacementPolicy::MostFreeSpace, &cs, &req(10_000)) {
            Placement::NoEligible { reasons } => assert_eq!(reasons.len(), 4),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn custom_rules_filtra_despues_de_los_hard_gates_y_limita_bytes() {
        use crate::custom_rules::{
            CustomRule, CustomRules, RuleContext, RuleEffect, RuleField, RuleOperator,
            RulePredicate,
        };

        let location_a = "018f1f7e-7b2c-7cc0-8b0e-9a0c69f3a101";
        let location_b = "018f1f7e-7b2c-7cc0-8b0e-9a0c69f3a102";
        let context = RuleContext::new(std::collections::BTreeMap::from([(
            RuleField::Format,
            serde_json::json!("gguf"),
        )]));
        let predicate = || RulePredicate {
            field: RuleField::Format,
            operator: RuleOperator::Eq,
            value: serde_json::json!("gguf"),
        };
        let rules = CustomRules {
            schema_version: "1.0".into(),
            rules: vec![
                CustomRule {
                    id: "deny-a".into(),
                    predicates: vec![predicate()],
                    effect: RuleEffect::Deny,
                    limit_bytes: None,
                    target_storage_location_id: Some(location_a.into()),
                },
                CustomRule {
                    id: "require-b".into(),
                    predicates: vec![predicate()],
                    effect: RuleEffect::Require,
                    limit_bytes: None,
                    target_storage_location_id: Some(location_b.into()),
                },
                CustomRule {
                    id: "limit-b".into(),
                    predicates: vec![predicate()],
                    effect: RuleEffect::Limit,
                    limit_bytes: Some("20".into()),
                    target_storage_location_id: Some(location_b.into()),
                },
            ],
        };
        let candidates = [
            cand(location_a, 100, PerformanceClass::Nvme, 9),
            cand(location_b, 100, PerformanceClass::Ssd, 1),
        ];
        assert_eq!(
            place_with_custom_rules(&rules, &context, &candidates, &req(10)).unwrap(),
            Placement::Chosen(location_b.into()),
            "deny y require prevalecen aunque la ubicación denegada tenga más prioridad"
        );
        assert_eq!(
            place_with_custom_rules(&rules, &context, &candidates, &req(21)).unwrap(),
            Placement::NoEligible {
                reasons: vec![
                    (location_a.into(), "custom_rule_denied"),
                    (location_b.into(), "custom_rule_limit_exceeded"),
                ],
            }
        );
    }

    #[test]
    fn custom_rules_pide_ayuda_si_no_hay_regla_aplicable() {
        use crate::custom_rules::{
            CustomRule, CustomRules, RuleContext, RuleEffect, RuleField, RuleOperator,
            RulePredicate,
        };

        let rule = CustomRule {
            id: "deny-safetensors".into(),
            predicates: vec![RulePredicate {
                field: RuleField::Format,
                operator: RuleOperator::Eq,
                value: serde_json::json!("safetensors"),
            }],
            effect: RuleEffect::Deny,
            limit_bytes: None,
            target_storage_location_id: None,
        };
        let rules = CustomRules {
            schema_version: "1.0".into(),
            rules: vec![rule],
        };
        let context = RuleContext::new(std::collections::BTreeMap::from([(
            RuleField::Format,
            serde_json::json!("gguf"),
        )]));
        let candidates = [cand(
            "018f1f7e-7b2c-7cc0-8b0e-9a0c69f3a103",
            100,
            PerformanceClass::Nvme,
            1,
        )];
        assert_eq!(
            place_with_custom_rules(&rules, &context, &candidates, &req(10)).unwrap(),
            Placement::Ask {
                reason: "custom_rules_no_match"
            }
        );
    }

    #[test]
    fn hard_gates() {
        let mut low = cand("low", 900, PerformanceClass::Nvme, 9);
        low.state = LocationState::LowSpace;
        let mut off = cand("off", 900, PerformanceClass::Nvme, 9);
        off.auto_placement = false;
        let mut unknown = cand("unk", 900, PerformanceClass::Nvme, 9);
        unknown.volume_key = format!("{UNKNOWN_VOLUME_PREFIX}D:\\x");
        let mut fat = cand("fat", 1 << 40, PerformanceClass::Removable, 9);
        fat.filesystem = "FAT32".into();
        let ok = cand("ok", 900, PerformanceClass::Hdd, 0);
        let cs = vec![low, off, unknown, fat, ok];
        let big = PlacementRequest {
            required_bytes: 10,
            largest_file_bytes: 5 << 30,
            model_type: None,
        };
        assert_eq!(
            place(&PlacementPolicy::MostFreeSpace, &cs, &big),
            Placement::Chosen("ok".into())
        );
        match place(&PlacementPolicy::MostFreeSpace, &cs[..4], &big) {
            Placement::NoEligible { reasons } => {
                let r: Vec<_> = reasons.iter().map(|(_, r)| *r).collect();
                assert_eq!(
                    r,
                    [
                        "state_not_online",
                        "auto_placement_disabled",
                        "volume_identity_unknown",
                        "fat32_file_too_large"
                    ]
                );
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn registro_con_la_forma_del_schema() {
        let create: StorageLocationCreate = serde_json::from_value(serde_json::json!({
            "name": "Modelos", "root_path": "C:\\IA", "reserved_bytes": "1073741824", "priority": 1,
            "auto_placement": true, "performance_class": "UNKNOWN"
        }))
        .unwrap();
        assert_eq!(create.reserved_bytes.get(), 1 << 30);
        let bad = serde_json::json!({"name": "x", "root_path": "C:\\", "reserved_bytes": "1", "priority": 0,
            "auto_placement": true, "performance_class": "UNKNOWN", "volume_guid": "x"});
        assert!(
            serde_json::from_value::<StorageLocationCreate>(bad).is_err(),
            "el GUID no es editable"
        );
        assert_eq!(
            serde_json::to_value(LocationState::LowSpace).unwrap(),
            "LOW_SPACE"
        );
    }

    #[test]
    fn niveles_por_volumen_sin_duplicar_raices() {
        let mut a = StorageLocation {
            id: "raiz".into(),
            created_at: String::new(),
            updated_at: String::new(),
            row_version: 1,
            metadata: Map::new(),
            name: "raiz".into(),
            root_path: r"C:\m".into(),
            canonical_path: r"C:\m".into(),
            volume_guid: None,
            drive_letter: None,
            unc_path: None,
            filesystem: "NTFS".into(),
            storage_type: "LOCAL".into(),
            capacity_bytes: DecimalU64::default(),
            free_bytes: DecimalU64::default(),
            reserved_bytes: DecimalU64::default(),
            priority: 0,
            auto_placement: true,
            state: LocationState::Online,
            last_seen_at: None,
            volume_key: String::new(),
            performance_class: PerformanceClass::Ssd,
        };
        a.volume_key = "vol-1".into();
        a.capacity_bytes = DecimalU64::new(1000).unwrap();
        a.free_bytes = DecimalU64::new(150).unwrap();
        a.last_seen_at = Some("2026-10-11T10:00:00.000Z".into());
        // Otra raíz del mismo volumen con una observación más reciente.
        let mut b = a.clone();
        b.id = "otra".into();
        b.free_bytes = DecimalU64::new(40).unwrap();
        b.last_seen_at = Some("2026-10-11T10:05:00.000Z".into());
        let mut c = a.clone();
        c.volume_key = "vol-2".into();
        c.free_bytes = DecimalU64::new(150).unwrap();
        let pct = DiskThresholds::Percent {
            warning_bp: 1500,
            critical_bp: 500,
        };
        let levels = levels_by_volume(&[a.clone(), b.clone(), c.clone()], pct);
        assert_eq!(levels.len(), 2, "una entrada por volumen");
        assert_eq!(
            levels["vol-1"],
            DiskLevel::Critical,
            "manda la observación más reciente"
        );
        assert_eq!(
            levels["vol-2"],
            DiskLevel::Normal,
            "15 % exacto: la igualdad no cruza"
        );
        let bytes = DiskThresholds::Bytes {
            warning: 150,
            critical: 40,
        };
        let by_bytes = levels_by_volume(&[c.clone()], bytes);
        assert_eq!(
            by_bytes["vol-2"],
            DiskLevel::Normal,
            "150 libres con aviso en 150: no cruza"
        );
        let before = levels_by_volume(&[a.clone(), c.clone()], pct);
        let t = level_transitions(&before, &levels);
        assert_eq!(
            t,
            vec![(
                "vol-1".to_string(),
                Some(DiskLevel::Normal),
                DiskLevel::Critical
            )]
        );
        assert!(
            level_transitions(&levels, &levels).is_empty(),
            "sin cambio no hay alerta"
        );
    }
}
