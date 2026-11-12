//! Server-owned lifetime of the helper's inherited stdin. Losing the owner
//! closes stdin; the dedicated helper retains the native closure witness.
use super::*;
use tokio::process::{Child, ChildStdin};

/// Neither the child PID nor its exit is authority to release Core resources.
/// Bootstrap secrets remain in memory and the inherited pipe only.
pub struct GuardianProcess {
    child: Child,
    _input: ChildStdin,
}
impl GuardianProcess {
    /// Diagnostic identity only; never resource-release authority.
    pub fn process_id(&self) -> Option<u32> {
        self.child.id()
    }
    /// For a registered transformation the daemon must commit its matching
    /// preparation intent before calling.
    /// Its helper verifies that record before any adapter handshake.
    pub async fn start(
        executable: &std::path::Path,
        bootstrap: &GuardianBootstrap,
    ) -> Result<Self, LarmError> {
        if !executable.is_absolute() {
            return Err(error("guardian_executable_invalid"));
        }
        let mut command = tokio::process::Command::new(executable);
        command
            .arg("worker-guardian")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(false);
        #[cfg(windows)]
        command.creation_flags(0x0800_0000);
        let mut child = command.spawn().map_err(io)?;
        let mut input = child
            .stdin
            .take()
            .ok_or_else(|| error("guardian_stdin_missing"))?;
        // A failed write is ambiguous: drop only this pipe, never claim closure
        // from child termination or erase its already committed intent.
        bootstrap.write_to(&mut input).await?;
        input.flush().await.map_err(io)?;
        Ok(Self {
            child,
            _input: input,
        })
    }
    pub async fn client(
        &mut self,
        bootstrap: &GuardianBootstrap,
    ) -> Result<GuardianTransformationClient, LarmError> {
        tokio::time::timeout(Duration::from_secs(60), async {
            loop {
                match bootstrap.transformation_client().await {
                    Ok(client) => return Ok(client),
                    Err(original) => {
                        if self.child.try_wait().map_err(io)?.is_some() {
                            return Err(original);
                        }
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                }
            }
        })
        .await
        .map_err(|_| error("guardian_startup_timeout"))?
    }
    /// Only after explicit witness retirement. Waiting never proves worker or
    /// descendant closure and confers no permission to free reservations.
    pub async fn wait(mut self) -> Result<(), LarmError> {
        let status = tokio::time::timeout(Duration::from_secs(5), self.child.wait())
            .await
            .map_err(|_| error("guardian_exit_timeout"))?
            .map_err(io)?;
        if !status.success() {
            return Err(error("guardian_exit_failed"));
        }
        Ok(())
    }
}
