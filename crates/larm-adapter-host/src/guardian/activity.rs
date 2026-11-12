//! One atomic admission/quiescence barrier; retiring cannot race a new check.
use super::{error, LarmError};
use std::sync::atomic::{AtomicU32, Ordering};
const STOPPING: u32 = 1 << 31;
const RETIRING: u32 = 1 << 30;
const FLAGS: u32 = STOPPING | RETIRING;
#[derive(Default)]
pub(super) struct Activity(AtomicU32);
impl Activity {
    pub fn stopping(&self) -> bool {
        self.0.load(Ordering::SeqCst) & STOPPING != 0
    }
    pub fn active(&self) -> u32 {
        self.0.load(Ordering::SeqCst) & !FLAGS
    }
    pub fn enter(&self, read_only: bool) -> Result<(), LarmError> {
        let mut previous = self.0.load(Ordering::SeqCst);
        loop {
            if previous & RETIRING != 0 || (previous & STOPPING != 0 && !read_only) {
                return Err(error("guardian_rpc_closed"));
            }
            if previous & !FLAGS >= 16 {
                return Err(error("guardian_rpc_busy"));
            }
            match self.0.compare_exchange_weak(
                previous,
                previous + 1,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => return Ok(()),
                Err(current) => previous = current,
            }
        }
    }
    pub fn leave(&self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
    pub fn stop(&self) -> bool {
        self.0.fetch_or(STOPPING, Ordering::SeqCst) & STOPPING == 0
    }
    pub fn retire(&self) -> bool {
        match self.0.compare_exchange(
            STOPPING,
            STOPPING | RETIRING,
            Ordering::SeqCst,
            Ordering::SeqCst,
        ) {
            Ok(_) => true,
            Err(current) => current == STOPPING | RETIRING,
        }
    }
}
#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    #[test]
    fn stop_bloquea_ejecucion_y_retira_solo_tras_drenar_comprobacion() {
        let a = Activity::default();
        a.enter(false).unwrap();
        assert!(a.stop());
        assert!(!a.stop());
        assert!(a.enter(false).is_err());
        a.enter(true).unwrap();
        assert!(!a.retire());
        a.leave();
        assert!(!a.retire());
        a.leave();
        assert_eq!(a.active(), 0);
        assert!(a.retire());
        assert!(a.retire());
        assert!(a.enter(true).is_err());
        assert!(a.enter(false).is_err());
    }
    #[test]
    fn capacidad_acotada_conserva_flags_y_no_se_confunde_con_cierre() {
        let a = Activity::default();
        for _ in 0..16 {
            a.enter(false).unwrap();
        }
        assert_eq!(
            a.enter(true).unwrap_err().details.reason,
            "guardian_rpc_busy"
        );
        assert_eq!(a.active(), 16);
        assert!(!a.retire());
        a.stop();
        for _ in 0..16 {
            a.leave();
        }
        assert!(a.stopping());
        assert_eq!(a.active(), 0);
        assert!(a.retire());
    }
    #[test]
    fn recheck_y_retire_concurrentes_no_admiten_trabajo_tras_confirmar_retirada() {
        use std::sync::{Arc, Barrier};
        for _ in 0..100 {
            let a = Arc::new(Activity::default());
            a.stop();
            let b = Arc::new(Barrier::new(2));
            let other = a.clone();
            let gate = b.clone();
            let checking = std::thread::spawn(move || {
                gate.wait();
                other.enter(true).is_ok()
            });
            b.wait();
            let retired = a.retire();
            let entered = checking.join().unwrap();
            assert_ne!(retired, entered);
            if entered {
                assert_eq!(a.active(), 1);
                a.leave();
                assert!(a.retire());
            }
            assert!(a.enter(true).is_err());
        }
    }
}
