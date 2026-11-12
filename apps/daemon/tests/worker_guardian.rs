//! Native detached-owner primitive, not public P093/qualification acceptance.
#![cfg(windows)]
#![forbid(unsafe_code)]
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
use larm_adapter_host::{
    guardian::{GuardianBootstrap, GuardianDescriptor, GuardianState, NativeGuardian},
    worker::WorkerSpec,
};
use larm_adapter_sdk::manifest::AdapterManifest;
use larm_domain::transformation_execution::TransformationAttemptFence;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    io::{BufRead, BufReader, Write},
    os::windows::{fs::OpenOptionsExt, process::CommandExt},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};
use tokio::{io::AsyncWriteExt, net::windows::named_pipe::ClientOptions};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

struct Fixture {
    directory: PathBuf,
    work: PathBuf,
    descriptor: GuardianDescriptor,
    parent: Child,
    // sysinfo keeps the exact native process handle, preventing PID recycling
    // while cleanup is authorized for this helper. Never kill by image/PID scan.
    helper: System,
    helper_pid: Pid,
}

impl Fixture {
    async fn start() -> Self {
        let repo = root();
        let directory = repo
            .join("docs/reports/P093_runs/native-worker-guardian")
            .join(uuid::Uuid::now_v7().to_string());
        let package = directory.join("package");
        let work = directory.join("work");
        std::fs::create_dir_all(package.join("bin")).unwrap();
        std::fs::create_dir_all(&work).unwrap();
        let source = repo.join("target/debug/examples/fake_adapter.exe");
        std::fs::copy(&source, package.join("bin/fake_adapter.exe"))
            .expect("build host example first");
        let mut manifest: Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/fake_adapter/manifest.json"
        ))
        .unwrap();
        manifest["executable"]["args"] = json!([
            "--manifest",
            package.join("manifest.json"),
            "--mode",
            "guardian-tree"
        ]);
        std::fs::write(
            package.join("manifest.json"),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        let bootstrap = GuardianBootstrap::new(
            WorkerSpec {
                package_dir: package,
                manifest: AdapterManifest::parse(&manifest.to_string()).unwrap(),
                environment: vec![],
                work_dir: work.clone(),
            },
            TransformationAttemptFence {
                job_id: uuid::Uuid::now_v7().to_string(),
                claim_id: uuid::Uuid::now_v7().to_string(),
                generation: 2,
                attempt: 3,
                plan_hash: "d".repeat(64),
            },
        )
        .unwrap();
        let descriptor = bootstrap.descriptor().clone();
        // Only public metadata survives on disk, as it would in a journal.
        std::fs::write(
            directory.join("descriptor.json"),
            serde_json::to_vec_pretty(&descriptor).unwrap(),
        )
        .unwrap();
        let log = std::fs::File::create(directory.join("helper.log")).unwrap();
        let mut parent = Command::new(repo.join(".venv-tools/Scripts/python.exe"))
            .arg("-X")
            .arg("utf8")
            .arg(repo.join("apps/daemon/tests/support/guardian_owner.py"))
            .arg(env!("CARGO_BIN_EXE_larm-daemon"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::from(log))
            .creation_flags(0x0800_0000)
            .spawn()
            .unwrap();
        let mut bootstrap_bytes = Vec::new();
        bootstrap.write_to(&mut bootstrap_bytes).await.unwrap();
        let mut input = parent.stdin.take().unwrap();
        input.write_all(&bootstrap_bytes).unwrap();
        input.flush().unwrap();
        parent.stdin = Some(input);
        drop(bootstrap_bytes);
        drop(bootstrap);
        let mut line = String::new();
        BufReader::new(parent.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        let launch: Value = serde_json::from_str(&line).expect("owned parent launch reply");
        let helper_pid = Pid::from_u32(launch["helper_pid"].as_u64().unwrap() as u32);
        let mut helper = System::new();
        helper.refresh_processes_specifics(
            ProcessesToUpdate::Some(&[helper_pid]),
            true,
            ProcessRefreshKind::nothing(),
        );
        assert!(helper.process(helper_pid).is_some());
        Self {
            directory,
            work,
            descriptor,
            parent,
            helper,
            helper_pid,
        }
    }

    async fn attach(&mut self) -> NativeGuardian {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if let Ok(owner) = NativeGuardian::attach(self.descriptor.clone()).await {
                return owner;
            }
            assert!(
                Instant::now() < deadline,
                "helper did not start: {}",
                std::fs::read_to_string(self.directory.join("helper.log")).unwrap_or_default()
            );
            assert!(self.parent.try_wait().unwrap().is_none());
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    async fn writers(&self) -> Vec<Value> {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let records: Vec<Value> = std::fs::read_dir(&self.work)
                .unwrap()
                .filter_map(Result::ok)
                .filter(|entry| {
                    entry.file_name().to_string_lossy().starts_with("writer-")
                        && entry.path().extension().is_some_and(|ext| ext == "json")
                })
                .filter_map(|entry| std::fs::read(entry.path()).ok())
                .filter_map(|bytes| serde_json::from_slice(&bytes).ok())
                .collect();
            if records.len() == 3 {
                return records;
            }
            assert!(Instant::now() < deadline, "missing actual writers");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    fn crash_parent(&mut self) {
        self.parent.kill().unwrap();
        self.parent.wait().unwrap();
    }

    async fn wait_helper_exit(&self) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            // Keep the original handle in self.helper; a separate observer
            // cannot recycle this PID or replace our cleanup authorization.
            let mut observer = System::new();
            observer.refresh_processes_specifics(
                ProcessesToUpdate::Some(&[self.helper_pid]),
                true,
                ProcessRefreshKind::nothing(),
            );
            if observer.process(self.helper_pid).is_none() {
                return;
            }
            assert!(Instant::now() < deadline, "guardian did not retire");
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    }

    async fn wait_clean_parent_exit(&mut self) {
        drop(self.parent.stdin.take());
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(status) = self.parent.try_wait().unwrap() {
                assert!(status.success(), "parent observed nonzero helper exit");
                return;
            }
            assert!(Instant::now() < deadline);
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    }

    fn report(&self, case: &str, writers: &[Value], result: Value) {
        let artifacts: Vec<_> = writers.iter().map(|writer| {
            let path = self.work.join(writer["path"].as_str().unwrap());
            let bytes = std::fs::read(&path).unwrap();
            json!({"file":path.strip_prefix(root()).unwrap().to_string_lossy().replace('\\',"/"),
                "size_bytes":bytes.len(),"sha256":hex::encode(Sha256::digest(&bytes))})
        }).collect();
        let report = json!({"status":"PASS", "case":case,
            "scope":"native detached P021 owner primitive; no P022/P093 admission or public daemon recovery acceptance",
            "actual_writers":writers,"retained_artifacts":artifacts,"results":result,
            "p093_resources_released":false,"generated_at":chrono::Utc::now().to_rfc3339()});
        std::fs::write(
            self.directory.join("report.json"),
            serde_json::to_vec_pretty(&report).unwrap(),
        )
        .unwrap();
        let receipt = json!({"status":"PASS", "case":case,
            "evidence":self.directory.strip_prefix(root()).unwrap().to_string_lossy().replace('\\',"/"),
            "generated_at":chrono::Utc::now().to_rfc3339(),"public_p093_accepted":false});
        std::fs::write(
            root().join(format!(
                "docs/reports/P093_runs/native-worker-guardian-{case}.json"
            )),
            serde_json::to_vec_pretty(&receipt).unwrap(),
        )
        .unwrap();
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self.parent.kill();
        let _ = self.parent.wait();
        if let Some(process) = self.helper.process(self.helper_pid) {
            let _ = process.kill();
        }
    }
}

async fn closed(owner: &NativeGuardian) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while owner.require_closed().await.is_err() {
        assert!(Instant::now() < deadline, "physical IO/tree not closed");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn assert_writers_locked(work: &Path, writers: &[Value]) {
    for writer in writers {
        let file = work.join(writer["path"].as_str().unwrap());
        assert!(std::fs::OpenOptions::new()
            .write(true)
            .share_mode(0)
            .open(file)
            .is_err());
    }
}

async fn assert_writers_closed(work: &Path, writers: &[Value]) {
    let mut sizes = Vec::new();
    let ids: Vec<_> = writers
        .iter()
        .map(|writer| Pid::from_u32(writer["pid"].as_u64().unwrap() as u32))
        .collect();
    let mut system = System::new();
    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&ids),
        true,
        ProcessRefreshKind::nothing(),
    );
    for pid in ids {
        assert!(system.process(pid).is_none(), "actual writer still alive");
    }
    for writer in writers {
        let file = work.join(writer["path"].as_str().unwrap());
        let handle = std::fs::OpenOptions::new()
            .write(true)
            .share_mode(0)
            .open(&file)
            .expect("native writer handle released");
        sizes.push((file, handle.metadata().unwrap().len()));
    }
    tokio::time::sleep(Duration::from_millis(150)).await;
    for (path, size) in sizes {
        assert_eq!(std::fs::metadata(path).unwrap().len(), size);
    }
}

#[tokio::test]
async fn propietario_caido_reconexion_desde_descriptor_y_cierre_fisico_de_tres_escritores() {
    let mut fixture = Fixture::start().await;
    let owner = fixture.attach().await;
    assert_eq!(owner.observe().await.unwrap(), GuardianState::Running);
    assert!(owner.require_closed().await.is_err());
    let writers = fixture.writers().await;
    assert_writers_locked(&fixture.work, &writers);
    fixture.crash_parent(); // Actual OS process death, not an in-memory drop.
    drop(owner);
    let descriptor =
        serde_json::from_slice(&std::fs::read(fixture.directory.join("descriptor.json")).unwrap())
            .unwrap();
    let recovered = NativeGuardian::attach(descriptor).await.unwrap();
    closed(&recovered).await;
    assert_writers_closed(&fixture.work, &writers).await;
    // Witness is still alive and physically rechecked on every observation.
    recovered.require_closed().await.unwrap();
    recovered.release().await.unwrap();
    fixture.wait_helper_exit().await;
    fixture.report(
        "owner-crash",
        &writers,
        json!({"parent_crashed":true,
        "reconnected_from_public_descriptor":true,"closed_native_file_handles":3,
        "retained_live_witness":true,"released_after_native_closure":true}),
    );
}

#[tokio::test]
async fn fence_clave_y_clientes_invalidos_no_acreditan_cierre_ni_tiran_el_supervisor() {
    let mut fixture = Fixture::start().await;
    let owner = fixture.attach().await;
    let writers = fixture.writers().await;
    let mut wrong = fixture.descriptor.clone();
    wrong.fence.generation += 1;
    assert!(NativeGuardian::attach(wrong).await.is_err());
    let other = GuardianBootstrap::new(
        WorkerSpec {
            package_dir: fixture.directory.join("package"),
            manifest: AdapterManifest::parse(
                &std::fs::read_to_string(fixture.directory.join("package/manifest.json")).unwrap(),
            )
            .unwrap(),
            environment: vec![],
            work_dir: fixture.work.clone(),
        },
        fixture.descriptor.fence.clone(),
    )
    .unwrap();
    let mut wrong_key = fixture.descriptor.clone();
    wrong_key.public_key = other.descriptor().public_key.clone();
    assert!(NativeGuardian::attach(wrong_key).await.is_err());
    // Early Release has no effect while actual writers are open.
    assert!(fixture.attach().await.release().await.is_err());
    for payload in [
        vec![0xff, 0xff, 0xff, 0xff],
        vec![1, 0, 0, 0, b'x'],
        vec![12, 0, 0, 0, b'{'],
    ] {
        let mut pipe = ClientOptions::new()
            .open(fixture.descriptor.pipe_name())
            .unwrap();
        pipe.write_all(&payload).await.unwrap();
        drop(pipe);
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    assert_eq!(owner.observe().await.unwrap(), GuardianState::Running);
    assert_writers_locked(&fixture.work, &writers);
    assert_ne!(owner.stop().await.unwrap(), GuardianState::Running);
    closed(&owner).await;
    assert_writers_closed(&fixture.work, &writers).await;
    assert!(fixture.parent.try_wait().unwrap().is_none());
    owner.release().await.unwrap();
    fixture.wait_helper_exit().await;
    fixture.wait_clean_parent_exit().await;
    fixture.report(
        "invalid-clients",
        &writers,
        json!({"wrong_fence_rejected":true,
        "wrong_key_rejected":true,"early_release_rejected":true,"invalid_frames_rejected":3,
        "stop_with_parent_alive":true,"closed_native_file_handles":3}),
    );
}

#[tokio::test]
async fn supervisor_caido_y_descriptor_guardado_no_se_convierten_en_prueba() {
    let mut fixture = Fixture::start().await;
    let owner = fixture.attach().await;
    let writers = fixture.writers().await;
    assert!(fixture.helper.process(fixture.helper_pid).unwrap().kill());
    let deadline = Instant::now() + Duration::from_secs(5);
    while owner.observe().await.is_ok() {
        assert!(Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    assert!(owner.require_closed().await.is_err());
    assert!(NativeGuardian::attach(fixture.descriptor.clone())
        .await
        .is_err());
    // Kill-on-close eventually releases real files, yet absence alone remains
    // insufficient to manufacture a native witness for journal cleanup.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if writers.iter().all(|writer| {
            std::fs::OpenOptions::new()
                .write(true)
                .share_mode(0)
                .open(fixture.work.join(writer["path"].as_str().unwrap()))
                .is_ok()
        }) {
            break;
        }
        assert!(Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    assert!(owner.require_closed().await.is_err());
    assert_writers_closed(&fixture.work, &writers).await;
    fixture.report(
        "guardian-crash",
        &writers,
        json!({"guardian_crashed":true,
        "descriptor_rejected_after_crash":true,"closure_proof_unavailable":true,
        "closed_native_file_handles":3}),
    );
}
