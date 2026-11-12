//! Real SQLite, local Windows files and the actual P012 probe child. Catalogue
//! origin/verified state is seeded; no adapter qualification/conversion claimed.
#![cfg(windows)]
#![allow(clippy::unwrap_used, clippy::panic)]
use larm_db::{
    locations::SqlStorageLocations, pool::Db, transformation_inputs::SqlTransformationInputs,
};
use larm_domain::{
    artifacts::below_mount,
    bridge::timestamp,
    estimate::{Confidence, MarginPolicy, StorageEstimate, VolumeEstimate},
    model::{bundle_hash, required_hash, BundleEntry},
    ports::StorageProber,
    storage::{StorageObservation, StorageVolume},
    transformations::{
        seal_bound_plan, PlanDraft, PlanStep, SealedTransformationPlan,
        TransformationExecutionBinding, TransformationInputGuard, TransformationRequest,
        TransformationSpaceObserver,
    },
    units::DecimalU64,
};
use larm_hardware::{devices::NativeCommand, storage::NativeStorageProber};
use larm_storage::estimate::SpacePolicy;
use larm_transform::environment::{
    TransformationEnvironment, TransformationSpacePolicy, WorkspaceRoot,
};
use sha2::{Digest, Sha256};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

fn id() -> String {
    uuid::Uuid::now_v7().to_string()
}
fn bytes(n: u64) -> DecimalU64 {
    DecimalU64::new(n).unwrap()
}
fn hash(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

#[derive(Clone, Copy, Default)]
enum Fault {
    #[default]
    None,
    Stale,
    Duplicate,
    ReadOnly,
    UnknownFree,
    ChangedLetters,
}
struct Probe {
    native: NativeStorageProber,
    fault: Mutex<Fault>,
}
#[async_trait::async_trait]
impl StorageProber for Probe {
    async fn probe(
        &self,
        unc: &[String],
    ) -> Result<StorageObservation, larm_domain::error::LarmError> {
        let mut observed = self.native.probe(unc).await?;
        match *self.fault.lock().unwrap() {
            Fault::None => {}
            Fault::Stale => {
                observed.observed_at = timestamp(chrono::Utc::now() - chrono::Duration::seconds(6))
            }
            Fault::Duplicate => observed.volumes.extend(observed.volumes.clone()),
            Fault::ReadOnly => {
                for v in &mut observed.volumes {
                    v.read_only = true;
                }
            }
            Fault::UnknownFree => {
                for v in &mut observed.volumes {
                    v.free_bytes = None;
                }
            }
            Fault::ChangedLetters => {
                for v in &mut observed.volumes {
                    if v.identity.volume_guid.is_some() {
                        v.identity.mount_points = vec!["Y:\\".into()];
                    }
                }
            }
        }
        Ok(observed)
    }
}
struct Policy(Mutex<SpacePolicy>);
#[async_trait::async_trait]
impl TransformationSpacePolicy for Policy {
    async fn current(&self) -> Result<SpacePolicy, larm_domain::error::LarmError> {
        Ok(*self.0.lock().unwrap())
    }
}

struct Fixture {
    env: TransformationEnvironment,
    db: Db,
    reader: sqlx::SqlitePool,
    probe: Arc<Probe>,
    policy: Arc<Policy>,
    directory: tempfile::TempDir,
    source: PathBuf,
    destination: PathBuf,
    work: PathBuf,
    variant: String,
    artifact: String,
    bundle: String,
    source_id: String,
    destination_id: String,
    volume: StorageVolume,
}
impl Fixture {
    async fn new() -> Self {
        let (directory, db) = larm_db::testing::temp_migrated().await.unwrap();
        let source = directory.path().join("origen 模型");
        let destination = directory.path().join("destino");
        let work = directory.path().join("worker");
        for root in [&source, &destination, &work] {
            std::fs::create_dir(root).unwrap();
        }
        std::fs::write(source.join("blob.bin"), b"fixture model bytes").unwrap();
        let probe = Arc::new(Probe {
            native: NativeStorageProber {
                command: NativeCommand {
                    program: env!("CARGO_BIN_EXE_larm-daemon").into(),
                    args: vec!["probe-storage".into()],
                    timeout: Duration::from_secs(30),
                },
            },
            fault: Mutex::new(Fault::None),
        });
        let observed = probe.probe(&[]).await.unwrap();
        let root = source.to_str().unwrap();
        let kind = larm_storage::locations::check_syntax(root).unwrap();
        let volume = larm_storage::locations::volume_for(&observed, root, &kind)
            .unwrap()
            .clone();
        assert!(
            volume.online && volume.identity.volume_guid.is_some() && volume.free_bytes.is_some()
        );
        let variant = id();
        let artifact = id();
        let bundle = id();
        let revision = id();
        let source_id = id();
        let destination_id = id();
        let at = timestamp(chrono::Utc::now());
        for sql in [
            "INSERT INTO canonical_model(id,created_at,updated_at,row_version,metadata,name,capabilities,possible_duplicates) VALUES('cm','t','t',1,'{}','Fixture','[]','[]')",
            "INSERT INTO adapter_installation(id,created_at,updated_at,row_version,metadata,adapter_id,version,family,package_hash,publisher,trust,manifest,state) VALUES('source-install','t','t',1,'{}','fixture.source','1.0.0','source','fixture','fixture','UNTRUSTED','{}','INSTALLED')",
            "INSERT INTO model_source_adapter(id,created_at,updated_at,row_version,metadata,adapter_installation_id,source_type,capabilities) VALUES('source-adapter','t','t',1,'{}','source-install','LOCAL','[]')",
            "INSERT INTO model_source(id,created_at,updated_at,row_version,metadata,name,adapter_id,base_url,priority,enabled,auth_mode,policy) VALUES('source','t','t',1,'{}','Fixture','source-adapter','file:///fixture',0,1,'NONE','{}')",
            "INSERT INTO model_distribution(id,created_at,updated_at,row_version,metadata,canonical_model_id,source_id,upstream_namespace,upstream_model_id) VALUES('distribution','t','t',1,'{}','cm','source','fixture','model')",
        ] {sqlx::query(sql).execute(&db.pool).await.unwrap();}
        sqlx::query("INSERT INTO model_revision(id,created_at,updated_at,row_version,metadata,distribution_id,revision_key,manifest_hash,state) VALUES(?,?,?,1,'{}','distribution','r1',?,'CURRENT')")
            .bind(&revision).bind(&at).bind(&at).bind("a".repeat(64)).execute(&db.pool).await.unwrap();
        let limits = serde_json::to_value(larm_domain::model::ModelLimits::default()).unwrap();
        sqlx::query("INSERT INTO model_variant(id,created_at,updated_at,row_version,metadata,revision_id,status,lifecycle,availability,ownership,promotion,system_role,capabilities,model_limits,update_available,bundle_hash) VALUES(?,?,?,1,'{}',?,'VERIFIED','VERIFIED','UNKNOWN','LARM_MANAGED','NONE','GENERAL','[]',?,0,?)")
            .bind(&variant).bind(&at).bind(&at).bind(&revision).bind(limits.to_string()).bind("a".repeat(64)).execute(&db.pool).await.unwrap();
        let entry = BundleEntry {
            relative_path: "model.bin".into(),
            sha256: hash(b"fixture model bytes"),
            size_bytes: 19,
            required: true,
        };
        sqlx::query("INSERT INTO model_bundle(id,created_at,updated_at,row_version,metadata,variant_id,required_artifact_ids,optional_artifact_ids,manifest_hash,state) VALUES(?,?,?,1,'{}',?,?,'[]',?,'VERIFIED')")
            .bind(&bundle).bind(&at).bind(&at).bind(&variant).bind(serde_json::json!([artifact]).to_string()).bind(bundle_hash(std::slice::from_ref(&entry)).unwrap()).execute(&db.pool).await.unwrap();
        sqlx::query("UPDATE model_variant SET bundle_hash=? WHERE id=?")
            .bind(required_hash(std::slice::from_ref(&entry)).unwrap())
            .bind(&variant)
            .execute(&db.pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO model_artifact(id,created_at,updated_at,row_version,metadata,variant_id,filename,expected_size,actual_size,sha256,required) VALUES(?,?,?,1,'{}',?,'model.bin',19,19,?,1)")
            .bind(&artifact).bind(&at).bind(&at).bind(&variant).bind(&entry.sha256).execute(&db.pool).await.unwrap();
        for (root, key, reserved) in [
            (&source, &source_id, 7_i64),
            (&destination, &destination_id, 11_i64),
        ] {
            let inside = volume
                .identity
                .mount_points
                .iter()
                .filter_map(|m| below_mount(m, root.to_str().unwrap()).map(|p| (m.len(), p)))
                .max_by_key(|(n, _)| *n)
                .unwrap()
                .1;
            let metadata = serde_json::json!({"volume_relative_path":inside});
            sqlx::query("INSERT INTO storage_location(id,created_at,updated_at,row_version,metadata,name,root_path,canonical_path,volume_guid,filesystem,storage_type,capacity_bytes,free_bytes,reserved_bytes,priority,auto_placement,state,volume_key,performance_class) VALUES(?,?,?,1,?,'Fixture',?,?,?,?, 'LOCAL',?,?,?,0,1,'ONLINE',?,'UNKNOWN')")
                .bind(key).bind(&at).bind(&at).bind(metadata.to_string()).bind(root.to_str().unwrap()).bind(root.to_str().unwrap()).bind(&volume.identity.volume_guid).bind(volume.filesystem.as_deref().unwrap())
                .bind(volume.capacity_bytes.unwrap().get() as i64).bind(volume.free_bytes.unwrap().get() as i64).bind(reserved).bind(volume.identity.volume_key.as_deref().unwrap()).execute(&db.pool).await.unwrap();
        }
        sqlx::query("INSERT INTO artifact_location(id,created_at,updated_at,row_version,metadata,artifact_id,storage_location_id,relative_path,file_size,sha256,state,ownership,verified_at) VALUES(?,?,?,1,'{}',?,?,'blob.bin',19,?,'ONLINE','LARM_MANAGED',?)")
            .bind(id()).bind(&at).bind(&at).bind(&artifact).bind(&source_id).bind(&entry.sha256).bind(&at).execute(&db.pool).await.unwrap();
        let reader = db.qualification_reader();
        let policy = Arc::new(Policy(Mutex::new(SpacePolicy {
            margin: MarginPolicy {
                minimum_margin_bytes: 64,
                margin_basis_points: 0,
            },
            minimum_free_bytes: 0,
        })));
        let env = TransformationEnvironment {
            catalogue: SqlTransformationInputs {
                pool: reader.clone(),
            },
            locations: SqlStorageLocations {
                pool: reader.clone(),
            },
            prober: probe.clone(),
            policy: policy.clone(),
        };
        Self {
            env,
            db,
            reader,
            probe,
            policy,
            directory,
            source,
            destination,
            work,
            variant,
            artifact,
            bundle,
            source_id,
            destination_id,
            volume,
        }
    }
    async fn plan(&self) -> SealedTransformationPlan {
        let catalogue = self.env.catalogue.load(&self.variant).await.unwrap();
        let estimate = StorageEstimate {
            download_bytes: None,
            final_artifact_bytes: Some(bytes(8)),
            installed_bytes: None,
            derived_cache_bytes: None,
            temporary_peak_bytes: Some(bytes(8)),
            existing_bytes: Some(bytes(19)),
            peak_extra_bytes: Some(bytes(16)),
            required_free_bytes: Some(bytes(80)),
            upper_bound_bytes: Some(bytes(16)),
            confidence: Confidence::Estimated,
            reason: "fixture allocations, not a tool estimate".into(),
            evidence_refs: vec!["fixture:allocation".into()],
            per_volume: vec![VolumeEstimate {
                volume_key: self.volume.identity.volume_key.clone().unwrap(),
                peak_extra_bytes: bytes(16),
                margin_bytes: bytes(64),
                other_reservations_bytes: bytes(0),
                usable_free_bytes: bytes(1000),
                eligible: true,
                reason: "fixture, admission must remeasure".into(),
            }],
        };
        seal_bound_plan(
            id(),
            TransformationRequest {
                input_variant_id: self.variant.clone(),
                adapter_id: "fixture.transform".into(),
                tool_version: "1.0.0".into(),
                target_format: "BASE64".into(),
                parameters: serde_json::json!({}),
                storage_location_id: self.destination_id.clone(),
                hard_budget_bytes: None,
            },
            catalogue.artifacts,
            PlanDraft {
                operation: "transformation.execute".into(),
                resource_ids: vec![self.variant.clone(), self.destination_id.clone()],
                storage_estimate: estimate,
                steps: vec![PlanStep {
                    phase: "fixture".into(),
                    adapter_id: None,
                    request: serde_json::json!({}),
                }],
                expires_at: timestamp(chrono::Utc::now() + chrono::Duration::minutes(15)),
                approval_required: false,
            },
            TransformationExecutionBinding {
                adapter_installation_id: id(),
                conformance_run_id: id(),
                conformance_run_sha256: "a".repeat(64),
                profile_sha256: "b".repeat(64),
                worker_plan_id: id(),
                worker_plan_hash: "c".repeat(64),
            },
        )
        .unwrap()
    }
    fn extra(&self) -> Vec<WorkspaceRoot> {
        vec![WorkspaceRoot {
            path: self.work.clone(),
            volume_key: self.volume.identity.volume_key.clone().unwrap(),
        }]
    }
    async fn effects(&self) -> (i64, i64, i64) {
        sqlx::query_as("SELECT (SELECT count(*) FROM transformation_job),(SELECT count(*) FROM volume_reservation),(SELECT count(*) FROM resource_lock)").fetch_one(&self.db.pool).await.unwrap()
    }
    async fn add_artifact(&self, name: &str, required: bool, with_copy: bool) -> String {
        let artifact = id();
        let at = timestamp(chrono::Utc::now());
        sqlx::query("INSERT INTO model_artifact(id,created_at,updated_at,row_version,metadata,variant_id,filename,expected_size,actual_size,sha256,required) VALUES(?,?,?,1,'{}',?,?,19,19,?,?)")
            .bind(&artifact).bind(&at).bind(&at).bind(&self.variant).bind(name).bind(hash(b"fixture model bytes")).bind(required).execute(&self.db.pool).await.unwrap();
        let rows:Vec<(String,String,bool)>=sqlx::query_as("SELECT id,filename,required FROM model_artifact WHERE variant_id=? ORDER BY filename,id").bind(&self.variant).fetch_all(&self.db.pool).await.unwrap();
        let entries = rows
            .iter()
            .map(|(_, name, required)| BundleEntry {
                relative_path: name.clone(),
                sha256: hash(b"fixture model bytes"),
                size_bytes: 19,
                required: *required,
            })
            .collect::<Vec<_>>();
        let required_ids = rows
            .iter()
            .filter(|(_, _, r)| *r)
            .map(|(id, _, _)| id)
            .collect::<Vec<_>>();
        let optional_ids = rows
            .iter()
            .filter(|(_, _, r)| !*r)
            .map(|(id, _, _)| id)
            .collect::<Vec<_>>();
        sqlx::query("UPDATE model_bundle SET required_artifact_ids=?,optional_artifact_ids=?,manifest_hash=? WHERE id=?")
            .bind(serde_json::to_string(&required_ids).unwrap()).bind(serde_json::to_string(&optional_ids).unwrap()).bind(bundle_hash(&entries).unwrap()).bind(&self.bundle).execute(&self.db.pool).await.unwrap();
        sqlx::query("UPDATE model_variant SET bundle_hash=? WHERE id=?")
            .bind(required_hash(&entries).unwrap())
            .bind(&self.variant)
            .execute(&self.db.pool)
            .await
            .unwrap();
        if with_copy {
            self.add_copy(&artifact, "blob.bin").await;
        }
        artifact
    }
    async fn add_copy(&self, artifact: &str, path: &str) {
        let at = timestamp(chrono::Utc::now());
        sqlx::query("INSERT INTO artifact_location(id,created_at,updated_at,row_version,metadata,artifact_id,storage_location_id,relative_path,file_size,sha256,state,ownership,verified_at) VALUES(?,?,?,1,'{}',?,?,?,19,?,'ONLINE','LARM_MANAGED',?)")
            .bind(id()).bind(&at).bind(&at).bind(artifact).bind(&self.source_id).bind(path).bind(hash(b"fixture model bytes")).bind(&at).execute(&self.db.pool).await.unwrap();
    }
    async fn close(self) {
        self.reader.close().await;
        self.db.pool.close().await;
        self.directory.close().unwrap();
    }
}

#[tokio::test]
async fn native_volume_catalogue_and_real_files_are_read_and_protected_without_admission() {
    let f = Fixture::new().await;
    let input = f.env.input(&f.variant, None).await.unwrap();
    assert_eq!(input.artifacts()[0].sha256, hash(b"fixture model bytes"));
    assert_eq!(input.identity().bundle_id, f.bundle);
    assert_eq!(input.artifacts()[0].artifact_id, f.artifact);
    assert_eq!(input.locations()[0].storage_location_id, f.source_id);
    input.recheck().await.unwrap();
    assert!(std::fs::write(f.source.join("blob.bin"), b"tampered").is_err());
    assert_eq!(f.effects().await, (0, 0, 0));
    drop(input);
    std::fs::write(f.source.join("blob.bin"), b"released").unwrap();
    f.close().await;
}

#[tokio::test]
async fn volume_guid_paths_survive_an_injected_drive_letter_change() {
    let f = Fixture::new().await;
    *f.probe.fault.lock().unwrap() = Fault::ChangedLetters;
    let input = f.env.input(&f.variant, None).await.unwrap();
    input.recheck().await.unwrap();
    assert_eq!(input.artifacts()[0].sha256, hash(b"fixture model bytes"));
    drop(input);
    f.close().await;
}

#[tokio::test]
async fn live_space_uses_native_free_bytes_and_current_policy_and_pins_both_roots() {
    let f = Fixture::new().await;
    let plan = f.plan().await;
    let space = f.env.space(&plan, f.extra()).await.unwrap();
    let snapshot = space.observe().await.unwrap();
    assert_eq!(snapshot.demands.len(), 1);
    assert_eq!(snapshot.demands[0].required_bytes, 80);
    assert_eq!(snapshot.demands[0].policy_minimum, 11);
    assert!(snapshot.demands[0].os_free > 1000); // Preview deliberately claimed only1000.
    assert!(std::fs::rename(&f.destination, f.directory.path().join("moved")).is_err());
    assert!(std::fs::rename(&f.work, f.directory.path().join("moved-worker")).is_err());
    f.policy.0.lock().unwrap().minimum_free_bytes = 1024;
    assert_eq!(
        space.observe().await.unwrap().demands[0].policy_minimum,
        1024
    );
    f.policy.0.lock().unwrap().margin.minimum_margin_bytes = 65;
    assert!(space.observe().await.is_err());
    assert_eq!(f.effects().await, (0, 0, 0));
    drop(space);
    std::fs::rename(&f.destination, f.directory.path().join("moved")).unwrap();
    f.close().await;
}

#[tokio::test]
async fn stale_ambiguous_readonly_or_unknown_space_is_not_accepted_as_fresh_storage() {
    let f = Fixture::new().await;
    let plan = f.plan().await;
    for fault in [Fault::Stale, Fault::Duplicate, Fault::ReadOnly] {
        *f.probe.fault.lock().unwrap() = fault;
        assert!(f.env.space(&plan, f.extra()).await.is_err());
    }
    *f.probe.fault.lock().unwrap() = Fault::None;
    let space = f.env.space(&plan, f.extra()).await.unwrap();
    *f.probe.fault.lock().unwrap() = Fault::UnknownFree;
    assert!(space.observe().await.is_err());
    assert_eq!(f.effects().await, (0, 0, 0));
    drop(space);
    f.close().await;
}

#[tokio::test]
async fn corrupt_preferred_copy_falls_back_and_distinct_artifacts_share_a_verified_blob() {
    let f = Fixture::new().await;
    std::fs::write(f.source.join("healthy.bin"), b"fixture model bytes").unwrap();
    std::fs::write(f.source.join("blob.bin"), b"corrupt model bytes").unwrap();
    f.add_copy(&f.artifact, "healthy.bin").await;
    let shared = f.add_artifact("config/duplicate.bin", true, false).await;
    f.add_copy(&shared, "healthy.bin").await;
    let input = f.env.input(&f.variant, None).await.unwrap();
    assert_eq!(input.artifacts().len(), 2);
    assert!(input
        .locations()
        .iter()
        .all(|l| l.relative_path == "healthy.bin"));
    assert!(std::fs::write(f.source.join("healthy.bin"), b"cannot mutate").is_err());
    std::fs::write(f.source.join("blob.bin"), b"failed copy released").unwrap();
    input.recheck().await.unwrap();
    drop(input);
    f.close().await;
}

#[tokio::test]
async fn confirmed_selection_does_not_gain_newly_installed_optional_files() {
    let f = Fixture::new().await;
    let optional = f.add_artifact("optional.bin", false, false).await;
    let initial = f.env.input(&f.variant, None).await.unwrap();
    let expected = initial.artifacts().to_vec();
    drop(initial);
    assert_eq!(expected.len(), 1);
    f.add_copy(&optional, "blob.bin").await;
    let confirmed = f.env.input(&f.variant, Some(&expected)).await.unwrap();
    assert_eq!(confirmed.artifacts(), expected);
    drop(confirmed);
    let planning = f.env.input(&f.variant, None).await.unwrap();
    assert_eq!(planning.artifacts().len(), 2);
    drop(planning);
    let mut wrong = expected;
    wrong[0].sha256 = "0".repeat(64);
    assert!(f.env.input(&f.variant, Some(&wrong)).await.is_err());
    f.close().await;
}

#[tokio::test]
async fn missing_or_incoherent_verified_catalogue_does_not_become_authority_from_location_hashes() {
    let f = Fixture::new().await;
    for modification in [
        "UPDATE model_variant SET lifecycle='CORRUPT'",
        "UPDATE model_bundle SET state='INCOMPLETE'",
        "UPDATE model_variant SET bundle_hash='ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff'",
        "UPDATE model_artifact SET sha256=NULL,expected_size=NULL",
    ] {
        let variant:(String,String)=sqlx::query_as("SELECT lifecycle,bundle_hash FROM model_variant WHERE id=?").bind(&f.variant).fetch_one(&f.db.pool).await.unwrap();
        sqlx::query(modification).execute(&f.db.pool).await.unwrap();
        assert!(f.env.catalogue.load(&f.variant).await.is_err(),"{modification}");
        sqlx::query("UPDATE model_variant SET lifecycle=?,bundle_hash=? WHERE id=?").bind(variant.0).bind(variant.1).bind(&f.variant).execute(&f.db.pool).await.unwrap();
        sqlx::query("UPDATE model_bundle SET state='VERIFIED' WHERE id=?").bind(&f.bundle).execute(&f.db.pool).await.unwrap();
        sqlx::query("UPDATE model_artifact SET sha256=?,expected_size=19 WHERE id=?").bind(hash(b"fixture model bytes")).bind(&f.artifact).execute(&f.db.pool).await.unwrap();
    }
    assert_eq!(f.effects().await, (0, 0, 0));
    f.close().await;
}

#[tokio::test]
async fn referenced_artifact_is_usable_only_with_an_explicit_variant_reference() {
    let f = Fixture::new().await;
    let owner = id();
    sqlx::query("INSERT INTO model_variant SELECT update_available,status,?,created_at,updated_at,row_version,metadata,revision_id,format,quantization,architecture,bundle_hash,lifecycle,availability,ownership,promotion,system1_state,system_role,capabilities,model_limits FROM model_variant WHERE id=?")
        .bind(&owner).bind(&f.variant).execute(&f.db.pool).await.unwrap();
    sqlx::query("UPDATE model_artifact SET variant_id=? WHERE id=?")
        .bind(&owner)
        .bind(&f.artifact)
        .execute(&f.db.pool)
        .await
        .unwrap();
    assert!(f.env.catalogue.load(&f.variant).await.is_err());
    sqlx::query("INSERT INTO artifact_reference(artifact_id,variant_id) VALUES(?,?)")
        .bind(&f.artifact)
        .bind(&f.variant)
        .execute(&f.db.pool)
        .await
        .unwrap();
    let input = f.env.input(&f.variant, None).await.unwrap();
    assert_eq!(input.artifacts()[0].artifact_id, f.artifact);
    drop(input);
    // The original owner can mark this file optional. Its role in the
    // consuming bundle remains required and is included in that bundle hash.
    sqlx::query("UPDATE model_artifact SET required=0 WHERE id=?")
        .bind(&f.artifact)
        .execute(&f.db.pool)
        .await
        .unwrap();
    let input = f.env.input(&f.variant, None).await.unwrap();
    assert_eq!(input.artifacts().len(), 1);
    drop(input);
    sqlx::query("UPDATE model_bundle SET manifest_hash=? WHERE id=?")
        .bind("0".repeat(64))
        .bind(&f.bundle)
        .execute(&f.db.pool)
        .await
        .unwrap();
    assert!(f.env.input(&f.variant, None).await.is_err());
    assert_eq!(f.effects().await, (0, 0, 0));
    f.close().await;
}

#[tokio::test]
async fn independent_reader_keeps_input_checks_alive_while_the_normal_pool_is_exhausted() {
    let f = Fixture::new().await;
    let mut writer = f.db.pool.begin().await.unwrap();
    sqlx::query("UPDATE model_variant SET row_version=row_version WHERE 0")
        .execute(&mut *writer)
        .await
        .unwrap();
    let mut occupied = Vec::new();
    for _ in 0..7 {
        occupied.push(f.db.pool.acquire().await.unwrap());
    }
    let input = tokio::time::timeout(Duration::from_secs(15), f.env.input(&f.variant, None))
        .await
        .unwrap()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(15), input.recheck())
        .await
        .unwrap()
        .unwrap();
    drop(input);
    drop(occupied);
    writer.rollback().await.unwrap();
    f.close().await;
}

#[tokio::test]
async fn changed_registered_destination_is_rejected_while_readonly_source_can_still_be_read() {
    let f = Fixture::new().await;
    *f.probe.fault.lock().unwrap() = Fault::ReadOnly;
    let input = f.env.input(&f.variant, None).await.unwrap();
    input.recheck().await.unwrap();
    drop(input);
    *f.probe.fault.lock().unwrap() = Fault::None;
    let plan = f.plan().await;
    let space = f.env.space(&plan, f.extra()).await.unwrap();
    sqlx::query("UPDATE storage_location SET canonical_path=? WHERE id=?")
        .bind(f.work.to_str().unwrap())
        .bind(&f.destination_id)
        .execute(&f.db.pool)
        .await
        .unwrap();
    assert!(space.observe().await.is_err());
    assert_eq!(f.effects().await, (0, 0, 0));
    drop(space);
    f.close().await;
}
