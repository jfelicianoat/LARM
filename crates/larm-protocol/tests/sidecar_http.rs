#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use larm_domain::correlation::CorrelationId;
use larm_protocol::sidecar::{RunningSidecar, SidecarConfig};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::process::{Child, Command};
use uuid::Uuid;

struct HttpResponse {
    status: u16,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
}

impl HttpResponse {
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap()
    }
}

async fn http_request(
    endpoint: &str,
    method: &str,
    path: &str,
    token: &str,
    correlation: Option<&str>,
    body: Option<&Value>,
) -> HttpResponse {
    let address = endpoint.strip_prefix("http://").unwrap();
    let payload = body
        .map(serde_json::to_vec)
        .transpose()
        .unwrap()
        .unwrap_or_default();
    let mut request = format!(
        "{method} {path} HTTP/1.1\r\nHost: {address}\r\nAuthorization: Bearer {token}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n",
        payload.len()
    );
    if let Some(correlation) = correlation {
        request.push_str(&format!("x-correlation-id: {correlation}\r\n"));
    }
    request.push_str("\r\n");

    let mut stream = TcpStream::connect(address).await.unwrap();
    stream.write_all(request.as_bytes()).await.unwrap();
    stream.write_all(&payload).await.unwrap();
    let mut response = Vec::new();
    stream.read_to_end(&mut response).await.unwrap();
    let header_end = response
        .windows(4)
        .position(|bytes| bytes == b"\r\n\r\n")
        .map(|position| position + 4)
        .unwrap();
    let header = std::str::from_utf8(&response[..header_end]).unwrap();
    let mut lines = header.split("\r\n");
    let status = lines
        .next()
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();
    let headers = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.to_ascii_lowercase(), value.trim().to_owned()))
        .collect();
    HttpResponse {
        status,
        headers,
        body: response[header_end..].to_vec(),
    }
}

fn fingerprint() -> Value {
    json!({
        "hash": "a".repeat(64),
        "components": {
            "fixture": {"value":"reference-echo","status":"DECLARED","evidence_refs":["fixture:reference-sidecar"]}
        }
    })
}

fn config(delay_ms: u64) -> SidecarConfig {
    let mut config = SidecarConfig::new(
        "reference-echo",
        CorrelationId::new().to_string(),
        PathBuf::from(env!("CARGO_BIN_EXE_larm-sidecar-reference-cli")),
        if delay_ms == 0 {
            vec![]
        } else {
            vec!["--delay-ms".into(), delay_ms.to_string()]
        },
        fingerprint(),
    );
    config.timeout_ms = 10_000;
    config
}

fn request(request_id: String) -> Value {
    json!({
        "lease_id": Uuid::now_v7().to_string(),
        "request_id": request_id,
        "capability": "text.generate",
        "input": {
            "messages": [
                {"role":"system","text":"be concise","media":[],"tool_call_id":null},
                {"role":"user","text":"echo me","media":[],"tool_call_id":null}
            ],
            "tools": []
        },
        "generation": null,
        "stream": false
    })
}

async fn launch_external(config: &SidecarConfig) -> (Child, Value, String) {
    let token = config.auth_token.clone();
    let mut command = Command::new(env!("CARGO_BIN_EXE_larm-protocol-sidecar"));
    command
        .env_clear()
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    if let Some(system_root) = std::env::var_os("SystemRoot") {
        command.env("SystemRoot", system_root);
    }
    let mut child = command.spawn().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    stdin
        .write_all(&serde_json::to_vec(config).unwrap())
        .await
        .unwrap();
    stdin.shutdown().await.unwrap();
    drop(stdin);
    let stdout = child.stdout.take().unwrap();
    let mut lines = BufReader::new(stdout).lines();
    let line = tokio::time::timeout(Duration::from_secs(5), lines.next_line())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    (child, serde_json::from_str(&line).unwrap(), token)
}

#[tokio::test]
async fn cli_echo_reaches_an_authenticated_http_endpoint_with_a_declared_subset() {
    let config = config(0);
    let (mut child, ready, token) = launch_external(&config).await;
    assert_eq!(ready["status"], "ready");
    assert!(!ready.to_string().contains(&token));
    let endpoint = ready["endpoint"].as_str().unwrap();
    let correlation = CorrelationId::new().to_string();
    let response = http_request(endpoint, "GET", "/v1/describe", &token, None, None).await;
    assert_eq!(response.status, 200);
    let description = response.json();
    assert_eq!(description["protocol"], "CUSTOM_REST");
    assert_eq!(description["capabilities"], json!(["text.generate"]));
    assert_eq!(description["streaming"], false);
    assert_eq!(description["tools"], false);

    let response = http_request(
        endpoint,
        "POST",
        "/v1/invoke",
        &token,
        Some(&correlation),
        Some(&request(Uuid::now_v7().to_string())),
    )
    .await;
    assert_eq!(response.status, 200);
    assert_eq!(
        response.headers.get("x-correlation-id").unwrap(),
        &correlation
    );
    let response = response.json();
    assert_eq!(
        response["output"]["text"],
        "[system]\nbe concise\n[user]\necho me\n"
    );
    assert_eq!(response["output"]["tool_calls"], json!([]));
    assert_eq!(response["usage"]["tokens_input"], Value::Null);
    child.start_kill().unwrap();
    let _ = child.wait().await.unwrap();
}

#[tokio::test]
async fn unsupported_streaming_and_tools_are_rejected_before_starting_a_cli() {
    let config = config(0);
    let token = config.auth_token.clone();
    let server = RunningSidecar::start(config).await.unwrap();
    let mut body = request(Uuid::now_v7().to_string());
    body["stream"] = json!(true);
    let response = http_request(
        server.endpoint(),
        "POST",
        "/v1/invoke",
        &token,
        None,
        Some(&body),
    )
    .await;
    assert_eq!(response.status, 422);
    assert_eq!(response.json()["code"], "UNSUPPORTED_CAPABILITY");
    assert_eq!(server.metrics_snapshot().active_requests, 0);

    body["stream"] = json!(false);
    body["input"]["tools"] = json!([{"name":"x","description":"x","input_schema":{}}]);
    let response = http_request(
        server.endpoint(),
        "POST",
        "/v1/invoke",
        &token,
        None,
        Some(&body),
    )
    .await;
    assert_eq!(response.status, 422);
    assert_eq!(server.metrics_snapshot().requests_total, 2);

    let mut invalid = request(Uuid::now_v7().to_string());
    invalid["input"]["messages"][0]
        .as_object_mut()
        .unwrap()
        .remove("text");
    let response = http_request(
        server.endpoint(),
        "POST",
        "/v1/invoke",
        &token,
        None,
        Some(&invalid),
    )
    .await;
    assert_eq!(response.status, 400);
    assert_eq!(response.json()["code"], "VALIDATION_FAILED");
    assert_eq!(server.metrics_snapshot().requests_total, 3);
    server.shutdown().await;
}

#[tokio::test]
async fn explicit_cancel_aborts_the_cli_and_releases_the_request_slot() {
    let config = config(8_000);
    let token = config.auth_token.clone();
    let server = RunningSidecar::start(config).await.unwrap();
    let request_id = Uuid::now_v7().to_string();
    let endpoint = server.endpoint().to_owned();
    let invoke_token = token.clone();
    let invoke_id = request_id.clone();
    let invoke = tokio::spawn(async move {
        http_request(
            &endpoint,
            "POST",
            "/v1/invoke",
            &invoke_token,
            None,
            Some(&request(invoke_id)),
        )
        .await
    });
    let started = Instant::now();
    while server.metrics_snapshot().active_requests == 0
        && started.elapsed() < Duration::from_secs(2)
    {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(server.metrics_snapshot().active_requests, 1);
    let response = http_request(
        server.endpoint(),
        "POST",
        "/v1/cancel",
        &token,
        None,
        Some(&json!({"request_id":request_id})),
    )
    .await;
    assert_eq!(response.status, 200);
    assert_eq!(response.json()["cancelled"], true);
    let response = invoke.await.unwrap();
    assert_eq!(response.status, 204);
    assert!(response.body.is_empty());
    assert_eq!(server.metrics_snapshot().active_requests, 0);
    assert_eq!(server.metrics_snapshot().cancelled_total, 1);
    server.shutdown().await;
}

#[tokio::test]
async fn endpoint_requires_the_ephemeral_bearer_token() {
    let config = config(0);
    let server = RunningSidecar::start(config).await.unwrap();
    let response = http_request(
        server.endpoint(),
        "GET",
        "/v1/describe",
        "unused",
        None,
        None,
    )
    .await;
    assert_eq!(response.status, 401);
    assert_eq!(response.json()["code"], "AUTH_REQUIRED");
    server.shutdown().await;
}

#[tokio::test]
async fn timeout_terminates_the_cli_child_and_releases_the_request_slot() {
    let marker = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!(
        "../../target/p103-timeout-{}.marker",
        Uuid::now_v7()
    ));
    let _ = std::fs::remove_file(&marker);
    let mut config = config(500);
    config
        .arguments
        .extend(["--marker".into(), marker.to_string_lossy().into_owned()]);
    config.timeout_ms = 50;
    let token = config.auth_token.clone();
    let server = RunningSidecar::start(config).await.unwrap();
    let response = http_request(
        server.endpoint(),
        "POST",
        "/v1/invoke",
        &token,
        None,
        Some(&request(Uuid::now_v7().to_string())),
    )
    .await;
    assert_eq!(response.status, 504);
    assert_eq!(response.json()["code"], "TIMEOUT");
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert!(
        !marker.exists(),
        "timed-out CLI child survived its deadline"
    );
    assert_eq!(server.metrics_snapshot().timed_out_total, 1);
    assert_eq!(server.metrics_snapshot().active_requests, 0);
    server.shutdown().await;
}

#[tokio::test]
async fn cli_stdout_over_the_configured_limit_is_rejected() {
    let mut config = config(0);
    config.max_output_bytes = 8;
    let token = config.auth_token.clone();
    let server = RunningSidecar::start(config).await.unwrap();
    let response = http_request(
        server.endpoint(),
        "POST",
        "/v1/invoke",
        &token,
        None,
        Some(&request(Uuid::now_v7().to_string())),
    )
    .await;
    assert_eq!(response.status, 502);
    assert_eq!(response.json()["code"], "PROTOCOL_ERROR");
    assert_eq!(server.metrics_snapshot().failed_total, 1);
    assert_eq!(server.metrics_snapshot().active_requests, 0);
    server.shutdown().await;
}
