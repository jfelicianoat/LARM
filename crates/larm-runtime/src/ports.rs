//! Reserva de puertos locales para procesos propios (P033, contrato 04).
//!
//! Cada reserva realiza un `bind` real sobre la dirección configurada y
//! mantiene exclusión entre instancias del supervisor mientras vive el guard.
//! El listener de comprobación se cierra antes de entregar el puerto al
//! runtime, por lo que el arranque aún debe confirmar que el proceso logró
//! escuchar en él y tratar un conflicto como fallo de arranque.

use larm_domain::error::{ErrorCode, LarmError};
use parking_lot::Mutex;
use std::{
    collections::HashSet,
    net::{IpAddr, SocketAddr, TcpListener},
    sync::Arc,
};

#[derive(Debug)]
struct PortPool {
    min_port: u16,
    max_port: u16,
    bind_address: IpAddr,
    reserved: Mutex<HashSet<u16>>,
}

/// Asigna puertos libres dentro de un rango sin duplicarlos entre callers.
#[derive(Clone, Debug)]
pub struct PortAllocator {
    pool: Arc<PortPool>,
}

impl PortAllocator {
    /// Crea un asignador para el rango inclusivo y la dirección que usará el
    /// runtime al publicar su endpoint.
    pub fn new(min_port: u16, max_port: u16, bind_address: IpAddr) -> Result<Self, LarmError> {
        if min_port == 0 || min_port > max_port {
            return Err(LarmError::new(
                ErrorCode::ValidationFailed,
                "Rango de puertos no válido",
                "runtime_port_range_invalid",
            ));
        }
        Ok(Self {
            pool: Arc::new(PortPool {
                min_port,
                max_port,
                bind_address,
                reserved: Mutex::new(HashSet::new()),
            }),
        })
    }

    /// Reserva el primer puerto disponible y comprobable con bind real.
    pub fn reserve(&self) -> Result<PortReservation, LarmError> {
        for port in self.pool.min_port..=self.pool.max_port {
            if let Ok(reservation) = self.reserve_specific(port) {
                return Ok(reservation);
            }
        }
        Err(LarmError::new(
            ErrorCode::ResourceUnavailable,
            "No hay puertos libres para iniciar el runtime",
            "runtime_port_range_exhausted",
        )
        .retryable(true))
    }

    /// Reserva el puerto declarado por un `LaunchPlan`, comprobándolo en la
    /// dirección real antes de permitir que el coordinador inicie el worker.
    pub fn reserve_specific(&self, port: u16) -> Result<PortReservation, LarmError> {
        if port < self.pool.min_port || port > self.pool.max_port {
            return Err(LarmError::new(
                ErrorCode::ValidationFailed,
                "El puerto del plan queda fuera del rango configurado",
                "runtime_port_out_of_range",
            ));
        }
        let mut reserved = self.pool.reserved.lock();
        let address = SocketAddr::new(self.pool.bind_address, port);
        if reserved.contains(&port) || TcpListener::bind(address).is_err() {
            return Err(LarmError::new(
                ErrorCode::ResourceUnavailable,
                "El puerto solicitado no está disponible",
                "runtime_port_unavailable",
            )
            .retryable(true));
        }
        // La reserva lógica se conserva hasta que se cierre la sesión dueña
        // del runtime. El proceso debe confirmar su propio bind durante health.
        reserved.insert(port);
        Ok(PortReservation {
            port,
            pool: Arc::clone(&self.pool),
        })
    }
}

/// Guard RAII: mientras viva, otro arranque administrado no recibe el mismo
/// puerto. Debe conservarse durante el lifecycle de la instancia.
#[derive(Debug)]
pub struct PortReservation {
    port: u16,
    pool: Arc<PortPool>,
}

impl PortReservation {
    #[must_use]
    pub const fn port(&self) -> u16 {
        self.port
    }
}

impl Drop for PortReservation {
    fn drop(&mut self) {
        self.pool.reserved.lock().remove(&self.port);
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use std::{
        net::{Ipv4Addr, TcpListener},
        thread,
    };

    fn loopback() -> IpAddr {
        IpAddr::V4(Ipv4Addr::LOCALHOST)
    }

    #[test]
    fn bind_real_omite_un_puerto_ocupado_por_otro_proceso() {
        let (occupied_port, next_port, occupied, next) = open_adjacent_ports().unwrap();
        drop(next);
        let allocator = PortAllocator::new(occupied_port, next_port, loopback()).unwrap();

        let reservation = allocator.reserve().unwrap();

        assert_eq!(reservation.port(), next_port);
        drop(occupied);
    }

    #[test]
    fn reservas_concurrentes_no_duplican_puerto_y_drop_lo_libera() {
        let allocator = PortAllocator::new(24000, 24003, loopback()).unwrap();
        let threads: Vec<_> = (0..4)
            .map(|_| {
                let allocator = allocator.clone();
                thread::spawn(move || allocator.reserve().unwrap())
            })
            .collect();
        let reservations: Vec<_> = threads
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .collect();
        let ports: HashSet<_> = reservations.iter().map(PortReservation::port).collect();
        assert_eq!(ports.len(), 4);

        drop(reservations);
        assert_eq!(allocator.reserve().unwrap().port(), 24000);
    }

    #[test]
    fn rango_agotado_es_reintentable_y_reaparece_al_liberarlo() {
        let allocator = PortAllocator::new(24010, 24010, loopback()).unwrap();
        let reservation = allocator.reserve().unwrap();
        let error = allocator.reserve().unwrap_err();
        assert_eq!(error.code, ErrorCode::ResourceUnavailable);
        assert!(error.retryable);

        drop(reservation);
        assert_eq!(allocator.reserve().unwrap().port(), 24010);
    }

    #[test]
    fn reserva_el_puerto_del_plan_y_rechaza_uso_duplicado_o_fuera_de_rango() {
        let allocator = PortAllocator::new(24100, 24101, loopback()).unwrap();
        let reservation = allocator.reserve_specific(24101).unwrap();
        assert_eq!(reservation.port(), 24101);
        assert_eq!(
            allocator.reserve_specific(24101).unwrap_err().code,
            ErrorCode::ResourceUnavailable
        );
        assert_eq!(
            allocator.reserve_specific(24102).unwrap_err().code,
            ErrorCode::ValidationFailed
        );
        drop(reservation);
        assert_eq!(allocator.reserve_specific(24101).unwrap().port(), 24101);
    }

    #[test]
    fn rango_invalido_se_rechaza_antes_de_tocar_red() {
        assert_eq!(
            PortAllocator::new(0, 10, loopback()).unwrap_err().code,
            ErrorCode::ValidationFailed
        );
        assert_eq!(
            PortAllocator::new(10, 9, loopback()).unwrap_err().code,
            ErrorCode::ValidationFailed
        );
    }

    fn open_adjacent_ports() -> Option<(u16, u16, TcpListener, TcpListener)> {
        for port in 20000..=60000 {
            let Ok(first) = TcpListener::bind(SocketAddr::new(loopback(), port)) else {
                continue;
            };
            let Ok(second) = TcpListener::bind(SocketAddr::new(loopback(), port + 1)) else {
                drop(first);
                continue;
            };
            return Some((port, port + 1, first, second));
        }
        None
    }
}
