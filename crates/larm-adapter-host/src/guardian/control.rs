//! One use per authenticated RPC challenge. Keep all entries until the helper
//! retires; eviction would make captured signed requests valid again.
use super::*;
use std::{collections::BTreeSet, sync::Mutex};

const MAX_CONTROL_REQUESTS: usize = 4096;

#[derive(Default)]
pub(super) struct ControlNonces(Mutex<BTreeSet<uuid::Uuid>>);
impl ControlNonces {
    pub(super) fn claim(&self, nonce: &str) -> Result<(), LarmError> {
        let nonce =
            uuid::Uuid::parse_str(nonce).map_err(|_| error("guardian_control_nonce_invalid"))?;
        if nonce.get_version() != Some(uuid::Version::Random) {
            return Err(error("guardian_control_nonce_invalid"));
        }
        let mut seen = self
            .0
            .lock()
            .map_err(|_| error("guardian_control_state_unavailable"))?;
        if seen.contains(&nonce) {
            return Err(error("guardian_control_replayed"));
        }
        if seen.len() >= MAX_CONTROL_REQUESTS {
            return Err(error("guardian_control_capacity"));
        }
        seen.insert(nonce);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    #[test]
    fn un_solo_consumidor_concurrente_y_alias_no_renuevan_el_reto() {
        let gate = Arc::new(ControlNonces::default());
        let nonce = uuid::Uuid::new_v4().to_string();
        let threads: Vec<_> = (0..16)
            .map(|_| {
                let gate = gate.clone();
                let nonce = nonce.clone();
                std::thread::spawn(move || gate.claim(&nonce).is_ok())
            })
            .collect();
        let accepted = threads
            .into_iter()
            .map(|t| usize::from(t.join().unwrap()))
            .sum::<usize>();
        assert_eq!(accepted, 1);
        assert_eq!(
            gate.claim(&nonce.to_uppercase())
                .unwrap_err()
                .details
                .reason,
            "guardian_control_replayed"
        );
        assert!(gate.claim(&uuid::Uuid::now_v7().to_string()).is_err());
    }
    #[test]
    fn capacidad_finita_no_desaloja_retos_anteriores() {
        let gate = ControlNonces::default();
        let first = uuid::Uuid::new_v4().to_string();
        gate.claim(&first).unwrap();
        for _ in 1..MAX_CONTROL_REQUESTS {
            gate.claim(&uuid::Uuid::new_v4().to_string()).unwrap();
        }
        assert_eq!(
            gate.claim(&uuid::Uuid::new_v4().to_string())
                .unwrap_err()
                .details
                .reason,
            "guardian_control_capacity"
        );
        assert_eq!(
            gate.claim(&first).unwrap_err().details.reason,
            "guardian_control_replayed"
        );
    }
}
