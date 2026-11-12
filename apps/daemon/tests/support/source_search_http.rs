//! Native P026 plus actual HTTP router/product provider/P041; owned fixture only.
use super::*;
use axum::{body::Body, http::Request, Router};
use tower::ServiceExt;

async fn post(
    app: &Router,
    token: &str,
    correlation: &str,
    key: &str,
    request: &Value,
) -> (u16, Value) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/models/search")
                .header("authorization", format!("Bearer {token}"))
                .header("x-correlation-id", correlation)
                .header("content-type", "application/json")
                .header("idempotency-key", key)
                .body(Body::from(request.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status().as_u16();
    let value = serde_json::from_slice(
        &axum::body::to_bytes(response.into_body(), 1 << 20)
            .await
            .unwrap(),
    )
    .unwrap();
    (status, value)
}
async fn counts(db: &larm_db::pool::Db) -> Value {
    let mut counts = serde_json::Map::new();
    for table in [
        "canonical_model",
        "model_variant",
        "model_artifact",
        "model_acquisition_manifest",
        "model_bundle",
        "artifact_location",
        "download_job",
        "volume_reservation",
        "artifact_provenance",
        "event",
        "event_outbox",
    ] {
        let n: i64 =
            sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT count(*) FROM {table}")))
                .fetch_one(&db.pool)
                .await
                .unwrap();
        counts.insert(table.into(), json!(n));
    }
    Value::Object(counts)
}
struct TraceArchive {
    from: PathBuf,
    to: PathBuf,
}
impl Drop for TraceArchive {
    fn drop(&mut self) {
        let _ = std::fs::copy(&self.from, &self.to);
    }
}

// Observe the real provider without replacing RPC, identity resolution or
// persistence. Drop records cancellation when the engine's source deadline
// discards an in-flight page, so a generic HTTP error retains its root cause.
struct PageTrace {
    started: std::time::Instant,
    row: Value,
    records: Arc<std::sync::Mutex<Vec<Value>>>,
}
impl Drop for PageTrace {
    fn drop(&mut self) {
        self.row["elapsed_ms"] = json!(self.started.elapsed().as_millis());
        self.records.lock().unwrap().push(self.row.clone());
    }
}
struct ObservedProvider {
    inner: qualified_source_search::QualifiedSourceSearch,
    records: Arc<std::sync::Mutex<Vec<Value>>>,
}
#[async_trait::async_trait]
impl larm_models::search::FederatedSearchProvider for ObservedProvider {
    async fn sources(
        &self,
        ids: &[String],
    ) -> Result<Vec<larm_models::search::SearchSource>, larm_domain::error::LarmError> {
        self.inner.sources(ids).await
    }
    async fn search_page(
        &self,
        id: &str,
        request: SourceSearch,
    ) -> Result<
        larm_domain::bridge::Page<larm_models::search::SearchCandidate>,
        larm_domain::error::LarmError,
    > {
        let mut trace = PageTrace {
            started: std::time::Instant::now(),
            row: json!({"source_id":id,"requested_limit":request.limit,
            "has_cursor":request.cursor.is_some(),"outcome":"CANCELLED"}),
            records: self.records.clone(),
        };
        let result = self.inner.search_page(id, request).await;
        match &result {
            Ok(page) => {
                trace.row["outcome"] = json!("OK");
                trace.row["items"] = json!(page.items.len());
            }
            Err(error) => {
                trace.row["outcome"] = json!("ERROR");
                trace.row["reason"] = json!(error.details.reason);
            }
        }
        result
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn exercise(
    db: &larm_db::pool::Db,
    fixture: &Fixture,
    runner: Arc<larm_adapter_host::conformance::NativeConformanceRunner>,
    ctx: &MutationContext,
    auth: Arc<larm_core::security::Principals>,
    token: &str,
    source: &larm_domain::sources::ModelSource,
    forbidden: &Path,
    config: larm_models::search::SearchConfig,
    defaults: bool,
    evidence_folder: &str,
) -> Value {
    let identity = uuid::Uuid::now_v7().to_string();
    let (timeout_seconds, cache_seconds) =
        (config.source_timeout.as_secs(), config.cache_ttl.as_secs());
    if defaults {
        let actual = larm_models::search::SearchConfig::default();
        assert_eq!(config.source_timeout, actual.source_timeout);
        assert_eq!(config.cache_ttl, actual.cache_ttl);
    }
    let output = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/reports/P044_runs")
        .join(evidence_folder)
        .join(&identity);
    std::fs::create_dir_all(&output).unwrap();
    let phase_logging = tracing_subscriber::fmt()
        .with_env_filter("off,larm_daemon::source_search=debug,larm_adapter_host::conformance::measurement=debug")
        .json()
        .with_ansi(false)
        .with_writer(std::sync::Mutex::new(
            std::fs::File::create(output.join("provider-phases.ndjson")).unwrap(),
        ))
        .try_init()
        .is_ok();
    let requests = PathBuf::from(fixture.data["requests"].as_str().unwrap());
    let _archive = TraceArchive {
        from: requests.clone(),
        to: output.join("requests.ndjson"),
    };
    let trace = || {
        std::fs::read_to_string(&requests)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str::<Value>(l).unwrap())
            .collect::<Vec<_>>()
    };
    let before = trace();
    assert_eq!(counts(db).await["canonical_model"], 0);
    let records = Arc::new(std::sync::Mutex::new(Vec::<Value>::new()));
    let provider = Arc::new(ObservedProvider {
        inner: qualified_source_search::QualifiedSourceSearch::new(db, runner.clone()),
        records: records.clone(),
    });
    let service = model_search::api_service(
        provider,
        Arc::new(larm_core::security::CursorSigner::default()),
        config,
    );
    let authenticator: Arc<dyn larm_domain::ports::PrincipalAuthenticator> = auth;
    let app = larm_api::search::routes(larm_api::search::SearchState {
        service,
        idempotency: Arc::new(larm_db::security::SqlIdempotency {
            pool: db.pool.clone(),
        }),
    })
    .layer(axum::middleware::from_fn_with_state(
        authenticator,
        larm_api::security::guard,
    ));
    let query = json!({"q":"","capabilities":[],"source_ids":[source.id],"compatibility":"ALL",
        "architectures":[],"formats":[],"quantizations":[],"licenses":[],"runtime_ids":[],
        "parameter_count":{"minimum":null,"maximum":null},"size_bytes":{"minimum":null,"maximum":null},
        "trust_score":{"minimum":null,"maximum":null},"certification_statuses":[],"only_executable":false,
        "only_recommended":false,"show_incompatible":true,"cursor":null,"limit":2});
    let key = uuid::Uuid::now_v7().to_string();
    let http_start = std::time::Instant::now();
    let (status, first) = post(&app, token, &ctx.correlation_id, &key, &query).await;
    let first_request_ms = http_start.elapsed().as_millis();
    let first_source_pages = records.lock().unwrap().clone();
    std::fs::write(
        output.join("first-source-pages.json"),
        serde_json::to_vec_pretty(&first_source_pages).unwrap(),
    )
    .unwrap();
    eprintln!("qualified HTTP first response: status={status}, elapsed_ms={first_request_ms}");
    std::fs::write(
        output.join("first-response.json"),
        serde_json::to_vec_pretty(
            &json!({"status":status,"body":first,"elapsed_ms":first_request_ms,"core_at_first_response":counts(db).await}),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(status, 201, "first qualified HTTP search: {first}");
    if defaults {
        assert!(
            first_request_ms < 60_000,
            "controlled first HTTP search exceeded the desktop transport's 60s: {first_request_ms}ms"
        );
        assert!(!first_source_pages.is_empty());
        for page in &first_source_pages {
            assert_eq!(page["outcome"], "OK");
            assert!(
                page["elapsed_ms"].as_u64().unwrap() <= timeout_seconds * 1000,
                "source page exceeded its configured deadline: {page}"
            );
        }
    }
    assert_eq!(first["items"].as_array().unwrap().len(), 2);
    let core = counts(db).await;
    for table in [
        "canonical_model",
        "model_variant",
        "model_artifact",
        "model_acquisition_manifest",
    ] {
        assert_eq!(core[table], 6, "{table}");
    }
    for table in [
        "model_bundle",
        "artifact_location",
        "download_job",
        "volume_reservation",
        "artifact_provenance",
    ] {
        assert_eq!(core[table], 0, "{table}");
    }
    let replay = post(&app, token, &ctx.correlation_id, &key, &query).await;
    assert_eq!(replay, (201, first.clone()));
    assert_eq!(counts(db).await, core);
    let mut next = query.clone();
    next["cursor"] = first["next_cursor"].clone();
    assert!(next["cursor"].is_string());
    let (status, second) = post(
        &app,
        token,
        &ctx.correlation_id,
        &uuid::Uuid::now_v7().to_string(),
        &next,
    )
    .await;
    assert_eq!(status, 201, "second cached global page: {second}");
    assert_eq!(second["items"].as_array().unwrap().len(), 2);
    assert_eq!(counts(db).await, core);
    let mut last = query.clone();
    last["cursor"] = second["next_cursor"].clone();
    assert!(last["cursor"].is_string());
    let (status, third) = post(
        &app,
        token,
        &ctx.correlation_id,
        &uuid::Uuid::now_v7().to_string(),
        &last,
    )
    .await;
    assert_eq!(status, 201);
    assert_eq!(third["items"].as_array().unwrap().len(), 2);
    assert!(third["next_cursor"].is_null());
    assert_eq!(counts(db).await, core);
    eprintln!("qualified HTTP cached pages and replay verified");
    let mut ids = std::collections::BTreeSet::new();
    let private_ids = fixture.data["catalog"]["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["model"]["id"].as_str().unwrap().to_string())
        .collect::<std::collections::BTreeSet<_>>();
    for item in [&first, &second, &third]
        .into_iter()
        .flat_map(|p| p["items"].as_array().unwrap())
    {
        assert!(ids.insert(item["id"].as_str().unwrap().to_string()));
        let best = &item["metadata"]["search"]["best_variant"];
        assert!(!private_ids.contains(best["variant_id"].as_str().unwrap()));
        assert_eq!(best["size_confidence"], "UNKNOWN");
        assert!(best["size_bytes"].is_null());
        assert!(best["acquisition"]["manifest_id"].is_string());
        assert_eq!(best["compatibility"], "UNKNOWN");
        assert!(best["executable"].is_null());
        assert_eq!(best["artifact_estimates"][0]["selected"], true);
    }
    // Alter an explicitly qualified immutable input only after worker cleanup.
    // The old positive cache/cursor must lose admission even without SQL change.
    let original = std::fs::read(forbidden).unwrap();
    std::fs::write(forbidden, b"owned sentinel changed after qualification").unwrap();
    let revoked = post(
        &app,
        token,
        &ctx.correlation_id,
        &uuid::Uuid::now_v7().to_string(),
        &next,
    )
    .await;
    std::fs::write(forbidden, original).unwrap();
    assert_eq!(revoked.0, 409, "context-revoked cache: {}", revoked.1);
    assert_eq!(
        revoked.1["details"]["reason"],
        "search_source_snapshot_changed"
    );
    assert_eq!(counts(db).await, core);
    eprintln!("qualified HTTP physical-context revocation rejected cached page");
    // Restored input allows a fresh observation; single declared capability
    // avoids conflating native qualification overhead with catalog scale.
    let mut filtered = query.clone();
    filtered["capabilities"] =
        json!([fixture.data["catalog"]["entries"][0]["model"]["capabilities"][0]]);
    let recovered = post(
        &app,
        token,
        &ctx.correlation_id,
        &uuid::Uuid::now_v7().to_string(),
        &filtered,
    )
    .await;
    assert_eq!(recovered.0, 201, "fresh recovered source: {}", recovered.1);
    assert_eq!(recovered.1["items"].as_array().unwrap().len(), 1);
    let sources = larm_core::sources::SourceCatalog {
        repo: Arc::new(larm_db::sources::SqlSources {
            pool: db.pool.clone(),
            qualification: Some(runner),
        }),
        installations: Arc::new(larm_db::adapters::SqlAdapterInstallations {
            pool: db.pool.clone(),
        }),
    };
    let mut disabled = source.configuration();
    disabled.enabled = false;
    sources
        .update(&source.id, source.row_version, &disabled, ctx)
        .await
        .unwrap();
    let unavailable = post(
        &app,
        token,
        &ctx.correlation_id,
        &uuid::Uuid::now_v7().to_string(),
        &query,
    )
    .await;
    assert_eq!(unavailable.0, 503, "disabled source: {}", unavailable.1);
    assert_eq!(
        unavailable.1["details"]["reason"],
        "all_sources_unavailable"
    );
    let after = trace();
    assert!(after[before.len()..]
        .iter()
        .all(|row| !row["path"].as_str().unwrap().starts_with("/artifact/")));
    let result = json!({"id":identity,"scope":"in-process authenticated Axum HTTP router -> native qualified source provider -> P041 atomic catalog; no TCP daemon, GUI, private auth or Core download",
        "source_id":source.id,"models":ids,"core_after_first_query":core,"core_at_end":counts(db).await,
        "http_pages":[201,201,201],"idempotent_replay_equal_without_events":true,
        "first_request_ms":first_request_ms,"http_exercise_ms":http_start.elapsed().as_millis(),
        "physical_context_revocation":revoked,"fresh_query_after_context_restoration_status":recovered.0,
        "disabled_source":unavailable,"metadata_request_trace_start":before.len(),"metadata_request_trace_end":after.len(),
        "no_artifact_reads_during_search":true,"size_scalar_without_typed_evidence_remains_unknown":true,
        "test_source_timeout_seconds":timeout_seconds,"test_cache_ttl_seconds":cache_seconds,
        "production_search_defaults":defaults,"debug_assertions":cfg!(debug_assertions),"phase_logging":phase_logging});
    std::fs::write(
        output.join("source-pages.json"),
        serde_json::to_vec_pretty(&*records.lock().unwrap()).unwrap(),
    )
    .unwrap();
    std::fs::write(
        output.join("result.json"),
        serde_json::to_vec_pretty(&result).unwrap(),
    )
    .unwrap();
    result
}
