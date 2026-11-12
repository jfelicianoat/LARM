//! P022 real + worker original; no ingesta Core ni cualificación P026 completa.
use super::*;
use larm_adapter_sdk::source::{
    download_spec, SourceAcquisitionArtifactBinding, SourceAcquisitionBinding,
    SourceDownloadChunkRequest, SOURCE_ACQUISITION_SPEC,
};
use larm_domain::model::{bundle_hash, BundleEntry};
use larm_domain::units::DecimalU64;

struct TraceArchive {
    from: PathBuf,
    to: PathBuf,
}

/// Complete P026 precedes this call. All Core model rows come from observed
/// public RPC DTOs through the product P041 preparer/atomic writer.
pub(super) async fn qualified_catalog_ingestion(
    db: &larm_db::pool::Db,
    fixture: &Fixture,
    runner: Arc<larm_adapter_host::conformance::NativeConformanceRunner>,
    installation: &AdapterInstallation,
    run: &larm_domain::conformance::Run,
    ctx: &MutationContext,
) -> Result<Value, larm_domain::error::LarmError> {
    use larm_models::acquisition::{
        SourceCatalogIngestor, SourceCatalogObservation, SourceObservationQualification,
    };
    let identity = uuid::Uuid::now_v7().to_string();
    let output = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/reports/P044_runs/native-qualified-ingestion")
        .join(&identity);
    std::fs::create_dir_all(&output).unwrap();
    let requests = PathBuf::from(fixture.data["requests"].as_str().unwrap());
    let _trace = TraceArchive {
        from: requests.clone(),
        to: output.join("requests.ndjson"),
    };
    let read_trace = || {
        std::fs::read_to_string(&requests)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .collect::<Vec<_>>()
    };
    let before = read_trace();
    let core_before:(i64,i64,i64)=sqlx::query_as("SELECT (SELECT count(*) FROM canonical_model),(SELECT count(*) FROM model_variant),(SELECT count(*) FROM model_artifact)")
        .fetch_one(&db.pool).await.unwrap();
    assert_eq!(core_before, (0, 0, 0));
    let source_id = fixture.data["config"]["source_id"].as_str().unwrap();
    let source_revision: i64 =
        sqlx::query_scalar("SELECT row_version FROM model_source WHERE id=?")
            .bind(source_id)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    let support: SearchSupport =
        serde_json::from_value(fixture.data["config"]["search_support"].clone()).unwrap();
    let worker = runner
        .source_worker(installation, run, source_id, support, ctx)
        .await?;
    let access = Access {
        reads: vec![fixture.work.clone()],
        writes: vec![fixture.work.clone()],
        urls: vec![format!(
            "{}/probe",
            fixture.data["base_url"].as_str().unwrap()
        )],
        ..Default::default()
    };
    let fetched: Result<Vec<SourceCatalogObservation>, larm_domain::error::LarmError> = async {
        let SourceReply::Search(page) = worker
            .call(
                SourceRequest::Search(SourceSearch {
                    source_id: source_id.into(),
                    q: String::new(),
                    capabilities: vec![],
                    cursor: None,
                    limit: 10,
                }),
                &access,
                None,
            )
            .await?
        else {
            return Err(larm_domain::error::LarmError::new(
                larm_domain::error::ErrorCode::ProtocolError,
                "Expected search page",
                "catalog_fixture_reply",
            ));
        };
        assert_eq!(page.items.len(), 6);
        assert!(page.next_cursor.is_none());
        let mut observations = Vec::new();
        for hit in page.items {
            let reference = larm_domain::acquisition::catalog_reference(&hit)?.unwrap();
            assert_eq!(reference.source_id, source_id);
            let SourceReply::GetModel(inspection) = worker
                .call(
                    SourceRequest::GetModel(larm_adapter_sdk::source::UpstreamRef {
                        source_id: reference.source_id.clone(),
                        upstream_namespace: reference.upstream_namespace.clone(),
                        upstream_model_id: reference.upstream_model_id.clone(),
                    }),
                    &access,
                    None,
                )
                .await?
            else {
                return Err(larm_domain::error::LarmError::new(
                    larm_domain::error::ErrorCode::ProtocolError,
                    "Expected inspection",
                    "catalog_fixture_reply",
                ));
            };
            let SourceReply::GetManifest(manifest) = worker
                .call(
                    SourceRequest::GetManifest(larm_adapter_sdk::source::RevisionRef {
                        source_id: reference.source_id.clone(),
                        upstream_namespace: reference.upstream_namespace.clone(),
                        upstream_model_id: reference.upstream_model_id.clone(),
                        revision_key: reference.revision_key.clone(),
                    }),
                    &access,
                    None,
                )
                .await?
            else {
                return Err(larm_domain::error::LarmError::new(
                    larm_domain::error::ErrorCode::ProtocolError,
                    "Expected manifest",
                    "catalog_fixture_reply",
                ));
            };
            observations.push(SourceCatalogObservation {
                source_revision,
                id: uuid::Uuid::now_v7().to_string(),
                observed_at: timestamp(chrono::Utc::now()),
                reference,
                hit,
                inspection,
                manifest,
                qualification: SourceObservationQualification {
                    adapter_installation_id: installation.id.clone(),
                    adapter_installation_revision: installation.row_version.to_string(),
                    adapter_installation_sha256:
                        larm_domain::conformance::committed_installation_hash(installation)?,
                    conformance_run_id: run.id.clone(),
                    conformance_run_sha256: run.digest()?,
                },
            });
        }
        Ok(observations)
    }
    .await;
    let closed = worker.close().await;
    drop(worker);
    closed?;
    let observations = fetched?;
    let ingestor = SourceCatalogIngestor {
        catalog: larm_db::catalog_acquisition::SqlCatalogAcquisitions {
            pool: db.pool.clone(),
            qualification: Some(runner.clone()),
        },
    };
    let private_ids = observations
        .iter()
        .map(|o| o.hit.id.clone())
        .collect::<std::collections::BTreeSet<_>>();
    let mut registered = Vec::new();
    for observation in &observations {
        let result = ingestor.ingest(observation, ctx).await?;
        assert!(!private_ids.contains(&result.variant.id));
        assert_eq!(result.variant.lifecycle, "DISCOVERED");
        assert_eq!(result.variant.availability, "UNKNOWN");
        assert!(result.variant.bundle_hash.is_none());
        registered.push(result);
    }
    let event_counts_before: (i64, i64) =
        sqlx::query_as("SELECT (SELECT count(*) FROM event),(SELECT count(*) FROM event_outbox)")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    let replay = ingestor.ingest(&observations[0], ctx).await?;
    assert_eq!(replay.manifest, registered[0].manifest);
    let event_counts_after: (i64, i64) =
        sqlx::query_as("SELECT (SELECT count(*) FROM event),(SELECT count(*) FROM event_outbox)")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(event_counts_before, event_counts_after);
    let models: i64 = sqlx::query_scalar("SELECT count(*) FROM canonical_model")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    let core:(i64,i64,i64,i64,i64,i64,i64,i64)=sqlx::query_as("SELECT (SELECT count(*) FROM model_variant),(SELECT count(*) FROM model_artifact),(SELECT count(*) FROM model_acquisition_manifest),(SELECT count(*) FROM model_bundle),(SELECT count(*) FROM artifact_location),(SELECT count(*) FROM download_job),(SELECT count(*) FROM volume_reservation),(SELECT count(*) FROM artifact_provenance)")
        .fetch_one(&db.pool).await.unwrap();
    assert_eq!(core, (6, 6, 6, 0, 0, 0, 0, 0));
    let after = read_trace();
    assert!(after[before.len()..]
        .iter()
        .all(|row| !row["path"].as_str().unwrap().starts_with("/artifact/")));
    let result = json!({"id":identity,"scope":"qualified original source RPC -> product P041 identity resolution -> atomic SQLite graph/snapshot; no synthetic model IDs or qualification, no first Core download or GUI",
        "source_id":source_id,"observed_variants":observations.len(),"canonical_models":models,
        "central_variant_ids":registered.iter().map(|r|&r.variant.id).collect::<Vec<_>>(),
        "private_variant_ids":private_ids,"core_variants_artifacts_snapshots_bundles_locations_jobs_reservations_provenance":core,
        "replay_events_unchanged":true,"metadata_request_trace_start":before.len(),"metadata_request_trace_end":after.len(),
        "no_artifact_reads_during_metadata_ingestion":true,"public_reference_ids_preserved":true});
    std::fs::write(
        output.join("result.json"),
        serde_json::to_vec_pretty(&result).unwrap(),
    )
    .unwrap();
    Ok(result)
}
impl Drop for TraceArchive {
    fn drop(&mut self) {
        let _ = std::fs::copy(&self.from, &self.to);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn public_binding_native_selects_core_uuid_optionals_and_bounded_bytes() {
    let mut fixture = Fixture::new("source-conformance");
    // Fixture público de seis variantes; añadimos un opcional al primer
    // manifiesto antes del grant y de abrir el worker.
    let catalog_path = fixture.work.join("catalog/catalog.json");
    let mut catalog: Value =
        serde_json::from_slice(&std::fs::read(&catalog_path).unwrap()).unwrap();
    let first = &mut catalog["entries"][0];
    let mut optional = first["inspection"]["artifacts"][0].clone();
    optional["relative_path"] = json!("optional.bin");
    optional["required"] = json!(false);
    optional["role"] = json!("other");
    first["inspection"]["artifacts"]
        .as_array_mut()
        .unwrap()
        .push(optional);
    let mut download = first["downloads"][0].clone();
    download["relative_path"] = json!("optional.bin");
    first["downloads"].as_array_mut().unwrap().push(download);
    let entries = first["inspection"]["artifacts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| BundleEntry {
            relative_path: a["relative_path"].as_str().unwrap().into(),
            sha256: a["sha256"].as_str().unwrap().into(),
            size_bytes: a["size_bytes"].as_str().unwrap().parse().unwrap(),
            required: a["required"].as_bool().unwrap(),
        })
        .collect::<Vec<_>>();
    let hash = bundle_hash(&entries).unwrap();
    first["revision"]["manifest_hash"] = json!(hash);
    first["revision"]["revision_key"] = json!(format!("content:{hash}"));
    let upstream_model = first["upstream_model_id"].as_str().unwrap().to_owned();
    std::fs::write(&catalog_path, serde_json::to_vec_pretty(&catalog).unwrap()).unwrap();
    fixture.install_source_candidate().await;
    let id = uuid::Uuid::now_v7().to_string();
    let output = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/reports/P044_runs/source-binding-native")
        .join(&id);
    std::fs::create_dir_all(&output).unwrap();
    let _trace = TraceArchive {
        from: PathBuf::from(fixture.data["requests"].as_str().unwrap()),
        to: output.join("requests.ndjson"),
    };
    let guard = fixture.start().await;
    let source_id = fixture.data["config"]["source_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let support: SearchSupport =
        serde_json::from_value(fixture.data["config"]["search_support"].clone()).unwrap();
    let source = SourceWorker::new(guard, source_id.clone(), support).unwrap();
    let access = Access {
        reads: vec![fixture.work.clone()],
        writes: vec![fixture.work.clone()],
        urls: vec![
            format!("{}/probe", fixture.data["base_url"].as_str().unwrap()),
            format!("{}/artifact/0", fixture.data["base_url"].as_str().unwrap()),
        ],
        ..Access::default()
    };
    let check:Result<Value,Box<dyn std::error::Error>>=async {
        let SourceReply::Search(page)=source.call(SourceRequest::Search(SourceSearch{
            source_id:source_id.clone(),q:String::new(),capabilities:vec![],cursor:None,limit:10,
        }),&access,None).await? else {return Err(std::io::Error::other("ModelPage").into());};
        assert_eq!(page.items.len(),6);
        let (item,reference)=page.items.iter().filter_map(|item|
            larm_domain::acquisition::catalog_reference(item).ok().flatten().map(|reference|(item,reference)))
            .find(|(_,reference)|reference.upstream_model_id==upstream_model).unwrap();
        let revision=larm_adapter_sdk::source::RevisionRef{
            source_id:reference.source_id.clone(),upstream_namespace:reference.upstream_namespace.clone(),
            upstream_model_id:reference.upstream_model_id.clone(),revision_key:reference.revision_key.clone(),
        };
        let SourceReply::GetManifest(manifest)=source.call(SourceRequest::GetManifest(revision.clone()),&access,None).await?
        else {return Err(std::io::Error::other("ArtifactManifest").into());};
        let mut bindings=manifest.artifacts.iter().map(|a|SourceAcquisitionArtifactBinding{
            artifact_id:uuid::Uuid::now_v7().to_string(),relative_path:a.relative_path.clone(),required:a.required,
        }).collect::<Vec<_>>();
        bindings.sort_by(|a,b|a.relative_path.cmp(&b.relative_path));
        let optional_id=bindings.iter().find(|a|!a.required).unwrap().artifact_id.clone();
        let central_id=uuid::Uuid::now_v7().to_string();
        let central_storage_id=uuid::Uuid::now_v7().to_string();
        assert_ne!(central_id,item.id);
        let request=DownloadPlanRequest{variant_id:central_id.clone(),
            storage_location_id:central_storage_id.clone(),
            source_id:Some(source_id.clone()),priority:DownloadPriority::Normal,hard_budget_bytes:None,
            optional_artifact_ids:Some(vec![]),
            acquisition:Some(SourceAcquisitionBinding{revision:reference,manifest_hash:manifest.manifest_hash,artifacts:bindings})};
        let SourceReply::DownloadPlan(required)=source.call(SourceRequest::DownloadPlan(request.clone()),&access,None).await?
        else {return Err(std::io::Error::other("required plan").into());};
        let spec=download_spec(&required)?;
        assert_eq!(spec.variant_id,central_id);
        assert_eq!(spec.storage_location_id,central_storage_id);
        assert!(required.resource_ids.contains(&central_storage_id));
        assert_eq!(spec.artifacts.len(),1);
        assert_eq!(required.steps[0].request[SOURCE_ACQUISITION_SPEC],json!(request.acquisition));
        let mut include=request.clone();include.optional_artifact_ids=Some(vec![optional_id]);
        let SourceReply::DownloadPlan(with_optional)=source.call(SourceRequest::DownloadPlan(include),&access,None).await?
        else {return Err(std::io::Error::other("optional plan").into());};
        assert_eq!(download_spec(&with_optional)?.artifacts.len(),2);
        let required_bytes=spec.artifacts[0].size_bytes.unwrap().get();
        assert_eq!(required.storage_estimate.download_bytes.unwrap().get(),required_bytes);
        assert_eq!(with_optional.storage_estimate.download_bytes.unwrap().get(),required_bytes*2);
        let private_execution=source.call(SourceRequest::Download(ExecutePlan {
            plan_id:required.plan_id.clone(),plan_hash:required.plan_hash.clone(),approval_id:None,
        }),&access,None).await.unwrap_err();
        assert!(private_execution.details.resource_ids.iter().any(|s|s.contains("reference_private_execution_storage_not_bound"))
            || private_execution.details.reason=="reference_private_execution_storage_not_bound");
        let mut legacy=request.clone();legacy.acquisition=None;legacy.variant_id=item.id.clone();
        assert!(source.call(SourceRequest::DownloadPlan(legacy),&access,None).await.is_err());
        let mut changed=request;changed.acquisition.as_mut().unwrap().manifest_hash="d".repeat(64);
        assert!(source.call(SourceRequest::DownloadPlan(changed),&access,None).await.is_err());
        let trace=std::fs::read_to_string(fixture.data["requests"].as_str().unwrap())?;
        assert!(!trace.lines().any(|line|serde_json::from_str::<Value>(line).unwrap()["path"].as_str().unwrap().starts_with("/artifact/")));
        let template=SourceDownloadChunkRequest{
            source_id:source_id.clone(),job_id:uuid::Uuid::now_v7().to_string(),
            plan_id:required.plan_id.clone(),plan_hash:required.plan_hash.clone(),approval_id:None,
            revision,relative_path:spec.artifacts[0].relative_path.clone(),
            offset:DecimalU64::new(0).unwrap(),max_bytes:DecimalU64::new(524288).unwrap(),validator:None,
        };
        let mut rejected=template.clone();rejected.relative_path="optional.bin".into();
        assert!(source.call(SourceRequest::DownloadChunk(rejected),&access,None).await.is_err());
        let mut bytes=Vec::new();let mut validator=None;let mut calls=0;
        loop {
            let mut request=template.clone();request.offset=DecimalU64::new(bytes.len() as u64).unwrap();request.validator=validator;
            let SourceReply::DownloadChunk(chunk)=source.call(SourceRequest::DownloadChunk(request),&access,None).await?
            else {return Err(std::io::Error::other("chunk").into());};
            let data=chunk.decoded_bytes()?;assert!(data.len()<=524288);
            bytes.extend(data);validator=chunk.validator;calls+=1;
            if chunk.eof {break;}
            assert!(calls<100);
        }
        let actual=hex::encode(Sha256::digest(&bytes));
        assert_eq!(Some(actual.as_str()),spec.artifacts[0].sha256.as_deref());
        assert_eq!(bytes.len() as u64,required_bytes);
        assert_eq!(std::fs::read_dir(fixture.work.join("storage"))?.count(),0);
        let db=larm_db::pool::Db::open(&fixture._root.path().join("data/state/larm.sqlite")).await?;
        let core:(i64,i64,i64,i64)=sqlx::query_as("SELECT (SELECT count(*) FROM canonical_model),(SELECT count(*) FROM model_artifact),(SELECT count(*) FROM download_job),(SELECT count(*) FROM volume_reservation)").fetch_one(&db.pool).await?;
        assert_eq!(core,(0,0,0,0));db.pool.close().await;
        Ok(json!({"id":id,"scope":"original reference worker with native P022 and six public variants; explicit fixture catalog optional; central UUID/reference/selection and bounded HTTP bytes; no Core ingestion, P026 full qualification or GUI acceptance",
            "central_variant_id":central_id,"central_storage_id":central_storage_id,"private_variant_id":item.id,"variants":page.items.len(),
            "private_execution_rejects_core_destination":true,"legacy_plan_rejects_unbound_destination":true,
            "required_artifacts":1,"with_optional_artifacts":2,"no_artifact_reads_during_plan":true,
            "unselected_optional_rejected":true,"bytes":bytes.len().to_string(),"sha256":actual,"chunk_calls":calls,
            "core_models_artifacts_jobs_reservations":core,"model_files_written":0}))
    }.await;
    source.stop().await;
    let result = check.unwrap();
    std::fs::write(
        output.join("result.json"),
        serde_json::to_vec_pretty(&result).unwrap(),
    )
    .unwrap();
    std::fs::write(
        output.parent().unwrap().with_extension("json"),
        serde_json::to_vec_pretty(&result).unwrap(),
    )
    .unwrap();
}
