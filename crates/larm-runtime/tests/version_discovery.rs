#![allow(clippy::unwrap_used, clippy::expect_used)]
use async_trait::async_trait;
use larm_domain::error::{ErrorCode, LarmError};
use larm_domain::runtime::{
    Runtime, RuntimeChannel, RuntimeChannelPolicy, RuntimePromotion, RuntimeVersion,
};
use larm_domain::runtime_install::{RuntimeVersionPage, RuntimeVersionSource, VersionQuery};
use larm_runtime::version_discovery::collect;
use serde_json::{json, Map};
use std::collections::VecDeque;
use std::num::NonZeroU32;
use std::sync::Mutex;

const RID: &str = "0192aaaa-0000-7000-8000-000000000001";
const AT: &str = "2026-10-20T14:00:00.000Z";
const CHANNELS: [RuntimeChannel; 5] = [
    RuntimeChannel::Stable,
    RuntimeChannel::Prerelease,
    RuntimeChannel::Nightly,
    RuntimeChannel::Commit,
    RuntimeChannel::Custom,
];
fn runtime(channels: Vec<RuntimeChannel>) -> Runtime {
    Runtime {
        id: RID.into(),
        created_at: AT.into(),
        updated_at: AT.into(),
        row_version: 1,
        metadata: Map::new(),
        name: "Fixture".into(),
        adapter_id: "0192aaaa-0000-7000-8000-000000000002".into(),
        capabilities: vec![],
        default_version_id: None,
        channel_policy: RuntimeChannelPolicy {
            allowed_channels: channels,
        },
        retention_policy: Default::default(),
    }
}
fn version(channel: RuntimeChannel, n: u32) -> RuntimeVersion {
    RuntimeVersion {
        id: format!("0192aaaa-0000-7000-8000-{n:012x}"),
        created_at: AT.into(),
        updated_at: AT.into(),
        row_version: 1,
        metadata: Map::new(),
        runtime_id: RID.into(),
        version_string: "same-upstream-build".into(),
        commit: Some("abcdef".into()),
        build: Some("b7142".into()),
        channel,
        released_at: None,
        artifact_hash: None,
        promotion: RuntimePromotion::None,
        lifecycle: Default::default(),
    }
}
fn page(items: Vec<RuntimeVersion>, cursor: Option<&str>) -> RuntimeVersionPage {
    RuntimeVersionPage {
        items,
        next_cursor: cursor.map(str::to_string),
        snapshot_at: AT.into(),
    }
}
struct Source {
    pages: Mutex<VecDeque<Result<RuntimeVersionPage, LarmError>>>,
    calls: Mutex<Vec<VersionQuery>>,
}
impl Source {
    fn new(pages: Vec<RuntimeVersionPage>) -> Self {
        Self {
            pages: Mutex::new(pages.into_iter().map(Ok).collect()),
            calls: Mutex::new(vec![]),
        }
    }
}
#[async_trait]
impl RuntimeVersionSource for Source {
    async fn list_versions(&self, query: &VersionQuery) -> Result<RuntimeVersionPage, LarmError> {
        self.calls.lock().unwrap().push(query.clone());
        self.pages
            .lock()
            .unwrap()
            .pop_front()
            .expect("fixture agotada")
    }
}
fn budget(n: u32) -> NonZeroU32 {
    NonZeroU32::new(n).unwrap()
}

#[tokio::test]
async fn cinco_canales_y_unknown_original_sin_promocion_automatica() {
    let mut rows: Vec<_> = CHANNELS
        .iter()
        .enumerate()
        .map(|(i, &c)| version(c, i as u32 + 10))
        .collect();
    rows[4].metadata.insert(
        "upstream_channel".into(),
        json!({"value":"experimental.vendor/42", "reason":"UNKNOWN"}),
    );
    let source = Source::new(rows.iter().cloned().map(|v| page(vec![v], None)).collect());
    let result = collect(&source, &runtime(CHANNELS.to_vec()), budget(5))
        .await
        .unwrap();
    assert_eq!(result.versions, rows);
    assert!(result
        .versions
        .iter()
        .all(|v| v.promotion == RuntimePromotion::None));
    assert_eq!(
        source
            .calls
            .lock()
            .unwrap()
            .iter()
            .map(|q| q.channel)
            .collect::<Vec<_>>(),
        CHANNELS
    );
}

#[tokio::test]
async fn marca_unknown_legacy_se_publica_en_forma_canonica_sin_inventar_origen() {
    let mut row = version(RuntimeChannel::Custom, 20);
    row.metadata
        .insert("channel_reason".into(), json!("UNKNOWN"));
    let source = Source::new(vec![page(vec![row], None)]);
    let result = collect(&source, &runtime(vec![RuntimeChannel::Custom]), budget(1))
        .await
        .unwrap();
    assert_eq!(
        result.versions[0].metadata["upstream_channel"],
        json!({"value":"", "reason":"UNKNOWN"})
    );
    assert!(!result.versions[0]
        .observed_channel()
        .unwrap()
        .automatic_selection_allowed(&RuntimeChannelPolicy {
            allowed_channels: vec![RuntimeChannel::Custom],
        }));

    let mut inconsistent = version(RuntimeChannel::Stable, 21);
    inconsistent.metadata.insert(
        "upstream_channel".into(),
        json!({"value":"experimental.vendor/42", "reason":"UNKNOWN"}),
    );
    let error = collect(
        &Source::new(vec![page(vec![inconsistent], None)]),
        &runtime(vec![RuntimeChannel::Stable]),
        budget(1),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::ProtocolError);
    assert_eq!(error.details.reason, "runtime_upstream_channel_mismatch");
}

#[tokio::test]
async fn politica_distingue_prerelease_nightly_commit_y_resultado_determinista() {
    let channels = vec![
        RuntimeChannel::Prerelease,
        RuntimeChannel::Nightly,
        RuntimeChannel::Commit,
    ];
    let pages: Vec<_> = channels
        .iter()
        .enumerate()
        .map(|(i, &c)| page(vec![version(c, i as u32 + 10)], None))
        .collect();
    let source = Source::new(pages.clone());
    let mut inverted = channels.clone();
    inverted.reverse();
    let result = collect(&source, &runtime(inverted), budget(3))
        .await
        .unwrap();
    let other = collect(&Source::new(pages), &runtime(channels.clone()), budget(3))
        .await
        .unwrap();
    assert_eq!(result, other);
    assert_eq!(
        source
            .calls
            .lock()
            .unwrap()
            .iter()
            .map(|q| q.channel)
            .collect::<Vec<_>>(),
        channels
    );
    assert_eq!(result.versions.len(), 3);
}

#[tokio::test]
async fn consulta_todas_las_paginas_y_conserva_cursores_opacos() {
    let source = Source::new(vec![
        page(
            vec![version(RuntimeChannel::Stable, 10)],
            Some("opaque:+/cursor"),
        ),
        page(vec![version(RuntimeChannel::Stable, 11)], None),
    ]);
    let result = collect(&source, &runtime(vec![RuntimeChannel::Stable]), budget(2))
        .await
        .unwrap();
    assert_eq!(result.versions.len(), 2);
    let calls = source.calls.lock().unwrap();
    assert_eq!(calls[0].cursor, None);
    assert_eq!(calls[1].cursor.as_deref(), Some("opaque:+/cursor"));
    assert_eq!(result.page_snapshots.len(), 2);
}

#[tokio::test]
async fn politica_invalida_se_rechaza_antes_del_callback() {
    let source = Source::new(vec![]);
    for channels in [vec![], vec![RuntimeChannel::Stable, RuntimeChannel::Stable]] {
        assert!(collect(&source, &runtime(channels), budget(1))
            .await
            .is_err());
    }
    assert!(source.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn canal_o_runtime_ajeno_no_produce_un_batch_aceptado() {
    let mut wrong = version(RuntimeChannel::Stable, 10);
    wrong.runtime_id = "0192aaaa-0000-7000-8000-000000000003".into();
    for row in [wrong, version(RuntimeChannel::Custom, 10)] {
        let source = Source::new(vec![page(vec![row], None)]);
        let error = collect(&source, &runtime(vec![RuntimeChannel::Stable]), budget(1))
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::ProtocolError);
        assert_eq!(error.details.reason, "runtime_version_binding_mismatch");
    }
}

#[tokio::test]
async fn cursor_repetido_no_devuelve_exito_parcial() {
    let source = Source::new(vec![
        page(vec![], Some("again")),
        page(vec![], Some("again")),
    ]);
    let error = collect(&source, &runtime(vec![RuntimeChannel::Stable]), budget(10))
        .await
        .unwrap_err();
    assert_eq!(error.details.reason, "runtime_discovery_cursor_repeated");
    assert_eq!(source.calls.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn identidad_repetida_no_se_fusiona_silenciosamente() {
    let row = version(RuntimeChannel::Stable, 10);
    let source = Source::new(vec![
        page(vec![row.clone()], Some("next")),
        page(vec![row], None),
    ]);
    let error = collect(&source, &runtime(vec![RuntimeChannel::Stable]), budget(2))
        .await
        .unwrap_err();
    assert_eq!(error.details.reason, "runtime_discovery_duplicate_version");
}

#[tokio::test]
async fn presupuesto_agotado_no_trunca_ni_continua_io() {
    let source = Source::new(vec![page(
        vec![version(RuntimeChannel::Stable, 10)],
        Some("next"),
    )]);
    let error = collect(&source, &runtime(vec![RuntimeChannel::Stable]), budget(1))
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::ResourceUnavailable);
    assert_eq!(
        error.details.reason,
        "runtime_discovery_page_budget_exhausted"
    );
    assert_eq!(source.calls.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn error_del_proveedor_conserva_codigo_motivo_y_correlacion() {
    let original = LarmError::new(ErrorCode::Timeout, "fixture", "fixture_timeout");
    let correlation = original.correlation_id.clone();
    let source = Source {
        pages: Mutex::new(VecDeque::from([Err(original)])),
        calls: Mutex::new(vec![]),
    };
    let error = collect(&source, &runtime(vec![RuntimeChannel::Stable]), budget(1))
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::Timeout);
    assert_eq!(error.details.reason, "fixture_timeout");
    assert_eq!(error.correlation_id, correlation);
}

#[tokio::test]
async fn pagina_invalida_no_se_acepta_por_ser_respuesta_del_proveedor() {
    let mut bad_time = page(vec![], None);
    bad_time.snapshot_at = "2026-10-20T14:00:00Z".into();
    let mut bad_revision = version(RuntimeChannel::Stable, 10);
    bad_revision.row_version = 0;
    for invalid in [
        bad_time,
        page(vec![], Some("")),
        page(vec![bad_revision], None),
    ] {
        let error = collect(
            &Source::new(vec![invalid]),
            &runtime(vec![RuntimeChannel::Stable]),
            budget(1),
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::ValidationFailed);
    }
}
