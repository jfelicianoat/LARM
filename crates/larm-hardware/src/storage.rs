//! Storage Profiler (P012; spec §22, §28 y §115): volúmenes locales con su
//! Volume GUID y puntos de montaje, unidades de red y raíces UNC.
//!
//! La enumeración corre en el proceso hijo `larm-daemon probe-storage`
//! (ADR-0010). Aquí se valida su informe y se traduce a [`StorageVolume`]:
//! la identidad sale del GUID (local) o de `\\servidor\recurso` (UNC), nunca
//! de la letra; la clase sale del bus y de la penalización de búsqueda
//! observados, o queda UNKNOWN.

use async_trait::async_trait;
use larm_domain::error::{ErrorCode, LarmError};
use larm_domain::ports::StorageProber;
use larm_domain::storage::{
    classify, parse_unc, volume_guid_from_path, BusKind, DriveKind, PerformanceClass,
    StorageObservation, StorageVolume, VolumeIdentity,
};
use larm_domain::units::DecimalU64;
use serde_json::{json, Value};

use crate::devices::NativeCommand;
use crate::native_report::{NativeShare, NativeVolume, StorageReport, STORAGE_REPORT_VERSION};

/// `GetDriveTypeW` → [`DriveKind`].
#[must_use]
pub fn drive_kind(raw: u32) -> DriveKind {
    match raw {
        2 => DriveKind::Removable,
        3 => DriveKind::Fixed,
        4 => DriveKind::Remote,
        5 => DriveKind::Cdrom,
        6 => DriveKind::Ramdisk,
        _ => DriveKind::Unknown,
    }
}

/// Primera letra de unidad entre los puntos de montaje (`C:`), solo para
/// presentación.
fn first_letter(mounts: &[String]) -> Option<String> {
    mounts.iter().find_map(|m| {
        let b = m.as_bytes();
        (b.len() == 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && b[2] == b'\\')
            .then(|| m[..2].to_ascii_uppercase())
    })
}

fn local(v: &NativeVolume, unknown: &mut Vec<Value>) -> StorageVolume {
    let guid = volume_guid_from_path(&v.volume_path);
    if guid.is_none() {
        unknown.push(json!({"field": format!("volumes[{}].volume_guid", v.volume_path), "reason": "volume_guid_unparseable", "detail": v.volume_path}));
    }
    let drive = drive_kind(v.drive_type);
    let bus = v.bus_type.map(BusKind::from_raw);
    // Medio extraíble declarado por el propio dispositivo cuenta como evidencia.
    let drive_for_class = if v.removable_media == Some(true) {
        DriveKind::Removable
    } else {
        drive
    };
    let class = classify(drive_for_class, bus, v.seek_penalty);
    let mut evidence = vec![format!("native:FindFirstVolumeW:{}", v.volume_path)];
    if let Some(raw) = v.bus_type {
        evidence.push(format!(
            "ioctl:STORAGE_DEVICE_DESCRIPTOR bus={raw} product={}",
            v.product.clone().unwrap_or_default()
        ));
    }
    if let Some(seek) = v.seek_penalty {
        evidence.push(format!(
            "ioctl:DEVICE_SEEK_PENALTY_DESCRIPTOR incurs={seek}"
        ));
    }
    evidence.extend(v.errors.iter().map(|e| format!("error:{e}")));
    if class == PerformanceClass::Unknown && drive == DriveKind::Fixed {
        unknown.push(json!({
            "field": format!("volumes[{}].performance_class", guid.clone().unwrap_or_else(|| v.volume_path.clone())),
            "reason": "storage_class_unproven",
            "detail": format!("bus={:?} seek_penalty={:?}", v.bus_type, v.seek_penalty),
        }));
    }
    StorageVolume {
        identity: VolumeIdentity {
            volume_key: guid.clone(),
            volume_guid: guid,
            unc_path: None,
            drive_letter: first_letter(&v.mount_points),
            mount_points: v.mount_points.clone(),
        },
        label: v.label.clone(),
        filesystem: v.filesystem.clone(),
        drive_kind: drive,
        bus,
        seek_penalty: v.seek_penalty,
        performance_class: class,
        capacity_bytes: v.capacity_bytes.and_then(DecimalU64::new),
        free_bytes: v.free_bytes.and_then(DecimalU64::new),
        read_only: v.read_only,
        online: v.capacity_bytes.is_some(),
        evidence_refs: evidence,
    }
}

fn network(s: &NativeShare, unknown: &mut Vec<Value>) -> StorageVolume {
    let root = parse_unc(&s.unc_path);
    if let Err(e) = &root {
        unknown.push(json!({"field": format!("shares[{}].volume_key", s.unc_path), "reason": "unc_unparseable", "detail": e}));
    }
    let mut evidence = vec![format!("native:GetDiskFreeSpaceExW:{}", s.unc_path)];
    evidence.extend(s.errors.iter().map(|e| format!("error:{e}")));
    StorageVolume {
        identity: VolumeIdentity {
            // Sin GUID inventado: la identidad UNC es servidor+recurso.
            volume_key: root
                .as_ref()
                .ok()
                .map(larm_domain::storage::UncRoot::volume_key),
            volume_guid: None,
            unc_path: root.as_ref().ok().map(larm_domain::storage::UncRoot::root),
            mount_points: s.drive_letter.iter().map(|l| format!("{l}\\")).collect(),
            drive_letter: s.drive_letter.clone(),
        },
        label: None,
        filesystem: s.filesystem.clone(),
        drive_kind: DriveKind::Remote,
        bus: None,
        seek_penalty: None,
        performance_class: PerformanceClass::Network,
        capacity_bytes: s.capacity_bytes.and_then(DecimalU64::new),
        free_bytes: s.free_bytes.and_then(DecimalU64::new),
        read_only: s.read_only,
        online: s.available,
        evidence_refs: evidence,
    }
}

/// Traduce el informe nativo. Una unidad mapeada y su UNC pedido aparte son
/// el mismo volumen: se conserva una entrada por `volume_key`.
#[must_use]
pub fn map_report(report: &StorageReport, observed_at: &str) -> StorageObservation {
    let mut unknown = Vec::new();
    if report.volumes_probe.status != crate::native_report::ProbeStatus::Verified {
        unknown.push(json!({"field": "volumes", "reason": "volumes_probe_failed", "detail": report.volumes_probe.detail}));
    }
    let mut volumes: Vec<StorageVolume> = report
        .volumes
        .iter()
        .map(|v| local(v, &mut unknown))
        .collect();
    for s in &report.shares {
        let v = network(s, &mut unknown);
        let duplicate = v.identity.volume_key.is_some()
            && volumes
                .iter()
                .any(|o| o.identity.volume_key == v.identity.volume_key);
        if !duplicate {
            volumes.push(v);
        }
    }
    volumes.sort_by(|a, b| a.identity.volume_key.cmp(&b.identity.volume_key));
    StorageObservation {
        volumes,
        unknown,
        observed_at: observed_at.to_string(),
    }
}

/// Prober que lanza el hijo `probe-storage`.
pub struct NativeStorageProber {
    pub command: NativeCommand,
}

#[async_trait]
impl StorageProber for NativeStorageProber {
    async fn probe(&self, unc_roots: &[String]) -> Result<StorageObservation, LarmError> {
        let observed_at = larm_domain::bridge::timestamp(chrono::Utc::now());
        let mut args = self.command.args.clone();
        for root in unc_roots {
            args.push("--unc".into());
            args.push(root.clone());
        }
        let fail = |reason: &str, detail: String| {
            LarmError::new(
                ErrorCode::Internal,
                "El sondeo de almacenamiento falló",
                reason,
            )
            .retryable(true)
            .with_technical(detail)
        };
        let out = crate::process::run(&self.command.program, &args, self.command.timeout)
            .await
            .map_err(|f| fail(&format!("storage_probe_{}", f.kind), f.detail))?;
        let report: StorageReport = serde_json::from_str(out.trim())
            .map_err(|e| fail("storage_probe_unparseable", e.to_string()))?;
        if report.report_version != STORAGE_REPORT_VERSION {
            return Err(fail(
                "storage_probe_version",
                format!(
                    "informe v{} y se espera v{STORAGE_REPORT_VERSION}",
                    report.report_version
                ),
            ));
        }
        Ok(map_report(&report, &observed_at))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::native_report::{ApiProbe, ProbeStatus};

    const PCIA: &str = include_str!("../../../tests/fixtures/hardware/storage-pcia.json");

    fn vol(
        path: &str,
        mounts: &[&str],
        drive: u32,
        bus: Option<u32>,
        seek: Option<bool>,
    ) -> NativeVolume {
        NativeVolume {
            volume_path: path.into(),
            mount_points: mounts.iter().map(|m| (*m).to_string()).collect(),
            label: None,
            filesystem: Some("NTFS".into()),
            read_only: false,
            drive_type: drive,
            capacity_bytes: Some(1000),
            free_bytes: Some(400),
            bus_type: bus,
            seek_penalty: seek,
            removable_media: Some(false),
            product: None,
            errors: vec![],
        }
    }

    fn share(path: &str, letter: Option<&str>, available: bool) -> NativeShare {
        NativeShare {
            unc_path: path.into(),
            drive_letter: letter.map(String::from),
            available,
            filesystem: available.then(|| "NTFS".into()),
            read_only: false,
            capacity_bytes: available.then_some(5000),
            free_bytes: available.then_some(100),
            errors: vec![],
        }
    }

    fn report(volumes: Vec<NativeVolume>, shares: Vec<NativeShare>) -> StorageReport {
        StorageReport {
            report_version: STORAGE_REPORT_VERSION,
            volumes_probe: ApiProbe::new(ProbeStatus::Verified, "ok"),
            volumes,
            shares,
        }
    }

    #[test]
    fn pc_ia_real_tres_volumenes_nvme_y_un_recurso_unc() {
        let obs = map_report(&serde_json::from_str(PCIA).unwrap(), "t");
        let c = obs.by_key("fb4f4fbc-5441-4427-a73a-0e1ea94ad8d3").unwrap();
        assert_eq!(
            (c.identity.drive_letter.as_deref(), c.performance_class),
            (Some("C:"), PerformanceClass::Nvme)
        );
        assert_eq!(c.identity.mount_points, [r"C:\"]);
        let locals: Vec<_> = obs
            .volumes
            .iter()
            .filter(|v| v.identity.volume_guid.is_some())
            .collect();
        assert_eq!(locals.len(), 3);
        assert_eq!(
            locals
                .iter()
                .filter(|v| v.identity.drive_letter.is_none())
                .count(),
            2,
            "Recovery y EFI sin letra"
        );
        let unc = obs.by_key(r"\\localhost\temp").unwrap();
        assert!(unc.online && unc.identity.volume_guid.is_none());
        assert_eq!(unc.performance_class, PerformanceClass::Network);
        let down = obs.by_key(r"\\noexiste-larm\x").unwrap();
        assert!(!down.online && down.capacity_bytes.is_none());
        assert!(obs.unknown.is_empty(), "{:?}", obs.unknown);
    }

    #[test]
    fn varios_montajes_letra_cambiada_y_sin_letra() {
        let g = r"\\?\Volume{11111111-2222-3333-4444-555555555555}\";
        // Mismo volumen con letra y montado en carpeta; luego con otra letra.
        let a = map_report(
            &report(
                vec![vol(
                    g,
                    &[r"E:\", r"C:\Montajes\Datos\"],
                    3,
                    Some(0x0B),
                    Some(true),
                )],
                vec![],
            ),
            "t",
        );
        let b = map_report(
            &report(vec![vol(g, &[r"F:\"], 3, Some(0x0B), Some(true))], vec![]),
            "t",
        );
        let (va, vb) = (&a.volumes[0], &b.volumes[0]);
        assert_eq!(
            va.identity.volume_key, vb.identity.volume_key,
            "la letra no es identidad"
        );
        assert_eq!(
            (
                va.identity.drive_letter.as_deref(),
                vb.identity.drive_letter.as_deref()
            ),
            (Some("E:"), Some("F:"))
        );
        assert_eq!(va.identity.mount_points.len(), 2);
        assert_eq!(va.performance_class, PerformanceClass::Hdd);
        let sin_letra = map_report(
            &report(
                vec![vol(g, &[r"C:\Montajes\Datos\"], 3, Some(0x11), None)],
                vec![],
            ),
            "t",
        );
        assert_eq!(sin_letra.volumes[0].identity.drive_letter, None);
    }

    #[test]
    fn sin_evidencia_la_clase_es_unknown_y_se_declara() {
        let g = r"\\?\Volume{11111111-2222-3333-4444-555555555555}\";
        let obs = map_report(&report(vec![vol(g, &[r"D:\"], 3, None, None)], vec![]), "t");
        assert_eq!(obs.volumes[0].performance_class, PerformanceClass::Unknown);
        assert_eq!(obs.unknown[0]["reason"], "storage_class_unproven");
        let usb = map_report(
            &report(vec![vol(g, &[r"D:\"], 2, Some(0x07), None)], vec![]),
            "t",
        );
        assert_eq!(
            usb.volumes[0].performance_class,
            PerformanceClass::Removable
        );
    }

    #[test]
    fn guid_ilegible_no_inventa_identidad() {
        let obs = map_report(
            &report(
                vec![vol(
                    r"\\?\Volume{roto}\",
                    &[r"G:\"],
                    3,
                    Some(0x11),
                    Some(false),
                )],
                vec![],
            ),
            "t",
        );
        assert_eq!(obs.volumes[0].identity.volume_key, None);
        assert_eq!(obs.unknown[0]["reason"], "volume_guid_unparseable");
    }

    #[test]
    fn unidad_mapeada_y_unc_pedido_son_un_volumen() {
        let obs = map_report(
            &report(
                vec![],
                vec![
                    share(r"\\NAS\Modelos", Some("Z:"), true),
                    share(r"\\nas\modelos", None, true),
                    share("no-unc", None, false),
                ],
            ),
            "t",
        );
        let keys: Vec<_> = obs
            .volumes
            .iter()
            .map(|v| v.identity.volume_key.clone())
            .collect();
        assert_eq!(keys, [None, Some(r"\\nas\modelos".to_string())]);
        assert_eq!(
            obs.by_key(r"\\nas\modelos")
                .unwrap()
                .identity
                .drive_letter
                .as_deref(),
            Some("Z:")
        );
        assert!(obs.unknown.iter().any(|u| u["reason"] == "unc_unparseable"));
    }

    #[test]
    fn enumeracion_fallida_se_declara() {
        let mut r = report(vec![], vec![]);
        r.volumes_probe = ApiProbe::new(ProbeStatus::Failed, "FindFirstVolumeW 0x80070005");
        let obs = map_report(&r, "t");
        assert!(obs.volumes.is_empty());
        assert_eq!(obs.unknown[0]["reason"], "volumes_probe_failed");
    }

    #[tokio::test]
    async fn hijo_inexistente_o_de_otra_version() {
        let missing = NativeStorageProber {
            command: NativeCommand {
                program: "larm-no-existe.exe".into(),
                args: vec![],
                timeout: std::time::Duration::from_secs(5),
            },
        };
        let e = missing.probe(&[]).await.unwrap_err();
        assert_eq!(e.details.reason, "storage_probe_unavailable");
        if cfg!(windows) {
            let mut r: Value = serde_json::from_str(PCIA).unwrap();
            r["report_version"] = 9.into();
            let dir = tempfile::tempdir().unwrap();
            let file = dir.path().join("r.json");
            std::fs::write(&file, r.to_string()).unwrap();
            let other = NativeStorageProber {
                command: NativeCommand {
                    program: "powershell".into(),
                    args: vec![
                        "-NoProfile".into(),
                        "-Command".into(),
                        format!("Get-Content -Raw '{}'", file.display()),
                    ],
                    timeout: std::time::Duration::from_secs(20),
                },
            };
            assert_eq!(
                other.probe(&[]).await.unwrap_err().details.reason,
                "storage_probe_version"
            );
        }
    }
}
