//! Transporte sin handshake automático, exclusivo de suites P026 del host.
//! No acepta mutantes ni concede una autorización distinta de la medida por P022.
use super::{error, invalid};
use crate::policy::LaunchAuthorization;
use crate::worker::{process_command, Containment, HostConfig, TreeKill, WorkerSpec};
use larm_adapter_sdk::permissions::{execution_digest, Access, Grants};
use larm_adapter_sdk::protocol::MAX_FRAME_BYTES;
use larm_domain::error::{ErrorCode, LarmError};
use sha2::{Digest, Sha256};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::ChildStdin;
use tokio::sync::{mpsc, watch};

#[derive(Clone, serde::Serialize)]
pub struct ProbeExit {
    pub success: bool,
    pub stdout_sha256: String,
    pub stderr_bytes: u64,
    pub extra_frames: usize,
}
struct Io {
    stdin: tokio::sync::Mutex<Option<ChildStdin>>,
    kill: mpsc::UnboundedSender<()>,
    done: watch::Sender<u8>,
    exited: AtomicBool,
    success: AtomicBool,
    failure: Mutex<Option<&'static str>>,
    stdout: Mutex<Sha256>,
    stderr: AtomicU64,
    _owner: Arc<dyn Send + Sync>,
}
struct Done {
    io: Arc<Io>,
    bit: u8,
}
impl Drop for Done {
    fn drop(&mut self) {
        self.io.done.send_modify(|v| *v |= self.bit);
    }
}
fn task(io: Arc<Io>, bit: u8, future: impl std::future::Future<Output = ()> + Send + 'static) {
    let done = Done { io, bit };
    tokio::spawn(async move {
        let _done = done;
        future.await;
    });
}
fn fail(io: &Io, pid: u32, reason: &'static str) {
    if let Ok(mut failure) = io.failure.lock() {
        failure.get_or_insert(reason);
    }
    if !io.exited.load(Ordering::SeqCst) {
        request_kill(io, pid);
    }
}
fn kill_tree_best_effort(pid: u32) {
    if let Ok(runtime) = tokio::runtime::Handle::try_current() {
        runtime.spawn_blocking(move || TreeKill.kill_tree(pid));
    } else {
        TreeKill.kill_tree(pid);
    }
}
fn request_kill(io: &Io, pid: u32) {
    let _ = io.kill.send(());
    kill_tree_best_effort(pid);
}
async fn wait(io: &Io, timeout: Duration) -> Result<(), LarmError> {
    let mut done = io.done.subscribe();
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if *done.borrow_and_update() == 7 && io.exited.load(Ordering::SeqCst) {
            return Ok(());
        }
        if !matches!(
            tokio::time::timeout_at(deadline, done.changed()).await,
            Ok(Ok(()))
        ) {
            return Err(error(ErrorCode::Timeout, "worker_io_cleanup_timeout"));
        }
    }
}
pub(super) struct RawProbe {
    io: Arc<Io>,
    frames: mpsc::Receiver<Vec<u8>>,
    pid: u32,
    grants: Grants,
    config: HostConfig,
}
impl RawProbe {
    pub(super) fn start(
        spec: &WorkerSpec,
        authorization: &LaunchAuthorization,
        config: HostConfig,
        owner: Arc<dyn Send + Sync>,
    ) -> Result<Self, LarmError> {
        if execution_digest(&spec.manifest)? != authorization.execution_digest {
            return Err(error(ErrorCode::Forbidden, "authorization_mismatch"));
        }
        let mut child = process_command(spec)?
            .spawn()
            .map_err(|_| error(ErrorCode::AdapterCrashed, "spawn_failed"))?;
        let pid = child.id().unwrap_or(0);
        let (kill, mut kill_requests) = mpsc::unbounded_channel();
        let io = Arc::new(Io {
            stdin: tokio::sync::Mutex::new(child.stdin.take()),
            kill,
            done: watch::channel(0).0,
            exited: AtomicBool::new(false),
            success: AtomicBool::new(false),
            failure: Mutex::new(None),
            stdout: Mutex::new(Sha256::new()),
            stderr: AtomicU64::new(0),
            _owner: owner,
        });
        let (sender, frames) = mpsc::channel(16);
        if let Some(stdout) = child.stdout.take() {
            let current = io.clone();
            task(io.clone(), 1, async move {
                let mut reader = BufReader::new(stdout);
                loop {
                    match crate::framing::read_line(&mut reader, MAX_FRAME_BYTES, false).await {
                        Ok(Some(crate::framing::Line::Frame(bytes, observed))) => {
                            if let Ok(mut digest) = current.stdout.lock() {
                                digest.update(&bytes);
                                if observed > bytes.len() as u64 {
                                    digest.update(b"\n");
                                }
                            }
                            if sender.try_send(bytes).is_err() {
                                fail(&current, pid, "probe_stdout_queue_full");
                                return;
                            }
                        }
                        Ok(Some(crate::framing::Line::Overflow(_))) => {
                            fail(&current, pid, "probe_stdout_frame_too_large");
                            return;
                        }
                        Ok(None) => return,
                        Err(_) => {
                            fail(&current, pid, "probe_stdout_read_failed");
                            return;
                        }
                    }
                }
            });
        } else {
            io.done.send_modify(|v| *v |= 1);
        }
        if let Some(mut stderr) = child.stderr.take() {
            let current = io.clone();
            task(io.clone(), 2, async move {
                let mut buffer = [0u8; 8192];
                loop {
                    match stderr.read(&mut buffer).await {
                        Ok(0) => return,
                        Ok(size) => {
                            current.stderr.fetch_add(size as u64, Ordering::Relaxed);
                        }
                        Err(_) => {
                            fail(&current, pid, "probe_stderr_read_failed");
                            return;
                        }
                    }
                }
            });
        } else {
            io.done.send_modify(|v| *v |= 2);
        }
        let current = io.clone();
        task(io.clone(), 4, async move {
            let status = tokio::select! {
                status = child.wait() => status,
                Some(()) = kill_requests.recv() => {
                    let _ = child.start_kill();
                    child.wait().await
                }
            };
            let success = status.is_ok_and(|status| status.success());
            current.success.store(success, Ordering::SeqCst);
            current.exited.store(true, Ordering::SeqCst);
        });
        Ok(Self {
            io,
            frames,
            pid,
            grants: authorization.grants.clone(),
            config,
        })
    }
    fn healthy(&self) -> Result<(), LarmError> {
        let failure = self
            .io
            .failure
            .lock()
            .map_err(|_| error(ErrorCode::Internal, "probe_state_unavailable"))?;
        if let Some(reason) = *failure {
            return Err(error(ErrorCode::ProtocolError, reason));
        }
        Ok(())
    }
    pub(super) async fn send(&self, bytes: &[u8], access: &Access) -> Result<(), LarmError> {
        self.grants.check(access)?;
        self.healthy()?;
        // Admite el caso negativo de límite+100, no líneas arbitrariamente grandes.
        if bytes.len() > MAX_FRAME_BYTES + 100 || bytes.contains(&b'\n') {
            return Err(invalid("probe_input_invalid"));
        }
        let write = async {
            let mut stdin = self.io.stdin.lock().await;
            let stdin = stdin
                .as_mut()
                .ok_or_else(|| error(ErrorCode::AdapterCrashed, "probe_stdin_closed"))?;
            stdin
                .write_all(bytes)
                .await
                .map_err(|_| error(ErrorCode::AdapterCrashed, "probe_write_failed"))?;
            stdin
                .write_all(b"\n")
                .await
                .map_err(|_| error(ErrorCode::AdapterCrashed, "probe_write_failed"))?;
            stdin
                .flush()
                .await
                .map_err(|_| error(ErrorCode::AdapterCrashed, "probe_write_failed"))
        };
        tokio::time::timeout(self.config.rpc_timeout, write)
            .await
            .map_err(|_| error(ErrorCode::Timeout, "probe_write_timeout"))?
    }
    pub(super) async fn next(&mut self, timeout: Duration) -> Result<Vec<u8>, LarmError> {
        self.healthy()?;
        let bytes = tokio::time::timeout(timeout, self.frames.recv())
            .await
            .map_err(|_| error(ErrorCode::Timeout, "probe_reply_timeout"))?
            .ok_or_else(|| error(ErrorCode::AdapterCrashed, "probe_stdout_closed"))?;
        self.healthy()?;
        Ok(bytes)
    }
    pub(super) async fn finish(&mut self) -> Result<ProbeExit, LarmError> {
        match tokio::time::timeout(self.config.cancel_grace, self.io.stdin.lock()).await {
            Ok(mut stdin) => {
                drop(stdin.take());
            }
            Err(_) => {
                if !self.io.exited.load(Ordering::SeqCst) {
                    request_kill(&self.io, self.pid);
                }
            }
        }
        if wait(&self.io, self.config.cancel_grace).await.is_err() {
            if !self.io.exited.load(Ordering::SeqCst) {
                request_kill(&self.io, self.pid);
            }
            wait(&self.io, Duration::from_secs(5)).await?;
        }
        self.healthy()?;
        let mut extra_frames = 0;
        while self.frames.try_recv().is_ok() {
            extra_frames += 1;
        }
        let digest = self
            .io
            .stdout
            .lock()
            .map_err(|_| error(ErrorCode::Internal, "probe_state_unavailable"))?
            .clone();
        Ok(ProbeExit {
            success: self.io.success.load(Ordering::SeqCst),
            stdout_sha256: hex::encode(digest.finalize()),
            stderr_bytes: self.io.stderr.load(Ordering::Relaxed),
            extra_frames,
        })
    }
    pub(super) fn pid(&self) -> u32 {
        self.pid
    }
    pub(super) fn terminate(&self) {
        request_kill(&self.io, self.pid);
    }
}
impl Drop for RawProbe {
    fn drop(&mut self) {
        if !self.io.exited.load(Ordering::SeqCst) {
            request_kill(&self.io, self.pid);
        }
    }
}
