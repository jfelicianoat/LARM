//! Memoria del sistema.
//!
//! - `physical_bytes`: RAM instalada en módulos (CIM `Win32_PhysicalMemory`).
//! - `usable_bytes`: RAM que ve Windows (`GlobalMemoryStatusEx.ullTotalPhys`).
//!   En una APU con carve-out de VRAM es menor que la instalada: esa
//!   diferencia pertenece a la GPU y no se suma como RAM.
//! - pagefile: `CommitLimit − PhysicalTotal` de `GetPerformanceInfo`.

use larm_domain::hardware::{MemoryInfo, SYSTEM_RAM_POOL};
use larm_domain::units::DecimalU64;

use crate::cim::CimFailure;

/// Lecturas crudas.
#[derive(Debug, Clone)]
pub struct RawMemory {
    pub total_phys: u64,
    pub avail_phys: u64,
    pub pagefile_total: u64,
    pub installed: Result<u64, CimFailure>,
}

/// Memoria mapeada más lo que no entra en el schema cerrado.
#[derive(Debug, Clone, PartialEq)]
pub struct MappedMemory {
    pub ram: MemoryInfo,
    pub available: Option<DecimalU64>,
    pub pagefile_total: Option<DecimalU64>,
    /// Motivo por el que `physical_bytes` es desconocido.
    pub physical_unknown: Option<CimFailure>,
}

/// Convierte las lecturas. Una RAM instalada menor que la visible es una
/// lectura CIM incoherente y queda desconocida; cero visible es desconocido.
#[must_use]
pub fn map(raw: &RawMemory) -> MappedMemory {
    let usable = DecimalU64::new(raw.total_phys).filter(|b| b.get() > 0);
    let (physical, physical_unknown) = match &raw.installed {
        Ok(installed) if *installed < raw.total_phys => (
            None,
            Some(CimFailure {
                reason: "wmi_inconsistent",
                detail: format!(
                    "RAM instalada {installed} menor que la visible {}",
                    raw.total_phys
                ),
            }),
        ),
        Ok(installed) => match DecimalU64::new(*installed) {
            Some(b) => (Some(b), None),
            None => (
                None,
                Some(CimFailure {
                    reason: "wmi_unparseable",
                    detail: format!("{installed} fuera de rango"),
                }),
            ),
        },
        Err(failure) => (None, Some(failure.clone())),
    };
    let available = usable
        .and(DecimalU64::new(raw.avail_phys))
        .filter(|a| usable.is_some_and(|u| *a <= u));
    MappedMemory {
        ram: MemoryInfo {
            physical_bytes: physical,
            usable_bytes: usable,
            pool_id: SYSTEM_RAM_POOL.into(),
        },
        available,
        pagefile_total: DecimalU64::new(raw.pagefile_total),
        physical_unknown,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    const GIB: u64 = 1 << 30;

    fn raw(installed: Result<u64, CimFailure>) -> RawMemory {
        RawMemory {
            total_phys: 64 * GIB - 300 * (1 << 20),
            avail_phys: 40 * GIB,
            pagefile_total: 8 * GIB,
            installed,
        }
    }

    #[test]
    fn carve_out_no_se_suma_como_ram() {
        let m = map(&raw(Ok(128 * GIB)));
        assert_eq!(m.ram.physical_bytes.unwrap().get(), 128 * GIB);
        assert_eq!(
            m.ram.usable_bytes.unwrap().get(),
            64 * GIB - 300 * (1 << 20)
        );
        assert_eq!(m.available.unwrap().get(), 40 * GIB);
        assert_eq!(m.pagefile_total.unwrap().get(), 8 * GIB);
        assert_eq!(m.ram.pool_id, SYSTEM_RAM_POOL);
        assert!(m.physical_unknown.is_none());
    }

    #[test]
    fn wmi_fallido_deja_la_instalada_desconocida_y_el_resto_intacto() {
        let failure = CimFailure {
            reason: "wmi_timeout",
            detail: "x".into(),
        };
        let m = map(&raw(Err(failure.clone())));
        assert_eq!(m.ram.physical_bytes, None);
        assert_eq!(m.physical_unknown, Some(failure));
        assert!(m.ram.usable_bytes.is_some() && m.available.is_some());
    }

    #[test]
    fn instalada_menor_que_visible_es_incoherente() {
        let m = map(&raw(Ok(32 * GIB)));
        assert_eq!(m.ram.physical_bytes, None);
        assert_eq!(m.physical_unknown.unwrap().reason, "wmi_inconsistent");
    }

    #[test]
    fn disponible_imposible_o_total_cero_son_desconocidos() {
        let mut r = raw(Ok(128 * GIB));
        r.avail_phys = 100 * GIB;
        assert_eq!(map(&r).available, None);
        r.total_phys = 0;
        let m = map(&r);
        assert_eq!((m.ram.usable_bytes, m.available), (None, None));
    }
}
