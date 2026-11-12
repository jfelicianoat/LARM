//! Bloqueo por usuario, adquirido antes de crear el WebView y liberado por el SO.
use std::fs::{File, OpenOptions, TryLockError};
use std::path::Path;

pub enum Instance {
    Primary(File),
    AlreadyRunning,
}

pub fn acquire(path: &Path) -> Result<Instance, String> {
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)
        .map_err(|e| format!("No se pudo comprobar si LARM está abierto: {e}"))?;
    match file.try_lock() {
        Ok(()) => Ok(Instance::Primary(file)),
        Err(TryLockError::WouldBlock) => Ok(Instance::AlreadyRunning),
        Err(TryLockError::Error(e)) => Err(format!("No se pudo reservar la sesión de LARM: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn una_sesion_y_recuperacion_al_cerrar() {
        let path =
            std::env::temp_dir().join(format!("larm-instance-test-{}.lock", std::process::id()));
        let first = match acquire(&path).unwrap() {
            Instance::Primary(file) => file,
            Instance::AlreadyRunning => panic!("la primera sesión debe adquirir el bloqueo"),
        };
        assert!(matches!(acquire(&path).unwrap(), Instance::AlreadyRunning));
        drop(first);
        let recovered = acquire(&path).unwrap();
        assert!(matches!(recovered, Instance::Primary(_)));
        drop(recovered);
        std::fs::remove_file(path).unwrap();
    }
}
