//! Private Core limits for the coexistence of worker output and managed CAS.
//! These are bounds, not claims that an unmeasured output has an exact size.
use crate::{
    estimate::{Confidence, ItemKind, Phase, PhaseItem, Size, StorageEstimate},
    transformations::TransformError,
    units::{DecimalU64, DECIMAL_U64_MAX},
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransformationOutputPolicy {
    pub worker_volume_key: String,
    pub cas_volume_key: String,
    pub max_total_bytes: DecimalU64,
    pub max_file_bytes: DecimalU64,
    pub max_artifacts: u32,
    /// Worker journal/control files retained through Core registration.
    pub worker_control_bytes: DecimalU64,
    /// Includes Core manifests, provenance and promotion receipts. The actual
    /// serialized control data must also fit before any filesystem mutation.
    pub control_bytes: DecimalU64,
}

impl TransformationOutputPolicy {
    pub fn validate(&self) -> Result<(), TransformError> {
        if [&self.worker_volume_key, &self.cas_volume_key]
            .iter()
            .any(|key| key.trim().is_empty() || key.starts_with("unknown:"))
            || self.max_total_bytes.get() == 0
            || self.max_file_bytes.get() == 0
            || self.max_file_bytes > self.max_total_bytes
            || self.max_artifacts == 0
            || self.control_bytes.get() == 0
        {
            return Err(TransformError::InvalidPlan);
        }
        self.required_peaks()?;
        Ok(())
    }

    fn required_peaks(&self) -> Result<BTreeMap<&str, u64>, TransformError> {
        let mut peaks = BTreeMap::new();
        for (key, bytes) in [
            (self.worker_volume_key.as_str(), self.max_total_bytes.get()),
            (
                self.worker_volume_key.as_str(),
                self.worker_control_bytes.get(),
            ),
            (self.cas_volume_key.as_str(), self.max_total_bytes.get()),
            (self.cas_volume_key.as_str(), self.control_bytes.get()),
        ] {
            let slot = peaks.entry(key).or_insert(0_u64);
            *slot = slot
                .checked_add(bytes)
                .filter(|n| *n <= DECIMAL_U64_MAX)
                .ok_or(TransformError::InvalidPlan)?;
        }
        Ok(peaks)
    }

    /// Append this to the worker's conversion phases before estimating the
    /// complete Core operation. No existing CAS deduplication is assumed.
    pub fn registration_phase(&self) -> Result<Phase, TransformError> {
        self.validate()?;
        let item = |name: &str, kind, volume: &str, bound| PhaseItem {
            name: name.into(),
            kind,
            volume_key: volume.into(),
            size: Size::estimated(0, bound, Confidence::Unknown),
            existing: false,
        };
        Ok(Phase {
            name: "transformation_register_managed_output".into(),
            items: vec![
                item(
                    "transformation_worker_output",
                    ItemKind::Temporary,
                    &self.worker_volume_key,
                    self.max_total_bytes.get(),
                ),
                item(
                    "transformation_cas_output",
                    ItemKind::FinalArtifact,
                    &self.cas_volume_key,
                    self.max_total_bytes.get(),
                ),
                item(
                    "transformation_control_data",
                    ItemKind::Temporary,
                    &self.cas_volume_key,
                    self.control_bytes.get(),
                ),
                item(
                    "transformation_worker_control",
                    ItemKind::Temporary,
                    &self.worker_volume_key,
                    self.worker_control_bytes.get(),
                ),
            ],
        })
    }

    /// A seal cannot authorize publication with only the worker's private
    /// estimate. Every involved volume must cover the coexistence phase.
    pub fn validate_estimate(&self, estimate: &StorageEstimate) -> Result<(), TransformError> {
        self.validate()?;
        let mut volumes = BTreeMap::new();
        for volume in &estimate.per_volume {
            if volumes
                .insert(volume.volume_key.as_str(), volume.peak_extra_bytes.get())
                .is_some()
            {
                return Err(TransformError::InvalidPlan);
            }
        }
        for (key, required) in self.required_peaks()? {
            if volumes.get(key).is_none_or(|peak| *peak < required) {
                return Err(TransformError::InvalidPlan);
            }
        }
        let total = volumes
            .values()
            .try_fold(0_u64, |sum, n| sum.checked_add(*n))
            .filter(|n| *n <= DECIMAL_U64_MAX)
            .ok_or(TransformError::InvalidPlan)?;
        if estimate.peak_extra_bytes.is_none_or(|n| n.get() < total)
            || estimate.upper_bound_bytes.is_none_or(|n| n.get() < total)
        {
            return Err(TransformError::InvalidPlan);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::estimate::{estimate, MarginPolicy, VolumeSnapshot};

    fn d(n: u64) -> DecimalU64 {
        DecimalU64::new(n).unwrap()
    }
    fn policy(target: &str) -> TransformationOutputPolicy {
        TransformationOutputPolicy {
            worker_volume_key: "worker".into(),
            cas_volume_key: target.into(),
            max_total_bytes: d(1_000),
            max_file_bytes: d(700),
            max_artifacts: 3,
            worker_control_bytes: d(200),
            control_bytes: d(100),
        }
    }
    fn estimate_policy(p: &TransformationOutputPolicy) -> StorageEstimate {
        estimate(
            &[p.registration_phase().unwrap()],
            &["worker", "cas"].map(|key| VolumeSnapshot {
                volume_key: key.into(),
                os_free: 100_000,
                policy_minimum: 0,
                other_reservations: 0,
            }),
            MarginPolicy {
                minimum_margin_bytes: 0,
                margin_basis_points: 500,
            },
            None,
        )
        .unwrap()
    }

    #[test]
    fn same_volume_counts_both_copies_without_claiming_unknown_sizes_are_exact() {
        let p = policy("worker");
        let e = estimate_policy(&p);
        assert_eq!(e.peak_extra_bytes, Some(d(2_300)));
        assert_eq!(e.required_free_bytes, Some(d(2_415)));
        assert_eq!(e.final_artifact_bytes, Some(d(1_000)));
        assert_eq!(e.confidence, Confidence::Unknown);
        assert_eq!(e.per_volume.len(), 1);
        assert!(e.per_volume[0].eligible);
        p.validate_estimate(&e).unwrap();
    }

    #[test]
    fn distinct_volumes_and_worker_temporary_peak_are_calculated_phase_by_phase() {
        let p = policy("cas");
        let mut phases = vec![Phase {
            name: "conversion".into(),
            items: vec![PhaseItem {
                name: "worker_scratch".into(),
                kind: ItemKind::Temporary,
                volume_key: "worker".into(),
                size: Size::exact(8_000),
                existing: false,
            }],
        }];
        phases.push(p.registration_phase().unwrap());
        let e = estimate(
            &phases,
            &["worker", "cas"].map(|key| VolumeSnapshot {
                volume_key: key.into(),
                os_free: 100_000,
                policy_minimum: 0,
                other_reservations: 0,
            }),
            MarginPolicy {
                minimum_margin_bytes: 0,
                margin_basis_points: 0,
            },
            None,
        )
        .unwrap();
        assert_eq!(e.peak_extra_bytes, Some(d(9_100)));
        assert_eq!(
            e.per_volume
                .iter()
                .find(|v| v.volume_key == "worker")
                .unwrap()
                .peak_extra_bytes,
            d(8_000)
        );
        assert_eq!(
            e.per_volume
                .iter()
                .find(|v| v.volume_key == "cas")
                .unwrap()
                .peak_extra_bytes,
            d(1_100)
        );
        p.validate_estimate(&e).unwrap();
    }

    #[test]
    fn invalid_or_overflowing_limits_and_unresolved_volumes_are_rejected() {
        for failure in [
            "total",
            "file",
            "file_above_total",
            "count",
            "control",
            "worker",
            "cas",
            "overflow",
        ] {
            let mut p = policy("cas");
            match failure {
                "total" => p.max_total_bytes = d(0),
                "file" => p.max_file_bytes = d(0),
                "file_above_total" => p.max_file_bytes = d(1_001),
                "count" => p.max_artifacts = 0,
                "control" => p.control_bytes = d(0),
                "worker" => p.worker_volume_key.clear(),
                "cas" => p.cas_volume_key = "unknown:drive".into(),
                "overflow" => {
                    p.max_total_bytes = d(DECIMAL_U64_MAX);
                }
                _ => unreachable!(),
            }
            assert!(p.validate().is_err(), "{failure}");
        }
    }

    #[test]
    fn private_worker_budget_missing_volume_duplicate_or_unknown_global_peak_cannot_publish() {
        let p = policy("worker");
        for failure in ["private", "missing", "duplicate", "global", "upper"] {
            let mut e = estimate_policy(&p);
            match failure {
                "private" => e.per_volume[0].peak_extra_bytes = d(1_100),
                "missing" => e.per_volume.clear(),
                "duplicate" => e.per_volume.push(e.per_volume[0].clone()),
                "global" => e.peak_extra_bytes = None,
                "upper" => e.upper_bound_bytes = Some(d(2_099)),
                _ => unreachable!(),
            }
            assert!(p.validate_estimate(&e).is_err(), "{failure}");
        }
    }
}
