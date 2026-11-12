//! Materialización determinista de perfiles builtin (P034, contrato 04).

use larm_domain::{
    error::{ErrorCode, LarmError},
    launch::{effective_memory_limit, LaunchParameters},
    launch_profile::{LaunchProfile, LaunchProfileKind},
    units::DecimalU64,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileMaterializationContext {
    pub backend_id: String,
    pub logical_threads: u32,
    pub physical_cores: u32,
    /// Contexto máximo del modelo, o `None` si el catálogo no lo conoce.
    pub model_context_limit: Option<u32>,
    /// Batch es explícito: el contrato no fija un valor universal.
    pub batch: u32,
}

fn invalid(reason: &'static str, message: &str) -> LarmError {
    LarmError::new(ErrorCode::ValidationFailed, message, reason)
}

/// Materializa ECO/BALANCED/PERFORMANCE/LONG_CONTEXT con los datos ya
/// observados por el caller. CUSTOM exige parámetros explícitos.
pub fn materialize_builtin(
    kind: LaunchProfileKind,
    context: &ProfileMaterializationContext,
) -> Result<LaunchParameters, LarmError> {
    if !larm_domain::bridge::is_uuid_v7(&context.backend_id)
        || context.logical_threads == 0
        || context.physical_cores == 0
        || context.batch == 0
        || context.model_context_limit == Some(0)
    {
        return Err(invalid(
            "launch_profile_materialization_input_invalid",
            "backend, hardware observado, contexto y batch deben tener valores válidos",
        ));
    }

    let (threads, context_limit) = match kind {
        LaunchProfileKind::Eco => (
            (context.logical_threads / 2).max(1),
            context.model_context_limit.map(|limit| limit.min(2048)),
        ),
        LaunchProfileKind::Balanced => (
            context.physical_cores.saturating_sub(1).max(1),
            context.model_context_limit.map(|limit| limit.min(4096)),
        ),
        LaunchProfileKind::Performance => (
            context.logical_threads.saturating_sub(1).max(1),
            context.model_context_limit.map(|limit| limit.min(4096)),
        ),
        LaunchProfileKind::LongContext => (
            context.physical_cores.saturating_sub(1).max(1),
            context.model_context_limit,
        ),
        LaunchProfileKind::Custom => {
            return Err(invalid(
                "launch_profile_custom_requires_parameters",
                "CUSTOM requiere parámetros de lanzamiento explícitos",
            ));
        }
    };

    let parameters = LaunchParameters::builtin(
        context.backend_id.clone(),
        threads,
        context_limit,
        context.batch,
    );
    parameters
        .validate()
        .map_err(|error| invalid(error.reason, &error.detail))?;
    Ok(parameters)
}

/// Hashes de comparación aptos para benchmark: cada cambio efectivo queda
/// identificado por el dominio y su diff estable, sin incluir timestamps.
pub fn compare_profiles(
    baseline: &LaunchProfile,
    candidate: &LaunchProfile,
) -> Result<Vec<String>, LarmError> {
    baseline.diff(candidate)
}

/// Combina el límite del perfil y el de la lease por el mínimo de los valores
/// declarados, manteniendo `None` como ausencia de límite adicional.
#[must_use]
pub fn effective_memory_limit_bytes(
    profile: Option<DecimalU64>,
    lease: Option<DecimalU64>,
) -> Option<DecimalU64> {
    effective_memory_limit(profile, lease)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    const BACKEND: &str = "0192aaaa-0000-7000-8000-000000000001";

    fn context() -> ProfileMaterializationContext {
        ProfileMaterializationContext {
            backend_id: BACKEND.into(),
            logical_threads: 16,
            physical_cores: 8,
            model_context_limit: Some(8192),
            batch: 512,
        }
    }

    #[test]
    fn materializa_formulas_builtin_sin_ocultar_batch_ni_contexto() {
        let eco = materialize_builtin(LaunchProfileKind::Eco, &context()).unwrap();
        let balanced = materialize_builtin(LaunchProfileKind::Balanced, &context()).unwrap();
        let performance = materialize_builtin(LaunchProfileKind::Performance, &context()).unwrap();
        let long = materialize_builtin(LaunchProfileKind::LongContext, &context()).unwrap();
        assert_eq!(
            (eco.threads, eco.context, eco.concurrency, eco.batch),
            (8, Some(2048), 1, 512)
        );
        assert_eq!((balanced.threads, balanced.context), (7, Some(4096)));
        assert_eq!((performance.threads, performance.context), (15, Some(4096)));
        assert_eq!((long.threads, long.context), (7, Some(8192)));
        for profile in [eco, balanced, performance, long] {
            assert_eq!(profile.memory_limit_bytes, None);
            assert!(profile.runtime_arguments.is_empty());
            assert!(profile.environment.allowlist.is_empty());
            assert!(profile.environment.variables.is_empty());
            assert!(profile.adapter_options.is_empty());
        }
    }

    #[test]
    fn rechaza_custom_y_no_inventa_hardware_ni_contexto() {
        let error = materialize_builtin(LaunchProfileKind::Custom, &context()).unwrap_err();
        assert_eq!(
            error.details.reason,
            "launch_profile_custom_requires_parameters"
        );
        let no_context = ProfileMaterializationContext {
            model_context_limit: None,
            ..context()
        };
        let balanced = materialize_builtin(LaunchProfileKind::Balanced, &no_context).unwrap();
        assert_eq!(balanced.context, None);
        let no_cpu = ProfileMaterializationContext {
            logical_threads: 0,
            ..context()
        };
        assert!(materialize_builtin(LaunchProfileKind::Eco, &no_cpu).is_err());
    }

    #[test]
    fn memory_lease_limit_only_reduces_profile_limit() {
        let profile = DecimalU64::new(2048).unwrap();
        let lease = DecimalU64::new(1024).unwrap();
        assert_eq!(
            effective_memory_limit_bytes(Some(profile), Some(lease))
                .unwrap()
                .get(),
            1024
        );
        assert_eq!(
            effective_memory_limit_bytes(Some(profile), None)
                .unwrap()
                .get(),
            2048
        );
    }
}
