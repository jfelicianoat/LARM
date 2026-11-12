//! Protocolo de adaptadores v1 (P020; contrato 03): JSON-RPC 2.0 sobre
//! stdio en NDJSON.
//!
//! - Una línea UTF-8 por frame, a lo sumo 1 MiB. stdout solo lleva frames;
//!   los logs van a stderr (filtrados, máx. 10 MiB).
//! - `jsonrpc = "2.0"`, `id` UUIDv7, `method` de la tabla publicada.
//! - Una respuesta por `id`. El progreso es una notificación
//!   `core.progress` sin `id`.
//! - Errores: `-32700` JSON mal formado, `-32600` frame que no es una
//!   petición válida, `-32601` método inexistente o no anunciado, `-32602`
//!   parámetros inválidos, `-32000` error de dominio con `data` = `LarmError`.
//! - `core.handshake`: misma versión mayor y el `nonce` exacto, o se rechaza
//!   antes de cualquier IO. `initialize` (legado) solo se traduce.

use larm_domain::error::{ErrorCode, LarmError};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Versión del protocolo.
pub const PROTOCOL_VERSION: &str = "1.0";
/// Límite de un frame (bytes, sin el salto de línea).
pub const MAX_FRAME_BYTES: usize = 1 << 20;
/// Límite de logs por stderr.
pub const MAX_LOG_BYTES: usize = 10 << 20;
/// Gracia de cancelación antes de matar el proceso.
pub const CANCEL_GRACE_SECONDS: u64 = 5;
/// Intervalo de heartbeat y ausencias toleradas.
pub const HEARTBEAT_SECONDS: u64 = 10;
pub const HEARTBEAT_MISSES: u32 = 3;

pub const PARSE_ERROR: i64 = -32700;
pub const INVALID_REQUEST: i64 = -32600;
pub const METHOD_NOT_FOUND: i64 = -32601;
pub const INVALID_PARAMS: i64 = -32602;
pub const DOMAIN_ERROR: i64 = -32000;

/// Objeto `error` de JSON-RPC.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Box<LarmError>>,
}

impl RpcError {
    #[must_use]
    pub fn new(code: i64, message: &str) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }

    /// Error de dominio: `-32000` con el `LarmError` en `data`.
    #[must_use]
    pub fn domain(error: LarmError) -> Self {
        Self {
            code: DOMAIN_ERROR,
            message: error.message.clone(),
            data: Some(Box::new(error)),
        }
    }

    /// Lo que ve el host como `LarmError`.
    #[must_use]
    pub fn to_larm(&self) -> LarmError {
        if let Some(e) = &self.data {
            return e.as_ref().clone();
        }
        let (code, reason) = match self.code {
            PARSE_ERROR | INVALID_REQUEST => (ErrorCode::ProtocolError, "malformed_frame"),
            METHOD_NOT_FOUND => (ErrorCode::UnsupportedCapability, "method_not_supported"),
            INVALID_PARAMS => (ErrorCode::ValidationFailed, "invalid_params"),
            _ => (ErrorCode::AdapterCrashed, "adapter_error"),
        };
        LarmError::new(code, "El adaptador devolvió un error", reason)
            .with_technical(format!("{}: {}", self.code, self.message))
    }
}

/// Un frame del protocolo.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Frame {
    Request(Request),
    Response(Response),
    Notification(Notification),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub jsonrpc: String,
    pub id: String,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Response {
    pub jsonrpc: String,
    /// `null` solo cuando el frame recibido no tenía un id legible.
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Notification {
    pub jsonrpc: String,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

impl Request {
    #[must_use]
    pub fn new(method: &str, params: Value) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id: uuid::Uuid::now_v7().to_string(),
            method: method.into(),
            params,
        }
    }
}

impl Response {
    #[must_use]
    pub fn ok(id: &str, result: Value) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id: Some(id.into()),
            result: Some(result),
            error: None,
        }
    }

    #[must_use]
    pub fn err(id: Option<&str>, error: RpcError) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id: id.map(String::from),
            result: None,
            error: Some(error),
        }
    }
}

/// `core.progress` (sin id; nunca mensajes de usuario ni credenciales).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Progress {
    pub request_id: String,
    pub phase: String,
    pub completed_bytes: Option<u64>,
    pub total_bytes: Option<u64>,
    /// Código estable, no texto libre.
    pub message_code: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Handshake {
    pub protocol_version: String,
    pub adapter_id: String,
    pub nonce: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HandshakeResult {
    pub protocol_version: String,
    pub adapter_version: String,
    pub methods: Vec<String>,
    pub capabilities: Vec<String>,
    pub nonce: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CancelRequest {
    pub request_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CancelResult {
    pub cancelled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Heartbeat {
    pub alive: bool,
    pub running_request_ids: Vec<String>,
}

/// Fallo al leer un frame, ya con la respuesta que corresponde.
#[derive(Debug, Clone, PartialEq)]
pub struct FrameError {
    pub id: Option<String>,
    pub error: RpcError,
}

/// Codifica un frame como una línea (con `\n`).
///
/// # Errors
/// `PROTOCOL_ERROR` si supera [`MAX_FRAME_BYTES`].
pub fn encode(frame: &Frame) -> Result<Vec<u8>, LarmError> {
    let mut bytes = serde_json::to_vec(frame).map_err(|e| {
        LarmError::new(
            ErrorCode::Internal,
            "Frame no serializable",
            "frame_serialize",
        )
        .with_technical(e.to_string())
    })?;
    if bytes.len() > MAX_FRAME_BYTES {
        return Err(LarmError::new(
            ErrorCode::ProtocolError,
            "Frame demasiado grande",
            "frame_too_large",
        ));
    }
    bytes.push(b'\n');
    Ok(bytes)
}

fn version_major(v: &str) -> Option<u32> {
    v.split('.').next()?.parse().ok()
}

/// Decodifica una línea recibida.
///
/// # Errors
/// [`FrameError`] con `-32700` (no es JSON, no es UTF-8 o supera el límite)
/// o `-32600` (JSON que no es un frame del protocolo: falta `jsonrpc` 2.0,
/// id no UUIDv7…).
pub fn decode(line: &[u8]) -> Result<Frame, FrameError> {
    let parse = |m: &str| FrameError {
        id: None,
        error: RpcError::new(PARSE_ERROR, m),
    };
    if line.len() > MAX_FRAME_BYTES {
        return Err(parse("frame demasiado grande"));
    }
    let text = std::str::from_utf8(line).map_err(|_| parse("frame no UTF-8"))?;
    let text = text.trim_end_matches(['\r', '\n']);
    if text.contains('\n') {
        return Err(parse("un frame es una sola línea"));
    }
    let value: Value = serde_json::from_str(text).map_err(|_| parse("JSON mal formado"))?;
    let raw_id = value.get("id").and_then(Value::as_str);
    let id = raw_id
        .filter(|id| larm_domain::bridge::is_uuid_v7(id))
        .map(String::from);
    let invalid = |m: &str| FrameError {
        id: id.clone(),
        error: RpcError::new(INVALID_REQUEST, m),
    };
    if value.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Err(invalid("jsonrpc debe ser 2.0"));
    }
    if let Some(i) = raw_id {
        if !larm_domain::bridge::is_uuid_v7(i) {
            return Err(invalid("id debe ser UUIDv7"));
        }
    }
    let original = value.clone();
    let mut frame = serde_json::from_value::<Frame>(value)
        .map_err(|_| invalid("no es una petición, respuesta ni notificación válida"))?;
    if let Frame::Request(request) = &frame {
        if request.method.is_empty() {
            return Err(invalid("method vacío"));
        }
    }
    if let Frame::Response(response) = &mut frame {
        if !original.get("id").is_some()
            || original.get("result").is_some() == original.get("error").is_some()
        {
            return Err(invalid(
                "respuesta requiere id y exactamente result o error",
            ));
        }
        // Option<Value> no distingue null de clave ausente al deserializar.
        if let Some(result) = original.get("result") {
            response.result = Some(result.clone());
        }
        if let Some(error) = &response.error {
            if ![
                PARSE_ERROR,
                INVALID_REQUEST,
                METHOD_NOT_FOUND,
                INVALID_PARAMS,
                DOMAIN_ERROR,
            ]
            .contains(&error.code)
            {
                return Err(invalid("código JSON-RPC no publicado"));
            }
            if let Some(data) = &error.data {
                if !larm_domain::bridge::is_uuid_v7(&data.correlation_id)
                    || data.message.is_empty()
                    || data.details.reason.is_empty()
                    || data.details.original_error.as_ref().is_some_and(|e| {
                        e.system.is_empty() || e.code.is_empty() || e.message.chars().count() > 4096
                    })
                {
                    return Err(invalid("Error de dominio fuera del contrato"));
                }
            }
        }
        if serde_json::to_value(&response).map_err(|_| invalid("respuesta no serializable"))?
            != original
        {
            return Err(invalid("respuesta incompleta o campos nullable ausentes"));
        }
    }
    // El progreso tiene forma cerrada: ni texto para el usuario ni campos de más.
    if let Frame::Notification(n) = &frame {
        let progress = serde_json::from_value::<Progress>(n.params.clone());
        if n.method != "core.progress"
            || progress.as_ref().is_err()
            || progress.is_ok_and(|p| {
                !larm_domain::bridge::is_uuid_v7(&p.request_id)
                    || p.phase.is_empty()
                    || serde_json::to_value(&p).ok().as_ref() != Some(&n.params)
            })
        {
            return Err(invalid(
                "notificación no prevista o progreso con otra forma",
            ));
        }
    }
    Ok(frame)
}

/// Mensaje del host para iniciar la sesión: versión, id del adaptador y un
/// nonce aleatorio nuevo.
#[must_use]
pub fn handshake_request(adapter_id: &str) -> (Request, String) {
    let nonce = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    let params = Handshake {
        protocol_version: PROTOCOL_VERSION.into(),
        adapter_id: adapter_id.into(),
        nonce: nonce.clone(),
    };
    (
        Request::new(
            "core.handshake",
            serde_json::to_value(params).unwrap_or_default(),
        ),
        nonce,
    )
}

/// Comprueba la respuesta del handshake frente al manifiesto: versión mayor
/// compatible, nonce exacto y ningún método o capacidad fuera de lo
/// anunciado.
///
/// # Errors
/// `PROTOCOL_ERROR` con `protocol_version_mismatch`, `nonce_mismatch` o
/// `handshake_methods_not_in_manifest`.
pub fn check_handshake(
    result: &HandshakeResult,
    nonce: &str,
    manifest: &crate::manifest::AdapterManifest,
) -> Result<(), LarmError> {
    let refuse = |reason: &str, m: &str| {
        Err(LarmError::new(ErrorCode::ProtocolError, m, reason).retryable(false))
    };
    if version_major(&result.protocol_version) != version_major(PROTOCOL_VERSION) {
        return refuse(
            "protocol_version_mismatch",
            "Versión de protocolo incompatible",
        );
    }
    if result.nonce != nonce {
        return refuse("nonce_mismatch", "El nonce del handshake no coincide");
    }
    if result.adapter_version != manifest.version {
        return refuse(
            "adapter_version_mismatch",
            "La versión del adaptador no es la del manifiesto",
        );
    }
    if result.methods.iter().any(|m| !manifest.methods.contains(m)) {
        return refuse(
            "handshake_methods_not_in_manifest",
            "El adaptador anuncia métodos fuera de su manifiesto",
        );
    }
    Ok(())
}

/// Lado del adaptador: valida el handshake recibido.
///
/// # Errors
/// [`RpcError`] `-32602` si los parámetros no son un `Handshake`; `-32000`
/// con `PROTOCOL_ERROR` si la versión mayor no es compatible o el id no es
/// el suyo.
pub fn accept_handshake(params: &Value, adapter_id: &str) -> Result<Handshake, RpcError> {
    let h: Handshake = serde_json::from_value(params.clone())
        .map_err(|_| RpcError::new(INVALID_PARAMS, "Handshake inválido"))?;
    if version_major(&h.protocol_version) != version_major(PROTOCOL_VERSION) {
        return Err(RpcError::domain(LarmError::new(
            ErrorCode::ProtocolError,
            "Versión de protocolo incompatible",
            "protocol_version_mismatch",
        )));
    }
    if h.adapter_id != adapter_id {
        return Err(RpcError::domain(LarmError::new(
            ErrorCode::ProtocolError,
            "El handshake es para otro adaptador",
            "adapter_id_mismatch",
        )));
    }
    Ok(h)
}

/// Traducción del `initialize` de legado a `core.handshake` (no es un
/// método público: solo se reconoce para responder con la forma nueva).
#[must_use]
pub fn translate_legacy(request: &Request) -> Option<Request> {
    (request.method == "initialize").then(|| Request {
        jsonrpc: "2.0".into(),
        id: request.id.clone(),
        method: "core.handshake".into(),
        params: serde_json::json!({
            "protocol_version": request.params.get("protocolVersion").or_else(|| request.params.get("protocol_version")).cloned().unwrap_or(Value::Null),
            "adapter_id": request.params.get("adapter_id").cloned().unwrap_or(Value::Null),
            "nonce": request.params.get("nonce").cloned().unwrap_or(Value::Null),
        }),
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    #[test]
    fn decoder_rechaza_respuestas_incompletas_sin_ocultarlo_en_el_oraculo() {
        let id = uuid::Uuid::now_v7().to_string();
        let valid = serde_json::json!({"jsonrpc":"2.0","id":id,"result":null});
        let frame = decode(valid.to_string().as_bytes()).unwrap();
        assert_eq!(serde_json::to_value(frame).unwrap(), valid);
        for invalid in [
            serde_json::json!({"jsonrpc":"2.0","id":id}),
            serde_json::json!({"jsonrpc":"2.0","result":{}}),
            serde_json::json!({"jsonrpc":"2.0","id":id,"result":{},"error":{"code":-32000,"message":"x"}}),
            serde_json::json!({"jsonrpc":"2.0","id":id,"error":{"code":12345,"message":"x"}}),
            serde_json::json!({"jsonrpc":"2.0","id":id,"error":null}),
            serde_json::json!({"jsonrpc":"2.0","method":"core.progress","params":{"request_id":id,"phase":"X"}}),
        ] {
            assert!(decode(invalid.to_string().as_bytes()).is_err(), "{invalid}");
        }
        let bad = decode(br#"{"jsonrpc":"2.0","id":"invalid","method":"x"}"#).unwrap_err();
        assert_eq!(
            bad.id, None,
            "un id inválido no puede salir en otro frame inválido"
        );
    }

    #[test]
    fn framing_y_errores() {
        let req = Request::new("core.heartbeat", serde_json::json!({}));
        let bytes = encode(&Frame::Request(req.clone())).unwrap();
        assert_eq!(bytes.last(), Some(&b'\n'));
        assert_eq!(
            bytes.iter().filter(|b| **b == b'\n').count(),
            1,
            "una línea"
        );
        assert_eq!(decode(&bytes).unwrap(), Frame::Request(req.clone()));
        // Mal formado: -32700 sin id.
        let e = decode(b"{\"jsonrpc\":\"2.0\",").unwrap_err();
        assert_eq!((e.error.code, e.id), (PARSE_ERROR, None));
        assert_eq!(decode(&[0xff, 0xfe]).unwrap_err().error.code, PARSE_ERROR);
        let big = vec![b' '; MAX_FRAME_BYTES + 1];
        assert_eq!(decode(&big).unwrap_err().error.code, PARSE_ERROR);
        // JSON válido que no es un frame: -32600 con el id si se puede leer.
        let e = decode(format!(r#"{{"jsonrpc":"1.0","id":"{}","method":"x"}}"#, req.id).as_bytes())
            .unwrap_err();
        assert_eq!(
            (e.error.code, e.id.as_deref()),
            (INVALID_REQUEST, Some(req.id.as_str()))
        );
        assert_eq!(
            decode(br#"{"jsonrpc":"2.0","id":"1","method":"x"}"#)
                .unwrap_err()
                .error
                .code,
            INVALID_REQUEST
        );
        assert_eq!(decode(br#"{"jsonrpc":"2.0","id":"0192f000-0000-7000-8000-000000000001","method":"x","extra":1}"#).unwrap_err().error.code, INVALID_REQUEST);
        // Notificación de progreso sin id.
        let n = decode(br#"{"jsonrpc":"2.0","method":"core.progress","params":{"request_id":"0192f000-0000-7000-8000-000000000001","phase":"DOWNLOADING","completed_bytes":1,"total_bytes":2,"message_code":null}}"#).unwrap();
        assert!(matches!(n, Frame::Notification(_)));
        // Solo existe la notificación de progreso.
        let other = br#"{"jsonrpc":"2.0","method":"core.log","params":{"request_id":"r","phase":"X","completed_bytes":null,"total_bytes":null,"message_code":null}}"#;
        assert_eq!(decode(other).unwrap_err().error.code, INVALID_REQUEST);
        // Un frame enorme no se emite.
        let huge = Request::new("x", Value::String("a".repeat(MAX_FRAME_BYTES)));
        assert_eq!(
            encode(&Frame::Request(huge)).unwrap_err().details.reason,
            "frame_too_large"
        );
    }

    #[test]
    fn error_de_dominio_y_mapeo() {
        let e = LarmError::new(ErrorCode::IntegrityFailed, "hash", "hash_mismatch");
        let r = RpcError::domain(e.clone());
        assert_eq!(r.code, DOMAIN_ERROR);
        assert_eq!(r.to_larm().details.reason, "hash_mismatch");
        let wire = serde_json::to_value(&r).unwrap();
        assert_eq!(wire["data"]["code"], "INTEGRITY_FAILED");
        assert_eq!(
            RpcError::new(METHOD_NOT_FOUND, "x").to_larm().code,
            ErrorCode::UnsupportedCapability
        );
        assert_eq!(
            RpcError::new(PARSE_ERROR, "x").to_larm().code,
            ErrorCode::ProtocolError
        );
        assert_eq!(
            RpcError::new(INVALID_PARAMS, "x").to_larm().code,
            ErrorCode::ValidationFailed
        );
    }

    #[test]
    fn handshake_version_y_nonce() {
        let m = crate::manifest::AdapterManifest::parse(
            &crate::manifest::tests_support::manifest_json().to_string(),
        )
        .unwrap();
        let (req, nonce) = handshake_request(&m.id);
        assert_eq!(req.method, "core.handshake");
        assert!(nonce.len() >= 64);
        let ok = HandshakeResult {
            protocol_version: "1.0".into(),
            adapter_version: m.version.clone(),
            methods: m.methods.clone(),
            capabilities: m.capabilities.clone(),
            nonce: nonce.clone(),
        };
        assert!(check_handshake(&ok, &nonce, &m).is_ok());
        let mut v2 = ok.clone();
        v2.protocol_version = "2.0".into();
        assert_eq!(
            check_handshake(&v2, &nonce, &m).unwrap_err().details.reason,
            "protocol_version_mismatch"
        );
        let mut minor = ok.clone();
        minor.protocol_version = "1.3".into();
        assert!(check_handshake(&minor, &nonce, &m).is_ok(), "misma mayor");
        let mut bad_nonce = ok.clone();
        bad_nonce.nonce = "otro".into();
        assert_eq!(
            check_handshake(&bad_nonce, &nonce, &m)
                .unwrap_err()
                .details
                .reason,
            "nonce_mismatch"
        );
        let mut extra = ok;
        extra.methods.push("runtime.benchmark".into());
        assert_eq!(
            check_handshake(&extra, &nonce, &m)
                .unwrap_err()
                .details
                .reason,
            "handshake_methods_not_in_manifest"
        );
        // Lado del adaptador.
        assert!(accept_handshake(&req.params, &m.id).is_ok());
        let mut p = req.params.clone();
        p["protocol_version"] = "2.0".into();
        assert_eq!(
            accept_handshake(&p, &m.id)
                .unwrap_err()
                .data
                .unwrap()
                .details
                .reason,
            "protocol_version_mismatch"
        );
        assert_eq!(
            accept_handshake(&serde_json::json!({}), &m.id)
                .unwrap_err()
                .code,
            INVALID_PARAMS
        );
        // Legado.
        let legacy = Request::new(
            "initialize",
            serde_json::json!({"protocolVersion": "1.0", "adapter_id": m.id, "nonce": "n"}),
        );
        let t = translate_legacy(&legacy).unwrap();
        assert_eq!((t.method.as_str(), t.id), ("core.handshake", legacy.id));
        assert!(translate_legacy(&Request::new("core.heartbeat", Value::Null)).is_none());
    }
}
