//! Cliente source tipado. Conserva el host P021 y todos los gates P022.
use crate::policy::GuardedWorker;
use larm_adapter_sdk::permissions::Access;
use larm_adapter_sdk::protocol::Request;
use larm_adapter_sdk::source::{
    download_spec, validate_source_binding, SearchSupport, SourceReply, SourceRequest,
};
use larm_domain::bridge::is_uuid_v7;
use larm_domain::error::{ErrorCode, LarmError};
use larm_domain::ports::AdapterQualificationGuard;
use std::sync::Arc;
use std::{future::Future, time::Duration};

async fn qualified_io<T, F, Fut>(
    qualification: Option<&dyn AdapterQualificationGuard>,
    operation: F,
) -> Result<T, LarmError>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<T, LarmError>>,
{
    if let Some(qualification) = qualification {
        qualification.recheck().await?;
    }
    let result = operation().await;
    if let Some(qualification) = qualification {
        let recheck = qualification.recheck().await;
        if result.is_ok() {
            recheck?;
        }
    }
    result
}

#[derive(Clone)]
pub struct SourceWorker {
    worker: Arc<GuardedWorker>,
    qualification: Option<Arc<dyn AdapterQualificationGuard>>,
    source_id: String,
    support: SearchSupport,
}
impl SourceWorker {
    pub fn new(
        worker: Arc<GuardedWorker>,
        source_id: String,
        support: SearchSupport,
    ) -> Result<Self, LarmError> {
        if !is_uuid_v7(&source_id) {
            return Err(LarmError::new(
                ErrorCode::ValidationFailed,
                "Fuente no válida",
                "source_id_invalid",
            ));
        }
        validate_source_binding(worker.manifest(), &support)?;
        Ok(Self {
            worker,
            qualification: None,
            source_id,
            support,
        })
    }

    pub(crate) fn new_qualified(
        worker: Arc<GuardedWorker>,
        source_id: String,
        support: SearchSupport,
        qualification: Arc<dyn AdapterQualificationGuard>,
    ) -> Result<Self, LarmError> {
        let mut source = Self::new(worker, source_id, support)?;
        source.qualification = Some(qualification);
        Ok(source)
    }
    pub async fn call(
        &self,
        request: SourceRequest,
        access: &Access,
        timeout: Option<Duration>,
    ) -> Result<SourceReply, LarmError> {
        self.call_with_id(&uuid::Uuid::now_v7().to_string(), request, access, timeout)
            .await
    }
    pub async fn call_with_id(
        &self,
        id: &str,
        request: SourceRequest,
        access: &Access,
        timeout: Option<Duration>,
    ) -> Result<SourceReply, LarmError> {
        if !is_uuid_v7(id) {
            return Err(LarmError::new(
                ErrorCode::ValidationFailed,
                "Id RPC no válido",
                "request_id_invalid",
            ));
        }
        request.validate()?;
        if request
            .source_id()
            .is_some_and(|source| source != self.source_id)
        {
            return Err(LarmError::new(
                ErrorCode::Forbidden,
                "La consulta pertenece a otra fuente",
                "source_identity_mismatch",
            ));
        }
        if let SourceRequest::Search(query) = &request {
            self.support.require(query)?;
        }
        let resource = match &request {
            SourceRequest::Download(plan) => Some(plan.plan_id.as_str()),
            SourceRequest::DownloadChunk(chunk) => Some(chunk.job_id.as_str()),
            SourceRequest::Sync(_) => Some(self.source_id.as_str()),
            _ => None,
        };
        let wire = Request {
            jsonrpc: "2.0".into(),
            id: id.into(),
            method: request.method().into(),
            params: request.params()?,
        };
        let value = qualified_io(self.qualification.as_deref(), || {
            self.worker.call_request(wire, access, timeout, resource)
        })
        .await?;
        let reply = SourceReply::parse(&request, value)?;
        if let SourceReply::DownloadPlan(plan) = &reply {
            if download_spec(plan)?.revision.source_id != self.source_id {
                return Err(LarmError::new(
                    ErrorCode::ProtocolError,
                    "Plan de otra fuente",
                    "source_identity_mismatch",
                ));
            }
        }
        Ok(reply)
    }
    pub async fn cancel(&self, id: &str) -> Result<bool, LarmError> {
        if !is_uuid_v7(id) {
            return Err(LarmError::new(
                ErrorCode::ValidationFailed,
                "Id RPC no válido",
                "request_id_invalid",
            ));
        }
        self.worker.cancel(id).await
    }
    pub async fn stop(&self) {
        self.worker.stop().await;
    }
    /// Cierre supervisado: no debe completarse un job hasta confirmar que el
    /// proceso y sus canales de IO han terminado.
    pub async fn close(&self) -> Result<(), LarmError> {
        self.worker.stop_and_wait().await
    }
    #[must_use]
    pub fn io_drained(&self) -> bool {
        self.worker.io_drained()
    }
    pub fn state(&self) -> crate::worker::WorkerState {
        self.worker.state()
    }
    #[must_use]
    pub fn supports_download_chunks(&self) -> bool {
        self.worker
            .manifest()
            .methods
            .iter()
            .any(|method| method == "source.download_chunk")
    }
}

#[cfg(test)]
mod qualification_tests {
    use super::*;
    use larm_domain::error::ErrorCode;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct CountingGuard {
        calls: AtomicUsize,
        fail_at: Option<usize>,
    }

    #[async_trait::async_trait]
    impl AdapterQualificationGuard for CountingGuard {
        async fn recheck(&self) -> Result<(), LarmError> {
            let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
            if self.fail_at == Some(call) {
                return Err(LarmError::new(
                    ErrorCode::Forbidden,
                    "La cualificación dejó de ser válida",
                    "qualification_revoked",
                ));
            }
            Ok(())
        }
    }

    #[tokio::test]
    async fn source_rpc_revalida_antes_y_despues_y_conserva_error_original() {
        let guard = CountingGuard {
            calls: AtomicUsize::new(0),
            fail_at: None,
        };
        let mut calls = 0;
        let result = qualified_io(Some(&guard), || async {
            calls += 1;
            Ok::<_, LarmError>("ok")
        })
        .await;
        assert!(matches!(result, Ok("ok")));
        assert_eq!(calls, 1);
        assert_eq!(guard.calls.load(Ordering::SeqCst), 2);

        let guard = CountingGuard {
            calls: AtomicUsize::new(0),
            fail_at: Some(1),
        };
        let mut calls = 0;
        let result = qualified_io(Some(&guard), || async {
            calls += 1;
            Ok::<_, LarmError>(())
        })
        .await;
        assert_eq!(
            result
                .err()
                .as_ref()
                .map(|error| error.details.reason.as_str()),
            Some("qualification_revoked")
        );
        assert_eq!(calls, 0, "la fuente no se invoca si falla el gate previo");

        let guard = CountingGuard {
            calls: AtomicUsize::new(0),
            fail_at: Some(2),
        };
        let result = qualified_io(Some(&guard), || async {
            Err::<(), _>(LarmError::new(
                ErrorCode::ProtocolError,
                "Error original de RPC",
                "source_rpc_failed",
            ))
        })
        .await;
        assert_eq!(
            result
                .err()
                .as_ref()
                .map(|error| error.details.reason.as_str()),
            Some("source_rpc_failed")
        );
    }
}
