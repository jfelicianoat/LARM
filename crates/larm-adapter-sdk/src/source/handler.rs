use super::{invalid, SearchSupport, SourceReply, SourceRequest};
use crate::manifest::{AdapterManifest, Family, MethodTable};
use crate::protocol::{RpcError, INVALID_PARAMS};
use crate::server::{AdapterHandler, CallContext};
use async_trait::async_trait;
use larm_domain::error::{ErrorCode, LarmError};
use serde_json::Value;
use std::sync::Arc;

/// Puerto que implementa cada fuente concreta. No hay defaults exitosos;
/// persistir una descarga y resolver credenciales son de sus propietarios.
#[async_trait]
pub trait ModelSourceAdapter: Send + Sync + 'static {
    fn manifest(&self) -> &AdapterManifest;
    fn search_support(&self) -> &SearchSupport;
    fn capability_registry(&self) -> Option<&crate::capabilities::CapabilityRegistry> {
        None
    }
    async fn invoke(
        &self,
        request: SourceRequest,
        context: CallContext,
    ) -> Result<SourceReply, LarmError>;
}

pub struct SourceHandler<A: ModelSourceAdapter> {
    adapter: Arc<A>,
}
impl<A: ModelSourceAdapter> SourceHandler<A> {
    pub fn new(adapter: Arc<A>) -> Result<Self, LarmError> {
        adapter.manifest().validate(&MethodTable::contract())?;
        validate_source_binding(adapter.manifest(), adapter.search_support())?;
        Ok(Self { adapter })
    }
}
pub fn validate_source_binding(
    manifest: &AdapterManifest,
    support: &SearchSupport,
) -> Result<(), LarmError> {
    support.validate()?;
    if manifest.family != Family::Source
        || support.search_applicable != manifest.capabilities.iter().any(|c| c == "source.search")
    {
        return Err(invalid("source_capability_binding"));
    }
    Ok(())
}
fn rpc(error: LarmError) -> RpcError {
    if error.code == ErrorCode::ValidationFailed {
        RpcError {
            code: INVALID_PARAMS,
            message: "SourceRequest inválido".into(),
            data: Some(Box::new(error)),
        }
    } else {
        RpcError::domain(error)
    }
}
#[async_trait]
impl<A: ModelSourceAdapter> AdapterHandler for SourceHandler<A> {
    fn manifest(&self) -> &AdapterManifest {
        self.adapter.manifest()
    }
    fn capability_registry(&self) -> Option<&crate::capabilities::CapabilityRegistry> {
        self.adapter.capability_registry()
    }
    async fn call(
        &self,
        method: &str,
        params: Value,
        context: CallContext,
    ) -> Result<Value, RpcError> {
        self.manifest()
            .require_method(method)
            .map_err(RpcError::domain)?;
        let request = SourceRequest::parse(method, params).map_err(rpc)?;
        if let SourceRequest::Search(query) = &request {
            self.adapter
                .search_support()
                .require(query)
                .map_err(RpcError::domain)?;
        }
        if context.is_cancelled() {
            return Err(RpcError::domain(LarmError::new(
                ErrorCode::ResourceUnavailable,
                "Consulta cancelada",
                "operation_cancelled",
            )));
        }
        let reply = self
            .adapter
            .invoke(request.clone(), context)
            .await
            .map_err(RpcError::domain)?;
        // Una implementación no puede devolver el tipo de otra operación ni
        // enviar un objeto incompleto como éxito. No reintenta el IO ejecutado.
        let value = reply.value().map_err(RpcError::domain)?;
        reply.validate_for(&request).map_err(|_| {
            RpcError::domain(LarmError::new(
                ErrorCode::ProtocolError,
                "Respuesta source fuera de contrato",
                "source_response_invalid",
            ))
        })?;
        SourceReply::parse(&request, value.clone()).map_err(RpcError::domain)?;
        Ok(value)
    }
}
