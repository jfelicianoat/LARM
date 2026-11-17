//! Rate limit de fallos de autenticación (P131).
//!
//! El guard de P009 bloquea los intents repetidos de credenciales inválidas
//! por fuente (IP remota del socket, o `local` cuando el router no la
//! expone): tras [`AUTH_FAILURE_LIMIT`] fallos dentro de la ventana
//! [`AUTH_FAILURE_WINDOW`], la fuente queda bloqueada — incluso con un token
//! válido — hasta que el fallo más antiguo salga de la ventana. Sin estado
//! contractual nuevo: la respuesta usa `AUTH_REQUIRED` (401) del catálogo
//! sellado del contrato 07 con la razón `auth_rate_limited` y la cabecera
//! `Retry-After`.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Fallos consecutivos tolerados por fuente dentro de la ventana.
pub const AUTH_FAILURE_LIMIT: usize = 10;

/// Ventana deslizante de los fallos de autenticación.
pub const AUTH_FAILURE_WINDOW: Duration = Duration::from_secs(60);

/// Historial de fallos de autenticación por fuente.
///
/// Componente en memoria del proceso: el daemon es local por defecto y el
/// bloqueo de brute force no necesita persistencia (reiniciar el daemon
/// libera el bloqueo, igual que cualquier ventana deslizante).
#[derive(Default)]
pub struct AuthRateLimiter {
    failures: Mutex<HashMap<String, VecDeque<Instant>>>,
}

impl AuthRateLimiter {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Si la fuente está bloqueada, los segundos restantes de bloqueo.
    ///
    /// El bloqueo es rolling respecto al `now` evaluado: basta con que el
    /// fallo más antiguo siga dentro de la ventana y la cola esté llena.
    #[must_use]
    pub fn retry_after(&self, key: &str, now: Instant) -> Option<Duration> {
        let mut failures = self
            .failures
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let queue = failures.get_mut(key)?;
        Self::prune(queue, now);
        let oldest = queue.front()?;
        (queue.len() >= AUTH_FAILURE_LIMIT)
            .then(|| AUTH_FAILURE_WINDOW.saturating_sub(now.duration_since(*oldest)))
    }

    /// Registra un fallo de autenticación de la fuente en `now`.
    pub fn record_failure(&self, key: &str, now: Instant) {
        let mut failures = self
            .failures
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let queue = failures.entry(key.to_string()).or_default();
        Self::prune(queue, now);
        queue.push_back(now);
    }

    /// Una autenticación correcta limpia el historial de la fuente.
    pub fn reset(&self, key: &str) {
        self.failures
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(key);
    }

    fn prune(queue: &mut VecDeque<Instant>, now: Instant) {
        while queue
            .front()
            .is_some_and(|at| now.duration_since(*at) >= AUTH_FAILURE_WINDOW)
        {
            queue.pop_front();
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn bajo_el_umbral_no_bloquea_y_el_duodecimo_fallo_bloquea() {
        let limiter = AuthRateLimiter::new();
        let base = Instant::now();
        for index in 0..AUTH_FAILURE_LIMIT as u64 {
            assert!(
                limiter
                    .retry_after("ip", base + Duration::from_secs(index))
                    .is_none(),
                "fallo {index} con límite {AUTH_FAILURE_LIMIT}: sin bloqueo"
            );
            limiter.record_failure("ip", base + Duration::from_secs(index));
        }
        // Diez fallos en la ventana: el siguiente intento (con token válido
        // o sin él) sale bloqueado con espera positiva.
        let blocked = limiter
            .retry_after("ip", base + Duration::from_secs(AUTH_FAILURE_LIMIT as u64))
            .expect("la fuente llena queda bloqueada");
        assert!(blocked > Duration::ZERO, "espera positiva: {blocked:?}");
        assert!(blocked <= AUTH_FAILURE_WINDOW);
    }

    #[test]
    fn la_ventana_deslizante_libera_la_fuente() {
        let limiter = AuthRateLimiter::new();
        let base = Instant::now();
        for index in 0..AUTH_FAILURE_LIMIT as u64 {
            limiter.record_failure("ip", base + Duration::from_secs(index));
        }
        assert!(limiter
            .retry_after("ip", base + AUTH_FAILURE_WINDOW)
            .is_none());
    }

    #[test]
    fn reset_tras_exito_libera_inmediatamente() {
        let limiter = AuthRateLimiter::new();
        let base = Instant::now();
        for index in 0..AUTH_FAILURE_LIMIT as u64 {
            limiter.record_failure("ip", base + Duration::from_secs(index));
        }
        assert!(limiter.retry_after("ip", base).is_some());
        limiter.reset("ip");
        assert!(limiter.retry_after("ip", base).is_none());
    }

    #[test]
    fn las_fuentes_son_independientes() {
        let limiter = AuthRateLimiter::new();
        let base = Instant::now();
        for index in 0..AUTH_FAILURE_LIMIT as u64 {
            limiter.record_failure("10.0.0.1", base + Duration::from_secs(index));
        }
        assert!(limiter.retry_after("10.0.0.1", base).is_some());
        assert!(limiter.retry_after("10.0.0.2", base).is_none());
    }

    #[test]
    fn limpiar_fallos_viejos_mantiene_el_bloqueo_solo_con_los_recientes() {
        let limiter = AuthRateLimiter::new();
        let base = Instant::now();
        for index in 0..AUTH_FAILURE_LIMIT as u64 {
            limiter.record_failure("ip", base + Duration::from_secs(index));
        }
        assert!(limiter.retry_after("ip", base).is_some());
        // A un minuto los fallos viejos salen de la ventana y la fuente se
        // libera aunque el estado persista.
        let later = base + AUTH_FAILURE_WINDOW;
        assert!(limiter.retry_after("ip", later).is_none());
        // Volviendo a llenar con fallos recientes vuelve el bloqueo.
        for index in 0..AUTH_FAILURE_LIMIT as u64 {
            limiter.record_failure("ip", later + Duration::from_secs(index));
        }
        assert!(limiter
            .retry_after("ip", later + Duration::from_secs(AUTH_FAILURE_LIMIT as u64))
            .is_some());
    }
}
