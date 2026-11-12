//! Trusted helper only. Never run this loop in the main API process: joining
//! the outer job changes the lifetime of this process and all future children.
use super::activity::Activity;
use super::*;
struct Active(Arc<Owned>);
impl Drop for Active {
    fn drop(&mut self) {
        self.0.activity.leave();
    }
}

struct Owned {
    tree: ProcessJob,
    worker: Arc<dyn GuardianWorker>,
    activity: Activity,
    nonces: super::control::ControlNonces,
}

impl Owned {
    fn stopping(&self) -> bool {
        self.activity.stopping()
    }
    fn active(&self) -> u32 {
        self.activity.active()
    }
    fn enter(self: &Arc<Self>, read_only: bool) -> Result<Active, LarmError> {
        self.activity.enter(read_only)?;
        Ok(Active(self.clone()))
    }
    fn retire(&self) -> bool {
        self.activity.retire()
    }
    fn request_stop(self: &Arc<Self>) {
        if !self.activity.stop() {
            return;
        }
        let owner = self.clone();
        tokio::spawn(async move {
            // Outer termination also covers children born before inner-job
            // assignment, with or without inherited stdout/stderr handles.
            let close_worker = owner.worker.close();
            let close_tree = async {
                loop {
                    if matches!(
                        owner
                            .tree
                            .terminate_members_except(Some(std::process::id()))
                            .await,
                        Ok(true)
                    ) {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            };
            let _ = tokio::join!(close_worker, close_tree);
            // No persisted/cached "closed" flag. Each response below inspects
            // actual IO and queries the retained native job again.
        });
    }

    fn observe(&self) -> Result<GuardianState, LarmError> {
        if self.stopping()
            && self.active() == 0
            && self.worker.io_drained()
            && self.tree.guardian_is_alone()?
        {
            Ok(GuardianState::Closed)
        } else if self.stopping() {
            Ok(GuardianState::Closing)
        } else {
            Ok(GuardianState::Running)
        }
    }
}

/// Serves a DACL-restricted pipe supplied by the Windows composition root.
/// `owner_lost` resolves on EOF/error/extra data on the original inherited
/// stdin. The helper retains its native authority
/// after closure until an explicit Release, so another host can reattach.
pub async fn serve<L, F>(
    bootstrap: GuardianBootstrap,
    owner_lost: L,
    create_pipe: F,
) -> Result<(), LarmError>
where
    L: std::future::Future<Output = ()> + Send + 'static,
    F: FnMut(bool) -> std::io::Result<NamedPipeServer>,
{
    serve_with(
        bootstrap,
        owner_lost,
        create_pipe,
        |launch, descriptor| async move { launch.start_basic(descriptor.fence.generation).await },
    )
    .await
}

/// The composition root resolves a qualified backend after joining the outer
/// job and before any adapter code. Raw launch documents grant no permission.
pub async fn serve_with<L, F, S, SF>(
    bootstrap: GuardianBootstrap,
    owner_lost: L,
    mut create_pipe: F,
    start: S,
) -> Result<(), LarmError>
where
    L: std::future::Future<Output = ()> + Send + 'static,
    F: FnMut(bool) -> std::io::Result<NamedPipeServer>,
    S: FnOnce(GuardianLaunch, GuardianDescriptor) -> SF,
    SF: std::future::Future<Output = Result<Arc<dyn GuardianWorker>, LarmError>>,
{
    let key = bootstrap.descriptor.validate()?;
    let signing = SigningKey::from_bytes(&bootstrap.signing_seed);
    if signing.verifying_key() != key {
        return Err(error("guardian_bootstrap_key_mismatch"));
    }
    // Exclusively occupy the pipe before executing any adapter code.
    let mut listener = create_pipe(true).map_err(io)?;
    let tree = ProcessJob::enter_guardian()?;
    let worker = match start(bootstrap.launch, bootstrap.descriptor.clone()).await {
        Ok(worker) => worker,
        Err(original) => {
            // Keep the outer containment while disposing any partially started
            // backend. Only then disarm self-kill and report the original error.
            loop {
                if matches!(
                    tree.terminate_members_except(Some(std::process::id()))
                        .await,
                    Ok(true)
                ) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            tree.finish_guardian()?;
            return Err(original);
        }
    };
    let owned = Arc::new(Owned {
        tree,
        worker,
        activity: Activity::default(),
        nonces: Default::default(),
    });
    let monitor = owned.clone();
    let monitor = tokio::spawn(async move {
        owner_lost.await;
        monitor.request_stop();
    });
    let (release, mut released) = tokio::sync::mpsc::channel(1);
    let mut clients = tokio::task::JoinSet::new();
    loop {
        tokio::select! {
            _ = released.recv() => break,
            Some(_) = clients.join_next(), if !clients.is_empty() => {},
            connected = listener.connect() => {
                connected.map_err(io)?;
                let connected = listener;
                // Keep an instance alive between clients; a slow RPC does not
                // block observation, stop or owner-loss cleanup.
                listener = create_pipe(false).map_err(io)?;
                if clients.len() < 16 {
                    let owner=owned.clone();
                    let descriptor=bootstrap.descriptor.clone();
                    let key=signing.clone();
                    let release=release.clone();
                    clients.spawn(async move {
                        let _ = exchange(connected,&descriptor,&key,&owner,release).await;
                    });
                }
            }
        }
    }
    drop(listener);
    monitor.abort();
    let _ = monitor.await;
    clients.abort_all();
    while clients.join_next().await.is_some() {}
    // Remaining cleanup references cannot be discarded merely on timeout.
    let mut owner = owned;
    loop {
        match Arc::try_unwrap(owner) {
            Ok(owner) => return owner.tree.finish_guardian(),
            Err(retained) => {
                owner = retained;
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }
    }
}

async fn exchange(
    mut pipe: NamedPipeServer,
    descriptor: &GuardianDescriptor,
    key: &SigningKey,
    owner: &Arc<Owned>,
    release: tokio::sync::mpsc::Sender<()>,
) -> Result<(), LarmError> {
    let challenge: Challenge = tokio::time::timeout(EXCHANGE_TIMEOUT, read_message(&mut pipe))
        .await
        .map_err(|_| error("guardian_request_timeout"))??;
    if challenge.descriptor != *descriptor || uuid::Uuid::parse_str(&challenge.nonce).is_err() {
        return Err(error("guardian_request_binding_mismatch"));
    }
    if challenge.action == Action::Stop {
        owner.request_stop();
    }
    let result = if let Action::Rpc(request) = &challenge.action {
        verify_control(&key.verifying_key(), &challenge)?;
        if let Err(replayed) = owner.nonces.claim(&challenge.nonce) {
            Some(Err(replayed))
        } else if descriptor.session_identity.is_none() {
            Some(Err(error("guardian_rpc_closed")))
        } else {
            let _active = owner.enter(false)?;
            let _ = request.exchange_timeout()?;
            // The client's observation deadline may expire, but do not abandon
            // qualification IO that retains native pins or can launch a trusted
            // identity probe. Worker RPC itself has its existing bounded timeout.
            // This task remains counted through both physical rechecks; Release
            // cannot abort it because CLOSED also requires zero active calls.
            Some(owner.worker.invoke(request).await)
        }
    } else if challenge.action == Action::Progress {
        verify_control(&key.verifying_key(), &challenge)?;
        if let Err(replayed) = owner.nonces.claim(&challenge.nonce) {
            Some(Err(replayed))
        } else {
            match serde_json::to_value(owner.worker.drain_progress()) {
                Ok(value) => Some(Ok(value)),
                Err(_) => Some(Err(error("guardian_progress_reply_invalid"))),
            }
        }
    } else if matches!(challenge.action, Action::Ready | Action::Recheck) {
        let _active = owner.enter(challenge.action == Action::Recheck)?;
        let checked = if challenge.action == Action::Ready {
            owner.worker.ready().await
        } else {
            owner.worker.recheck().await
        };
        Some(match checked {
            Ok(())
                if challenge.action == Action::Recheck
                    || owner.tree.contains_member(owner.worker.pid())? =>
            {
                Ok(serde_json::Value::Bool(true))
            }
            Ok(()) => Err(error("guardian_worker_not_present")),
            Err(original) => Err(original),
        })
    } else {
        None
    };
    let mut state = owner.observe()?;
    let retire =
        challenge.action == Action::Release && state == GuardianState::Closed && owner.retire();
    // A read-only physical recheck can enter after observe(). Its retained
    // resources must drain before Release may acknowledge retirement.
    if challenge.action == Action::Release && state == GuardianState::Closed && !retire {
        state = GuardianState::Closing;
    }
    let observation = Observation {
        challenge,
        state,
        guardian_pid: std::process::id(),
        worker_pid: owner.worker.pid(),
        active_rpcs: owner.active(),
        result,
    };
    let signature = hex::encode(key.sign(&signed_bytes(&observation)?).to_bytes());
    tokio::time::timeout(
        EXCHANGE_TIMEOUT,
        write_message(
            &mut pipe,
            &SignedObservation {
                observation,
                signature,
            },
        ),
    )
    .await
    .map_err(|_| error("guardian_response_timeout"))??;
    if retire {
        let _ = release.try_send(());
    }
    Ok(())
}
