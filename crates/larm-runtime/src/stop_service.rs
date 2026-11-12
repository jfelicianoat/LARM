//! Consumidor de jobs de parada de RuntimeInstance (P033).

use crate::supervisor::{stop_owned_process, OwnedProcessController, StopOutcome};
use larm_domain::{
    bridge::JobPhase,
    error::{ErrorCode, LarmError},
    instances::{InstanceState, RuntimeInstanceStopJob, StopResult},
    runtime_ports::{RuntimeInstanceRepository, RuntimeInstanceStopRepository},
};
use std::sync::Arc;
use std::time::Duration;

pub const DEFAULT_STOP_BATCH: u32 = 4;

pub struct RuntimeInstanceStopService {
    pub instances: Arc<dyn RuntimeInstanceRepository>,
    pub jobs: Arc<dyn RuntimeInstanceStopRepository>,
    pub controller: Arc<dyn OwnedProcessController>,
    pub grace: Duration,
}

impl RuntimeInstanceStopService {
    pub fn new(
        instances: Arc<dyn RuntimeInstanceRepository>,
        jobs: Arc<dyn RuntimeInstanceStopRepository>,
        controller: Arc<dyn OwnedProcessController>,
        grace: Duration,
    ) -> Result<Self, LarmError> {
        if grace > Duration::from_secs(120) {
            return Err(LarmError::new(
                ErrorCode::ValidationFailed,
                "Tiempo de gracia de parada no válido",
                "runtime_stop_grace_invalid",
            ));
        }
        Ok(Self {
            instances,
            jobs,
            controller,
            grace,
        })
    }

    /// Consume hasta `limit` jobs reclamados transaccionalmente.
    pub async fn consume_queued(
        &self,
        limit: u32,
    ) -> Result<Vec<(String, Result<StopOutcome, LarmError>)>, LarmError> {
        if !(1..=64).contains(&limit) {
            return Err(LarmError::new(
                ErrorCode::ValidationFailed,
                "Límite de jobs de parada no válido",
                "runtime_stop_batch_invalid",
            ));
        }
        let mut outcomes = Vec::new();
        for _ in 0..limit {
            let Some(job) = self.jobs.claim_stop_job().await? else {
                break;
            };
            let job_id = job.job_id.clone();
            let outcome = self.execute_claimed(&job).await;
            outcomes.push((job_id, outcome));
        }
        Ok(outcomes)
    }

    async fn execute_claimed(
        &self,
        job: &RuntimeInstanceStopJob,
    ) -> Result<StopOutcome, LarmError> {
        if job.state != JobPhase::Running {
            return Err(LarmError::new(
                ErrorCode::IntegrityFailed,
                "El job de parada no fue reclamado correctamente",
                "runtime_stop_job_not_running",
            )
            .with_resource(&job.job_id));
        }
        let result = self.stop_instance(job).await;
        match result {
            Ok(outcome) => {
                self.jobs
                    .complete_stop_job(&job.job_id, StopResult { stopped: true })
                    .await?;
                Ok(outcome)
            }
            Err(error) => {
                self.jobs.fail_stop_job(&job.job_id, &error).await?;
                Err(error)
            }
        }
    }

    async fn stop_instance(&self, job: &RuntimeInstanceStopJob) -> Result<StopOutcome, LarmError> {
        let instance = self
            .instances
            .instance(&job.instance_id)
            .await?
            .ok_or_else(|| {
                LarmError::new(
                    ErrorCode::NotFound,
                    "Instancia de runtime desconocida",
                    "runtime_instance_not_found",
                )
                .with_resource(&job.instance_id)
            })?;
        if instance.state != InstanceState::Stopping {
            return Err(LarmError::new(
                ErrorCode::StaleRevision,
                "La instancia ya no está en parada",
                "runtime_instance_stop_state_stale",
            )
            .with_resource(&instance.id));
        }
        stop_owned_process(&instance, self.controller.as_ref(), self.grace).await
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

    use super::*;
    use crate::supervisor::{ProcessIdentity, ProcessProbe};
    use async_trait::async_trait;
    use larm_domain::{
        bridge::{JobReceipt, JobState},
        error::LarmError,
        instances::{RuntimeInstance, StopRequest},
        ports::{JobSource, MutationContext, Principal},
        runtime::RuntimeOwnership,
        runtime_ports::{RuntimeInstanceRepository, RuntimeInstanceStopRepository},
    };
    use serde_json::Map;
    use std::{collections::VecDeque, sync::Mutex};

    fn id(n: u16) -> String {
        format!("0192aaaa-0000-7000-8000-{n:012x}")
    }

    fn instance() -> RuntimeInstance {
        RuntimeInstance {
            id: id(1),
            created_at: "2026-10-22T00:00:00.000Z".into(),
            updated_at: "2026-10-22T00:00:00.000Z".into(),
            row_version: 2,
            metadata: Map::new(),
            runtime_installation_id: id(2),
            variant_id: id(3),
            backend_id: id(4),
            launch_profile_id: id(5),
            state: InstanceState::Stopping,
            pid: Some(1234),
            process_started_at: Some("2026-10-22T00:00:00.000Z".into()),
            executable_hash: Some("a".repeat(64)),
            generation: 3,
            execution_fingerprint: Some("b".repeat(64)),
            last_used_at: None,
            ownership: RuntimeOwnership::LarmManaged,
        }
    }

    fn job() -> RuntimeInstanceStopJob {
        RuntimeInstanceStopJob {
            job_id: id(10),
            actor_id: id(11),
            correlation_id: id(12),
            instance_id: id(1),
            request: StopRequest {
                reason: "operator_requested".into(),
                approval_id: None,
            },
            state: JobPhase::Queued,
            result: None,
            error: None,
            evidence_refs: Vec::new(),
        }
    }

    #[derive(Default)]
    struct MemoryInstances(Mutex<Option<RuntimeInstance>>);

    #[async_trait]
    impl RuntimeInstanceRepository for MemoryInstances {
        async fn create_instance(
            &self,
            _instance: &RuntimeInstance,
            _ctx: &MutationContext,
        ) -> Result<(), LarmError> {
            unreachable!()
        }

        async fn instance(&self, _id: &str) -> Result<Option<RuntimeInstance>, LarmError> {
            Ok(self.0.lock().unwrap().clone())
        }

        async fn instances(
            &self,
            _after: Option<(String, String)>,
            _limit: u32,
            _runtime_id: Option<&str>,
        ) -> Result<Vec<RuntimeInstance>, LarmError> {
            Ok(Vec::new())
        }

        async fn runtime_exists(&self, _id: &str) -> Result<bool, LarmError> {
            Ok(false)
        }

        async fn transition_instance(
            &self,
            _instance: &RuntimeInstance,
            _expected_row_version: i64,
            _ctx: &MutationContext,
        ) -> Result<RuntimeInstance, LarmError> {
            unreachable!()
        }

        async fn begin_stop_instance(
            &self,
            _instance: &RuntimeInstance,
            _expected_row_version: i64,
            _ctx: &MutationContext,
        ) -> Result<RuntimeInstance, LarmError> {
            unreachable!()
        }
    }

    #[derive(Default)]
    struct MemoryJobs {
        queued: Mutex<VecDeque<RuntimeInstanceStopJob>>,
        completed: Mutex<Vec<String>>,
        failed: Mutex<Vec<(String, LarmError)>>,
    }

    #[async_trait]
    impl JobSource for MemoryJobs {
        async fn job(
            &self,
            _principal: &Principal,
            _job_id: &str,
        ) -> Result<Option<JobState>, LarmError> {
            Ok(None)
        }
    }

    #[async_trait]
    impl RuntimeInstanceStopRepository for MemoryJobs {
        async fn submit_stop(
            &self,
            _instance_id: &str,
            _request: &StopRequest,
            _body_hash: &str,
            _idempotency_key: &str,
            _ctx: &MutationContext,
        ) -> Result<JobReceipt, LarmError> {
            unreachable!()
        }

        async fn claim_stop_job(&self) -> Result<Option<RuntimeInstanceStopJob>, LarmError> {
            let Some(mut job) = self.queued.lock().unwrap().pop_front() else {
                return Ok(None);
            };
            job.state = JobPhase::Running;
            Ok(Some(job))
        }

        async fn complete_stop_job(
            &self,
            job_id: &str,
            result: StopResult,
        ) -> Result<(), LarmError> {
            assert!(result.stopped);
            self.completed.lock().unwrap().push(job_id.to_owned());
            Ok(())
        }

        async fn fail_stop_job(&self, job_id: &str, error: &LarmError) -> Result<(), LarmError> {
            self.failed
                .lock()
                .unwrap()
                .push((job_id.to_owned(), error.clone()));
            Ok(())
        }

        async fn recover_stop_jobs(&self) -> Result<u64, LarmError> {
            Ok(0)
        }
    }

    struct FakeController {
        probes: Mutex<VecDeque<ProcessProbe>>,
        waits: Mutex<VecDeque<bool>>,
        graceful_calls: Mutex<u32>,
        force_calls: Mutex<u32>,
    }

    impl FakeController {
        fn new(probes: Vec<ProcessProbe>, waits: Vec<bool>) -> Self {
            Self {
                probes: Mutex::new(probes.into()),
                waits: Mutex::new(waits.into()),
                graceful_calls: Mutex::new(0),
                force_calls: Mutex::new(0),
            }
        }
    }

    #[async_trait]
    impl OwnedProcessController for FakeController {
        async fn probe(&self, _expected: &ProcessIdentity) -> Result<ProcessProbe, LarmError> {
            Ok(self.probes.lock().unwrap().pop_front().unwrap())
        }

        async fn request_graceful_stop(
            &self,
            _expected: &ProcessIdentity,
        ) -> Result<(), LarmError> {
            *self.graceful_calls.lock().unwrap() += 1;
            Ok(())
        }

        async fn wait_for_exit(
            &self,
            _expected: &ProcessIdentity,
            _timeout: Duration,
        ) -> Result<bool, LarmError> {
            Ok(self.waits.lock().unwrap().pop_front().unwrap())
        }

        async fn force_stop_tree(&self, _expected: &ProcessIdentity) -> Result<(), LarmError> {
            *self.force_calls.lock().unwrap() += 1;
            Ok(())
        }
    }

    fn running_identity() -> ProcessProbe {
        ProcessProbe::Running(ProcessIdentity {
            pid: 1234,
            creation_time: "2026-10-22T00:00:00.000Z".into(),
            executable_sha256: "a".repeat(64),
            generation: 3,
        })
    }

    fn service(
        controller: Arc<FakeController>,
        jobs: Arc<MemoryJobs>,
    ) -> RuntimeInstanceStopService {
        let instances = Arc::new(MemoryInstances(Mutex::new(Some(instance()))));
        let instance_repo: Arc<dyn RuntimeInstanceRepository> = instances;
        let job_repo: Arc<dyn RuntimeInstanceStopRepository> = jobs;
        let process_controller: Arc<dyn OwnedProcessController> = controller;
        RuntimeInstanceStopService::new(
            instance_repo,
            job_repo,
            process_controller,
            Duration::from_secs(15),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn consume_completes_only_after_graceful_exit_is_confirmed() {
        let jobs = Arc::new(MemoryJobs::default());
        jobs.queued.lock().unwrap().push_back(job());
        let controller = Arc::new(FakeController::new(vec![running_identity()], vec![true]));
        let service = service(controller.clone(), jobs.clone());

        let outcomes = service.consume_queued(1).await.unwrap();

        assert_eq!(outcomes.len(), 1);
        assert_eq!(outcomes[0].0, id(10));
        assert_eq!(outcomes[0].1, Ok(StopOutcome::Graceful));
        assert_eq!(*controller.graceful_calls.lock().unwrap(), 1);
        assert_eq!(*controller.force_calls.lock().unwrap(), 0);
        assert_eq!(*jobs.completed.lock().unwrap(), vec![id(10)]);
        assert!(jobs.failed.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn identity_mismatch_fails_the_job_without_forcing_or_completing() {
        let jobs = Arc::new(MemoryJobs::default());
        jobs.queued.lock().unwrap().push_back(job());
        let mut replacement = match running_identity() {
            ProcessProbe::Running(identity) => identity,
            _ => unreachable!(),
        };
        replacement.pid += 1;
        let controller = Arc::new(FakeController::new(
            vec![ProcessProbe::Running(replacement)],
            Vec::new(),
        ));
        let service = service(controller.clone(), jobs.clone());

        let outcomes = service.consume_queued(1).await.unwrap();

        assert_eq!(
            outcomes[0].1.as_ref().unwrap_err().details.reason,
            "runtime_process_identity_ambiguous"
        );
        assert_eq!(*controller.graceful_calls.lock().unwrap(), 0);
        assert_eq!(*controller.force_calls.lock().unwrap(), 0);
        assert!(jobs.completed.lock().unwrap().is_empty());
        assert_eq!(jobs.failed.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn timeout_revalidates_identity_before_forcing_process_tree() {
        let jobs = Arc::new(MemoryJobs::default());
        jobs.queued.lock().unwrap().push_back(job());
        let controller = Arc::new(FakeController::new(
            vec![running_identity(), running_identity()],
            vec![false, true],
        ));
        let service = service(controller.clone(), jobs.clone());

        let outcomes = service.consume_queued(1).await.unwrap();

        assert_eq!(outcomes[0].1, Ok(StopOutcome::Forced));
        assert_eq!(*controller.force_calls.lock().unwrap(), 1);
        assert_eq!(*jobs.completed.lock().unwrap(), vec![id(10)]);
    }

    #[tokio::test]
    async fn consume_limit_is_validated_before_claiming() {
        let jobs = Arc::new(MemoryJobs::default());
        let controller = Arc::new(FakeController::new(Vec::new(), Vec::new()));
        let service = service(controller, jobs.clone());

        let error = service.consume_queued(0).await.unwrap_err();

        assert_eq!(error.details.reason, "runtime_stop_batch_invalid");
        assert!(jobs.queued.lock().unwrap().is_empty());
    }
}
