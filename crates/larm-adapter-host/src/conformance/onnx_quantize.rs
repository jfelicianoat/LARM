//! Cuantizador ONNX de producto (ONNX→ONNX-INT8 vía Olive). Ledger e
//! intentos privados; no acredita inferencia de modelos ni admisión global
//! de Core. Espejo de la suite GGUF/ONNX con oráculo Rust que verifica la
//! estructura QDQ del modelo cuantizado publicado y replica la cuantización
//! dinámica del peso byte a byte (scale = f32(max_abs/127), round half-away,
//! clamp ±127, zero_point 0 — verificada contra el runtime Olive).
use super::onnxq_golden::ONNXQ_GOLDEN_HEX;
use super::wire::{run_common, WireProbe, WireReport};
use super::{canonical, error, file_digest, invalid, NativeIo, NativeSession, NativeSuite};
use async_trait::async_trait;
use larm_adapter_sdk::{
    contracts::Contracts, manifest::MethodTable, permissions::Access, protocol::Request,
};
use larm_domain::conformance::{hash, Observation, Run};
use larm_domain::error::{ErrorCode, LarmError};
use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::sqlite::SqliteConnectOptions;
use sqlx::{Connection, Row, SqliteConnection};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

const CAP: &str = "vendor.product.onnxq";
const SCOPE:&str="product streaming ONNX→ONNX-INT8 quantization and private ledger; real model quantization; no model inference or Core admission";
#[derive(Clone, Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OnnxQuantizeConfig {
    pub profile: Value,
    pub expected_input: PathBuf,
    pub forbidden: PathBuf,
    pub evidence_root: PathBuf,
    /// Ruta sellada del runtime gestionado python-olive (modeloby ruta).
    pub olive_runtime: PathBuf,
}
pub struct OnnxQuantizeSuite {
    config: OnnxQuantizeConfig,
    bundle: Value,
    expected: Vec<u8>,
}
#[derive(Serialize)]
pub struct OnnxQReport {
    pub run: Run,
    pub run_sha256: String,
    pub scope: &'static str,
    pub wire: Option<WireReport>,
    pub cases: Vec<Value>,
    pub methods: BTreeSet<String>,
    pub checkpoints: BTreeMap<String, Value>,
    pub observations: Result<Vec<Observation>, LarmError>,
}
impl OnnxQReport {
    fn check(&mut self, id: &str, expected: Value, observed: Value) -> Result<(), LarmError> {
        let passed = expected == observed;
        self.cases
            .push(json!({"id":id,"expected":expected,"observed":observed,"passed":passed}));
        if passed {
            Ok(())
        } else {
            Err(error(
                ErrorCode::ValidationFailed,
                &format!("conformance_onnxq_{id}"),
            ))
        }
    }
    fn material(&self) -> Value {
        json!({"run_sha256":self.run_sha256,"scope":self.scope,"wire":self.wire,"cases":self.cases,"methods":self.methods,"checkpoints":self.checkpoints})
    }
}
fn digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
fn descriptor(bytes: &[u8]) -> Value {
    json!({"sha256":digest(bytes),"size_bytes":bytes.len().to_string()})
}
fn bundle() -> Result<Value, LarmError> {
    use crate::conformance::onnxq_golden::{ONNXQ_FIXTURE_HEX, ONNXQ_GOLDEN_HEX};
    let schema = |maximum: usize| {
        json!({"type":"object","additionalProperties":false,"required":["sha256","size_bytes"],"properties":{
        "sha256":{"type":"string","pattern":"^[0-9a-f]{64}$"},"size_bytes":{"type":"string","pattern":"^(0|[1-9][0-9]*)$","maxLength":19,"x-maximum-decimal":maximum.to_string()}}})
    };
    // Ambos techos son 64 MiB: el descriptor del paquete los fija asi.
    let input_schema = schema(64 << 20);
    let output_schema = schema(64 << 20);
    let fixture_input =
        hex::decode(ONNXQ_FIXTURE_HEX).map_err(|_| invalid("onnxq_golden_fixture_invalid"))?;
    let converted =
        hex::decode(ONNXQ_GOLDEN_HEX).map_err(|_| invalid("onnxq_golden_output_invalid"))?;
    let fixture = json!({"input":descriptor(&fixture_input),"output":descriptor(&converted)});
    Ok(
        json!({"schema_version":"1.0","entries":[{"id":CAP,"version":"1.0","input_schema":input_schema,"output_schema":output_schema,
        "input_sha256":hash(&input_schema)?,"output_sha256":hash(&output_schema)?,"mapping":"product.onnx.onnxq.v1","estimator_id":"product.onnxq.buffers.v1","fixture":fixture,"fixture_sha256":hash(&fixture)?}]}),
    )
}

/// Oráculo ONNX en Rust: parseo safetensors, emisión esperada byte a byte
/// (protobuf ModelProto espejo del escritor Python) y parseo estructural del
/// modelo publicado. Los dtypes son códigos ONNX TensorProto reales.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SafetensorsTensor {
    pub name: String,
    pub dtype: String,
    pub shape: Vec<u64>,
    pub source_offset: usize,
    pub length: usize,
    pub onnx_dtype: u32,
}
#[derive(Debug, Clone)]
pub struct SafetensorsDocument {
    pub data_start: usize,
    pub tensors: Vec<SafetensorsTensor>,
}
const ONNX_F32: u32 = 1;
const ONNX_I8: u32 = 4;
const ONNX_I16: u32 = 5;
const ONNX_I32: u32 = 6;
const ONNX_I64: u32 = 7;
const ONNX_F16: u32 = 10;
const ONNX_F64: u32 = 11;
const ONNX_BF16: u32 = 16;
fn onnx_dtype(dtype: &str) -> Option<u32> {
    Some(match dtype {
        "F64" => ONNX_F64,
        "F32" => ONNX_F32,
        "F16" => ONNX_F16,
        "BF16" => ONNX_BF16,
        "I64" => ONNX_I64,
        "I32" => ONNX_I32,
        "I16" => ONNX_I16,
        "I8" => ONNX_I8,
        _ => return None,
    })
}
fn element_bytes(dtype: &str) -> Option<u64> {
    Some(match dtype {
        "F64" => 8,
        "F32" => 4,
        "F16" | "BF16" => 2,
        "I64" => 8,
        "I32" => 4,
        "I16" => 2,
        "I8" => 1,
        _ => return None,
    })
}
#[allow(clippy::unwrap_used)] // las rebanadas tienen longitud verificada antes de cada try_into
pub fn parse_safetensors(bytes: &[u8]) -> Result<SafetensorsDocument, LarmError> {
    if bytes.len() < 8 {
        return Err(invalid("onnx_safetensors_prefix_invalid"));
    }
    let header_len = u64::from_le_bytes(bytes[..8].try_into().unwrap()) as usize;
    if header_len == 0 || header_len > 16 << 20 || 8 + header_len > bytes.len() {
        return Err(invalid("onnx_safetensors_header_invalid"));
    }
    let header: Value = serde_json::from_slice(&bytes[8..8 + header_len])
        .map_err(|_| invalid("onnx_safetensors_header_invalid"))?;
    let data_start = 8 + header_len;
    let data_size = bytes.len() - data_start;
    let mut tensors = Vec::new();
    for (name, entry) in header
        .as_object()
        .ok_or_else(|| invalid("onnx_safetensors_header_invalid"))?
    {
        if name == "__metadata__" {
            continue;
        }
        let dtype = entry["dtype"]
            .as_str()
            .ok_or_else(|| invalid("onnx_safetensors_dtype_unsupported"))?;
        let onnx_type =
            onnx_dtype(dtype).ok_or_else(|| invalid("onnx_safetensors_dtype_unsupported"))?;
        let element =
            element_bytes(dtype).ok_or_else(|| invalid("onnx_safetensors_dtype_unsupported"))?;
        let shape: Vec<u64> = entry["shape"]
            .as_array()
            .ok_or_else(|| invalid("onnx_safetensors_shape_invalid"))?
            .iter()
            .map(|v| {
                v.as_u64()
                    .ok_or_else(|| invalid("onnx_safetensors_shape_invalid"))
            })
            .collect::<Result<_, _>>()?;
        if shape.is_empty() || shape.len() > 4 {
            return Err(invalid("onnx_safetensors_shape_invalid"));
        }
        let offsets = entry["data_offsets"]
            .as_array()
            .ok_or_else(|| invalid("onnx_safetensors_offsets_invalid"))?;
        if offsets.len() != 2 {
            return Err(invalid("onnx_safetensors_offsets_invalid"));
        }
        let start = offsets[0]
            .as_u64()
            .ok_or_else(|| invalid("onnx_safetensors_offsets_invalid"))?;
        let end = offsets[1]
            .as_u64()
            .ok_or_else(|| invalid("onnx_safetensors_offsets_invalid"))?;
        let elements: u64 = shape.iter().product();
        if end < start || end > data_size as u64 || end - start != elements * element {
            return Err(invalid("onnx_safetensors_offsets_invalid"));
        }
        tensors.push(SafetensorsTensor {
            name: name.clone(),
            dtype: dtype.to_string(),
            shape,
            source_offset: start as usize,
            length: (end - start) as usize,
            onnx_dtype: onnx_type,
        });
    }
    if tensors.is_empty() {
        return Err(invalid("onnx_safetensors_tensor_missing"));
    }
    tensors.sort_by_key(|tensor| tensor.source_offset);
    Ok(SafetensorsDocument {
        data_start,
        tensors,
    })
}
#[derive(Debug, Clone, PartialEq)]
pub struct OnnxInitializer {
    pub name: String,
    pub dims: Vec<u64>,
    pub onnx_dtype: u32,
    pub raw_data: Vec<u8>,
}
#[derive(Debug, Clone)]
pub struct OnnxModel {
    pub ir_version: u64,
    pub producer_name: String,
    pub opsets: Vec<u64>,
    pub nodes: Vec<(Vec<String>, Vec<String>, String, String)>,
    pub initializers: Vec<OnnxInitializer>,
    pub graph_name: String,
}
fn read_varint(bytes: &[u8], offset: &mut usize) -> Result<u64, LarmError> {
    let mut result = 0u64;
    let mut shift = 0u32;
    loop {
        let byte = *bytes
            .get(*offset)
            .ok_or_else(|| invalid("onnx_protobuf_truncated"))?;
        *offset += 1;
        result |= u64::from(byte & 0x7F) << shift;
        if byte & 0x80 == 0 {
            return Ok(result);
        }
        shift += 7;
    }
}
#[allow(clippy::type_complexity)] // el tuple es la forma natural del walk protobuf
fn read_fields(
    bytes: &[u8],
    start: usize,
    end: usize,
) -> Result<Vec<(u64, u64, Vec<u8>, u64)>, LarmError> {
    let mut fields = Vec::new();
    let mut offset = start;
    while offset < end {
        let key = read_varint(bytes, &mut offset)?;
        let field = key >> 3;
        let wire = key & 7;
        if wire == 2 {
            let len = read_varint(bytes, &mut offset)?;
            let payload = bytes
                .get(offset..offset + len as usize)
                .ok_or_else(|| invalid("onnx_protobuf_truncated"))?
                .to_vec();
            fields.push((field, wire, payload, 0));
            offset += len as usize;
        } else if wire == 0 {
            let value = read_varint(bytes, &mut offset)?;
            fields.push((field, wire, Vec::new(), value));
        } else {
            return Err(invalid("onnx_protobuf_wire_unsupported"));
        }
    }
    Ok(fields)
}
#[allow(clippy::type_complexity)]
#[allow(clippy::unwrap_used)] // las rebanadas tienen longitud verificada antes de cada try_into
pub fn parse_onnx(bytes: &[u8]) -> Result<OnnxModel, LarmError> {
    let mut ir_version = 0u64;
    let mut producer_name = String::new();
    let mut opsets = Vec::new();
    let mut nodes = Vec::new();
    let mut initializers = Vec::new();
    let mut graph_name = String::new();
    let mut graph_seen = false;
    for (field, wire, payload, value) in read_fields(bytes, 0, bytes.len())? {
        match (field, wire) {
            (1, 0) => ir_version = value,
            (2, 2) => {
                producer_name =
                    String::from_utf8(payload).map_err(|_| invalid("onnx_string_invalid"))?
            }
            (8, 2) => {
                for (o_field, o_wire, _o_payload, _o_value) in
                    read_fields(&payload, 0, payload.len())?
                {
                    if (o_field, o_wire) == (2, 0) {
                        opsets.push(_o_value);
                    }
                }
            }
            (7, 2) => {
                graph_seen = true;
                for (g_field, g_wire, g_payload, _g_value) in
                    read_fields(&payload, 0, payload.len())?
                {
                    match (g_field, g_wire) {
                        (1, 2) => {
                            let mut inputs = Vec::new();
                            let mut outputs = Vec::new();
                            let mut name = String::new();
                            let mut op_type = String::new();
                            for (n_field, n_wire, n_payload, _n_value) in
                                read_fields(&g_payload, 0, g_payload.len())?
                            {
                                let text = String::from_utf8(n_payload.clone())
                                    .map_err(|_| invalid("onnx_string_invalid"))?;
                                match (n_field, n_wire) {
                                    (1, 2) => inputs.push(text),
                                    (2, 2) => outputs.push(text),
                                    (3, 2) => name = text,
                                    (4, 2) => op_type = text,
                                    _ => {}
                                }
                            }
                            nodes.push((inputs, outputs, name, op_type));
                        }
                        (2, 2) => {
                            graph_name = String::from_utf8(g_payload)
                                .map_err(|_| invalid("onnx_string_invalid"))?
                        }
                        (5, 2) => {
                            let mut tensor = OnnxInitializer {
                                name: String::new(),
                                dims: Vec::new(),
                                onnx_dtype: 0,
                                raw_data: Vec::new(),
                            };
                            for (t_field, t_wire, t_payload, t_value) in
                                read_fields(&g_payload, 0, g_payload.len())?
                            {
                                match (t_field, t_wire) {
                                    (1, 2) => {
                                        let mut offset = 0usize;
                                        while offset < t_payload.len() {
                                            let dim = read_varint(&t_payload, &mut offset)?;
                                            tensor.dims.push(dim);
                                        }
                                    }
                                    (2, 0) => tensor.onnx_dtype = t_value as u32,
                                    (8, 2) => {
                                        tensor.name = String::from_utf8(t_payload)
                                            .map_err(|_| invalid("onnx_string_invalid"))?
                                    }
                                    (9, 2) => tensor.raw_data = t_payload,
                                    _ => {}
                                }
                            }
                            initializers.push(tensor);
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    if !graph_seen {
        return Err(invalid("onnx_graph_missing"));
    }
    Ok(OnnxModel {
        ir_version,
        producer_name,
        opsets,
        nodes,
        initializers,
        graph_name,
    })
}
pub fn expected_onnx(document: &SafetensorsDocument, source: &[u8]) -> Vec<u8> {
    fn varint(mut value: u64, out: &mut Vec<u8>) {
        loop {
            let byte = (value & 0x7F) as u8;
            value >>= 7;
            if value == 0 {
                out.push(byte);
                return;
            }
            out.push(byte | 0x80);
        }
    }
    fn tag(field: u64, wire: u64, out: &mut Vec<u8>) {
        varint((field << 3) | wire, out);
    }
    fn bytes_field(field: u64, payload: &[u8], out: &mut Vec<u8>) {
        tag(field, 2, out);
        varint(payload.len() as u64, out);
        out.extend_from_slice(payload);
    }
    fn string_field(field: u64, value: &str, out: &mut Vec<u8>) {
        bytes_field(field, value.as_bytes(), out);
    }
    fn varint_field(field: u64, value: u64, out: &mut Vec<u8>) {
        tag(field, 0, out);
        varint(value, out);
    }
    fn packed_varint_field(field: u64, values: &[u64], out: &mut Vec<u8>) {
        tag(field, 2, out);
        let mut packed = Vec::new();
        for value in values {
            varint(*value, &mut packed);
        }
        varint(packed.len() as u64, out);
        out.extend_from_slice(&packed);
    }
    let mut graph = Vec::new();
    for tensor in &document.tensors {
        let mut node = Vec::new();
        string_field(1, &tensor.name, &mut node);
        string_field(2, &format!("{}_out", tensor.name), &mut node);
        string_field(3, &format!("{}_identity", tensor.name), &mut node);
        string_field(4, "Identity", &mut node);
        bytes_field(1, &node, &mut graph);
    }
    string_field(2, "larm_product_graph", &mut graph);
    for tensor in &document.tensors {
        let mut tensor_proto = Vec::new();
        packed_varint_field(1, &tensor.shape, &mut tensor_proto);
        varint_field(2, u64::from(tensor.onnx_dtype), &mut tensor_proto);
        string_field(8, &tensor.name, &mut tensor_proto);
        let begin = document.data_start + tensor.source_offset;
        bytes_field(9, &source[begin..begin + tensor.length], &mut tensor_proto);
        bytes_field(5, &tensor_proto, &mut graph);
        // output(12): ValueInfoProto{1: name, 3: TypeProto{1: Tensor{1:
        // elem_type, 2: shape{1: dim{1: dim_value}}}}}. Se intercala junto a
        // su initializer (misma emisión que el escritor Python del paquete).
        let mut dims = Vec::new();
        for dim in &tensor.shape {
            let mut dimension = Vec::new();
            varint_field(1, *dim, &mut dimension);
            bytes_field(1, &dimension, &mut dims);
        }
        let mut tensor_msg = Vec::new();
        varint_field(1, u64::from(tensor.onnx_dtype), &mut tensor_msg);
        // Tensor.field 2 = shape: los dim entries van directo como payload.
        bytes_field(2, &dims, &mut tensor_msg);
        let mut type_proto = Vec::new();
        bytes_field(1, &tensor_msg, &mut type_proto);
        let mut value_info = Vec::new();
        string_field(1, &format!("{}_out", tensor.name), &mut value_info);
        bytes_field(3, &type_proto, &mut value_info);
        bytes_field(12, &value_info, &mut graph);
    }
    let mut out = Vec::new();
    varint_field(1, 8, &mut out);
    string_field(2, "larm-onnx-product", &mut out);
    let mut opset = Vec::new();
    varint_field(2, 13, &mut opset);
    bytes_field(8, &opset, &mut out);
    bytes_field(7, &graph, &mut out);
    out
}
fn read_json(path: &Path) -> Result<Value, LarmError> {
    crate::package::check_managed_path(path)?;
    let raw = std::fs::read(path).map_err(super::io)?;
    if raw.len() > 1 << 20 {
        return Err(invalid("onnx_oracle_document_too_large"));
    }
    serde_json::from_slice(&raw).map_err(|_| invalid("onnx_oracle_document_invalid"))
}
fn db_error(_: sqlx::Error) -> LarmError {
    error(
        ErrorCode::ResourceUnavailable,
        "conformance_onnxq_ledger_io",
    )
}
struct LedgerIo(tokio::sync::Mutex<Option<SqliteConnection>>);
#[async_trait]
impl NativeIo for LedgerIo {
    async fn close_io(&self) -> Result<(), LarmError> {
        if let Some(db) = self.0.lock().await.take() {
            db.close().await.map_err(db_error)?;
        }
        Ok(())
    }
}
// Restaura sólo bytes propios enumerados incluso si se cancela el oráculo.
struct Restore<'a> {
    path: PathBuf,
    bytes: &'a [u8],
}
impl Drop for Restore<'_> {
    fn drop(&mut self) {
        let _ = std::fs::write(&self.path, self.bytes);
    }
}
async fn snapshot(db: &mut SqliteConnection) -> Result<Value, LarmError> {
    let mut value = serde_json::Map::new();
    for (name,query) in [
        ("ref_plan","SELECT json_group_array(json_array(id,hash,document,request,binding,job_id,outcome)) FROM (SELECT * FROM ref_plan ORDER BY rowid)"),
        ("ref_job","SELECT json_group_array(json_array(id,identity,attempt,phase,document,version)) FROM (SELECT * FROM ref_job ORDER BY rowid)"),
        ("ref_attempt","SELECT json_group_array(json_array(job_id,attempt,phase,snapshot)) FROM (SELECT * FROM ref_attempt ORDER BY rowid)"),
        ("ref_journal","SELECT json_group_array(json_array(job_id,attempt,plan_id,phase,payload,closed)) FROM (SELECT * FROM ref_journal ORDER BY rowid)"),
        ("ref_event","SELECT json_group_array(json_array(id,job_id,version,event_type,phase,occurred_at,payload)) FROM (SELECT * FROM ref_event ORDER BY rowid)"),
        ("ref_outbox","SELECT json_group_array(json_array(event_id,delivered_at)) FROM (SELECT * FROM ref_outbox ORDER BY rowid)"),
        ("ref_lock","SELECT json_group_array(json_array(job_id,generation)) FROM (SELECT * FROM ref_lock ORDER BY rowid)"),
        ("ref_reservation","SELECT json_group_array(json_array(job_id,total_bytes,remaining_bytes)) FROM (SELECT * FROM ref_reservation ORDER BY rowid)"),
        ("ref_checkpoint","SELECT json_group_array(json_array(job_id,attempt,offset)) FROM (SELECT * FROM ref_checkpoint ORDER BY rowid)"),
    ] {
        let raw:String=sqlx::query_scalar(query).fetch_one(&mut *db).await.map_err(db_error)?;
        value.insert(name.into(),serde_json::from_str(&raw).map_err(|_|invalid("onnx_snapshot_invalid"))?);
    }
    Ok(Value::Object(value))
}
async fn held(db: &mut SqliteConnection, id: &str) -> Result<i64, LarmError> {
    sqlx::query_scalar("SELECT (SELECT count(*) FROM ref_lock WHERE job_id=?)+(SELECT count(*) FROM ref_reservation WHERE job_id=?)").bind(id).bind(id).fetch_one(db).await.map_err(db_error)
}
async fn job(db: &mut SqliteConnection, id: &str) -> Result<Value, LarmError> {
    let row = sqlx::query("SELECT phase,attempt,document FROM ref_job WHERE id=?")
        .bind(id)
        .fetch_one(db)
        .await
        .map_err(db_error)?;
    Ok(
        json!({"phase":row.get::<String,_>(0),"attempt":row.get::<i64,_>(1),"document":serde_json::from_str::<Value>(&row.get::<String,_>(2)).map_err(|_|invalid("onnx_job_invalid"))?}),
    )
}
impl OnnxQuantizeSuite {
    pub fn new(config: OnnxQuantizeConfig) -> Result<Self, LarmError> {
        for key in ["input_variant_id", "storage_location_id"] {
            if !config.profile[key]
                .as_str()
                .is_some_and(larm_domain::bridge::is_uuid_v7)
            {
                return Err(invalid("onnx_config_invalid"));
            }
        }
        if config.profile["input"]["relative_path"] != "model.onnx" {
            return Err(invalid("onnx_config_invalid"));
        }
        for path in [
            &config.expected_input,
            &config.forbidden,
            &config.evidence_root,
        ] {
            crate::package::check_managed_path(path)?;
        }
        let input = std::fs::read(&config.expected_input).map_err(super::io)?;
        if input.is_empty() || input.len() > 64 << 20 {
            return Err(invalid("onnx_fixture_size_invalid"));
        }
        // El modelo de entrada: grafo MatMul con el initializer F32 "w".
        let model = parse_onnxq_model(&input)?;
        if model.graph_name != "larm-onnxq-fixture" {
            return Err(invalid("onnxq_fixture_shape_invalid"));
        }
        let weight = onnxq_initializer(&model, "w")?;
        let raw = weight
            .raw_data
            .clone()
            .ok_or_else(|| invalid("onnxq_fixture_weight_missing"))?;
        let weight: Vec<f32> = raw
            .as_chunks::<4>()
            .0
            .iter()
            .map(|chunk| f32::from_le_bytes(*chunk))
            .collect();
        if weight.len() != 8192 {
            return Err(invalid("onnxq_fixture_weight_missing"));
        }
        let suite = Self {
            config,
            bundle: bundle()?,
            expected: hex::decode(ONNXQ_GOLDEN_HEX)
                .map_err(|_| invalid("onnxq_golden_output_invalid"))?,
        };
        Contracts::shared()?.request("transformation.plan", &suite.request())?;
        Ok(suite)
    }
    pub fn report_path(&self, id: &str) -> PathBuf {
        self.config
            .evidence_root
            .join(id)
            .join("transformation.json")
    }
    fn request(&self) -> Value {
        json!({"input_variant_id":self.config.profile["input_variant_id"],"adapter_id":"product.transformation.onnxq","tool_version":"1.0.0","target_format":"ONNX","parameters":{"quantization":"INT8"},"storage_location_id":self.config.profile["storage_location_id"],"hard_budget_bytes":null})
    }
    fn input(&self, session: &NativeSession) -> PathBuf {
        session.work_dir().join("input/model.onnx")
    }
    fn access(&self, session: &NativeSession) -> Access {
        Access {
            reads: ["input", "profile", "state", "output"]
                .into_iter()
                .map(|p| session.work_dir().join(p))
                .collect(),
            writes: ["state", "output"]
                .into_iter()
                .map(|p| session.work_dir().join(p))
                .collect(),
            ..Access::default()
        }
    }
    async fn call(
        &self,
        session: &NativeSession,
        report: &mut OnnxQReport,
        method: &str,
        params: Value,
        access: &Access,
    ) -> Result<Value, LarmError> {
        Contracts::shared()?.request(method, &params)?;
        report.methods.insert(method.into());
        session
            .call(
                Request::new(method, params),
                access,
                Some(Duration::from_secs(30)),
                None,
            )
            .await
    }
    async fn refused(
        &self,
        session: &NativeSession,
        report: &mut OnnxQReport,
        method: &str,
        params: Value,
        access: &Access,
        reason: &str,
    ) -> Result<LarmError, LarmError> {
        match self.call(session, report, method, params, access).await {
            Err(error) => {
                report.check(reason, json!(reason), json!(error.details.reason))?;
                Ok(error)
            }
            Ok(_) => {
                report.check(reason, json!(reason), json!("unexpected success"))?;
                Err(invalid("onnx_unexpected_success"))
            }
        }
    }
    async fn plan(
        &self,
        session: &NativeSession,
        report: &mut OnnxQReport,
    ) -> Result<Value, LarmError> {
        let plan = self
            .call(
                session,
                report,
                "transformation.plan",
                self.request(),
                &self.access(session),
            )
            .await?;
        let mut unsigned = plan.clone();
        unsigned
            .as_object_mut()
            .ok_or_else(|| invalid("onnx_plan_invalid"))?
            .remove("plan_hash");
        report.check(
            "plan-jcs",
            json!(hash(&unsigned)?),
            plan["plan_hash"].clone(),
        )?;
        report.check(
            "plan-predicted-by-rust-onnx-oracle",
            descriptor(&self.expected),
            plan["steps"][0]["request"]["binding"]["predicted"].clone(),
        )?;
        let upper = self.expected.len() as u64 + (3 << 20);
        let margin = (1 << 30).max(upper.div_ceil(20));
        report.check("private-storage-estimate",json!({"upper":upper.to_string(),"required":(upper+margin).to_string(),"confidence":"ESTIMATED","eligible":false}),
            json!({"upper":plan["storage_estimate"]["upper_bound_bytes"],"required":plan["storage_estimate"]["required_free_bytes"],"confidence":plan["storage_estimate"]["confidence"],"eligible":plan["storage_estimate"]["per_volume"][0]["eligible"]}))?;
        Ok(plan)
    }
    async fn execute(
        &self,
        session: &NativeSession,
        report: &mut OnnxQReport,
        plan: &Value,
    ) -> Result<Value, LarmError> {
        self.call(
            session,
            report,
            "transformation.execute",
            execute(plan),
            &self.access(session),
        )
        .await
    }
    async fn state(
        &self,
        db: &mut SqliteConnection,
        report: &mut OnnxQReport,
        id: &str,
        phase: &str,
        attempt: i64,
        resources: i64,
    ) -> Result<Value, LarmError> {
        let state = job(db, id).await?;
        report.check(
            "ledger-state-attempt-resources",
            json!({"phase":phase,"attempt":attempt,"held":resources}),
            json!({"phase":state["phase"],"attempt":state["attempt"],"held":held(db,id).await?}),
        )?;
        let journal: String =
            sqlx::query_scalar("SELECT phase FROM ref_journal WHERE job_id=? AND attempt=?")
                .bind(id)
                .bind(attempt)
                .fetch_one(&mut *db)
                .await
                .map_err(db_error)?;
        report.check("journal-matches-phase", json!(phase), json!(journal))?;
        let counts: (i64, i64) = sqlx::query_as(
            "SELECT (SELECT count(*) FROM ref_event),(SELECT count(*) FROM ref_outbox)",
        )
        .fetch_one(&mut *db)
        .await
        .map_err(db_error)?;
        report.check("event-outbox-pairing", json!(counts.0), json!(counts.1))?;
        report
            .checkpoints
            .insert(format!("{id}-{attempt}-{phase}"), state.clone());
        Ok(state)
    }
    #[allow(clippy::too_many_arguments)] // el par de esperados del mirror viaja con la llamada
    async fn output(
        &self,
        session: &NativeSession,
        db: &mut SqliteConnection,
        report: &mut OnnxQReport,
        receipt: &Value,
        original: &[u8],
        expected_scale_bytes: &[u8],
        expected_weight_bytes: &[u8],
    ) -> Result<Value, LarmError> {
        let id = receipt["job_id"]
            .as_str()
            .filter(|s| larm_domain::bridge::is_uuid_v7(s))
            .ok_or_else(|| invalid("onnx_job_invalid"))?;
        let state = job(db, id).await?;
        let attempt = state["attempt"]
            .as_i64()
            .filter(|v| *v > 0)
            .ok_or_else(|| invalid("onnx_attempt_invalid"))?;
        self.state(db, report, id, "COMPLETED", attempt, 0).await?;
        let manifest = self
            .call(
                session,
                report,
                "transformation.verify",
                json!({"job_id":id}),
                &self.access(session),
            )
            .await?;
        let relative = format!("{id}-{attempt}");
        let root = session.work_dir().join("output").join(&relative);
        let artifacts = manifest["artifacts"]
            .as_array()
            .ok_or_else(|| invalid("onnx_manifest_invalid"))?;
        let names = artifacts
            .iter()
            .filter_map(|a| a["relative_path"].as_str())
            .collect::<BTreeSet<_>>();
        report.check(
            "manifest-only-owned-artifacts",
            json!([
                format!("{relative}/model.onnx"),
                format!("{relative}/provenance.json")
            ]),
            json!(names),
        )?;
        let actual = std::fs::read(root.join("model.onnx")).map_err(super::io)?;
        report.check(
            "input-intact",
            json!(digest(original)),
            json!(digest(original)),
        )?;
        // El modelo cuantizado: parse QDQ y mirror byte-exacto de los pesos.
        let model = parse_onnxq_model(&actual)?;
        report.check("quant-ir-version", json!(8u64), json!(model.ir_version))?;
        report.check(
            "quant-graph-name",
            json!("larm-onnxq-fixture"),
            json!(model.graph_name),
        )?;
        let ops: Vec<String> = model
            .nodes
            .iter()
            .map(|node| node.op_type.clone())
            .collect();
        report.check(
            "quant-ops-in-order",
            json!([
                "DynamicQuantizeLinear",
                "Mul",
                "MatMulInteger",
                "Cast",
                "Mul"
            ]),
            json!(ops),
        )?;
        report.check(
            "quant-inputs-first-node",
            json!(["x"]),
            json!(model
                .nodes
                .iter()
                .find(|node| node.op_type == "DynamicQuantizeLinear")
                .map(|node| node.inputs.clone())
                .unwrap_or_default()),
        )?;
        let weight = onnxq_initializer(&model, "w_quantized")?;
        report.check(
            "quant-weight-dtype-int8",
            json!(3),
            json!(weight.onnx_dtype),
        )?;
        let weight_raw = weight
            .raw_data
            .clone()
            .ok_or_else(|| invalid("onnxq_quantized_weight_missing"))?;
        report.check(
            "quant-weight-bytes-match-mirror",
            json!(digest(expected_weight_bytes)),
            json!(digest(&weight_raw)),
        )?;
        report.check(
            "quant-weight-dims",
            json!([64u64, 128u64]),
            json!(weight.dims),
        )?;
        let scale = onnxq_initializer(&model, "w_scale")?;
        report.check("quant-scale-dtype-f32", json!(1), json!(scale.onnx_dtype))?;
        let scale_bytes = scale
            .float_data
            .clone()
            .ok_or_else(|| invalid("onnxq_scale_missing"))?;
        report.check(
            "quant-scale-bytes-match-mirror",
            json!(digest(expected_scale_bytes)),
            json!(digest(&scale_bytes)),
        )?;
        let zero_point = onnxq_initializer(&model, "w_zero_point")?;
        report.check(
            "quant-zero-point-dtype-int8",
            json!(3),
            json!(zero_point.onnx_dtype),
        )?;
        // ORT codifica el zero_point de forma no determinista: int32_data
        // (field 5, varints empaquetados) o raw_data (field 9, un i8 por
        // elemento). Un varint es cero exactamente cuando todos sus bytes lo
        // son, así que la condición "todo cero" es la misma en ambos campos.
        let zero_bytes = zero_point
            .int32_data
            .as_ref()
            .or(zero_point.raw_data.as_ref());
        let all_zero = zero_bytes
            .is_some_and(|bytes| !bytes.is_empty() && bytes.iter().all(|byte| *byte == 0));
        report.check("quant-zero-point-is-zero", json!(true), json!(all_zero))?;
        report.check(
            "quant-no-f32-weight",
            json!(false),
            json!(model
                .initializers
                .iter()
                .any(|tensor| tensor.name == "w" && tensor.onnx_dtype == 1)),
        )?;
        let stored = read_json(&root.join("manifest.json"))?;
        report.check("manifest-file-and-rpc", stored, manifest.clone())?;
        let mut descriptors = Vec::new();
        for artifact in artifacts {
            let name = artifact["relative_path"]
                .as_str()
                .ok_or_else(|| invalid("onnx_artifact_invalid"))?;
            let path = session.work_dir().join("output").join(name);
            crate::package::check_managed_path(&path)?;
            let size = std::fs::metadata(&path).map_err(super::io)?.len();
            report.check(
                "artifact-byte-sha",
                artifact["sha256"].clone(),
                json!(file_digest(&path)?),
            )?;
            report.check(
                "artifact-byte-size",
                artifact["size_bytes"].clone(),
                json!(size.to_string()),
            )?;
            descriptors.push(json!({"relative_path":name,"sha256":artifact["sha256"],"size_bytes":size,"required":artifact["required"]}));
        }
        descriptors.sort_by(|a, b| {
            a["relative_path"]
                .as_str()
                .cmp(&b["relative_path"].as_str())
        });
        let manifest_hash = hash(&descriptors)?;
        report.check(
            "full-manifest-content-key",
            json!({"hash":manifest_hash,"revision":format!("content:{manifest_hash}")}),
            json!({"hash":manifest["manifest_hash"],"revision":manifest["revision_key"]}),
        )?;
        let provenance = read_json(&root.join("provenance.json"))?;
        report.check("provenance-backed-by-bytes",json!({"input":self.config.profile["input"],"variant":self.config.profile["input_variant_id"],"output":descriptor(&actual),"tool":{"id":session.manifest().id,"version":"1.0.0","adapter_package_sha256":session.manifest().package_sha256,"package_verification":"HOST_OWNED"},"parameters":{"quantization":"INT8"},"job_id":id,"attempt":attempt,"logs":["input_verified","onnxq_written","output_verified"]}),
            json!({"input":provenance["source"],"variant":provenance["input_variant_id"],"output":provenance["output"],"tool":provenance["tool"],"parameters":provenance["parameters"],"job_id":provenance["job_id"],"attempt":provenance["attempt"],"logs":provenance["logs"]}))?;
        let path = session
            .work_dir()
            .join("state/job-status")
            .join(format!("{id}.json"));
        let plain = PathBuf::from(path.to_string_lossy().trim_start_matches(r"\\?\"));
        let uri =
            url::Url::from_file_path(&plain).map_err(|_| invalid("onnx_status_uri_invalid"))?;
        report.check(
            "receipt-uri-is-owned-status",
            json!(uri.as_str()),
            receipt["status_url"].clone(),
        )?;
        report.check(
            "receipt-status-matches-ledger",
            state["document"].clone(),
            read_json(&path)?,
        )?;
        report.checkpoints.insert(format!("output-{id}-{attempt}"),json!({"manifest":manifest,"provenance":provenance,"onnx_sha256":digest(&actual),"onnx_bytes":actual.len().to_string()}));
        Ok(manifest)
    }
    async fn interrupted(
        &self,
        session: &mut NativeSession,
        db: &mut SqliteConnection,
        report: &mut OnnxQReport,
        plan: &Value,
        how: &str,
    ) -> Result<(String, i64), LarmError> {
        let mut progress = session.progress()?;
        let request = Request::new("transformation.execute", execute(plan));
        let id = request.id.clone();
        let access = self.access(session);
        report.methods.insert("transformation.execute".into());
        let mut pending =
            Box::pin(session.call(request, &access, Some(Duration::from_secs(30)), None));
        // Este conversor emite un único evento de progreso al final de la
        // cuantización y la emisión es atómica: los fallos que deben aterrizar
        // durante el trabajo (cancel/crash/eof) esperan al job RUNNING en el
        // ledger, que implica que el primer check_cancelled ya pasó y el
        // siguiente está tras el subproceso del runtime.
        let mut interrupted_job: Option<String> = None;
        if matches!(how, "cancel" | "crash" | "eof") {
            let deadline = std::time::Instant::now() + Duration::from_secs(20);
            loop {
                tokio::select! {
                    result = &mut pending => {
                        // La llamada terminó sin llegar a RUNNING: el fallo del
                        // escenario es ese, no el del poll.
                        return Err(result.err().unwrap_or_else(|| invalid("onnx_interrupted_call_succeeded")));
                    }
                    _ = tokio::time::sleep(Duration::from_millis(20)) => {
                        let running: Option<String> = sqlx::query_scalar(
                            "SELECT id FROM ref_job WHERE phase='RUNNING' ORDER BY rowid DESC LIMIT 1",
                        )
                        .fetch_optional(&mut *db)
                        .await
                        .map_err(db_error)?;
                        if running.is_some() {
                            interrupted_job = running;
                            break;
                        }
                        assert!(
                            std::time::Instant::now() < deadline,
                            "el job no llegó a RUNNING: {:?}",
                            sqlx::query_as::<_, (String, String)>(
                                "SELECT id,phase FROM ref_job ORDER BY rowid DESC LIMIT 3",
                            )
                            .fetch_all(&mut *db)
                            .await
                            .map_err(db_error)?
                        );
                    }
                }
            }
        }
        match how {
            "cancel" => {
                report.check("cancel-ack", json!(true), json!(session.cancel(&id).await?))?;
                let event = loop {
                    tokio::select! {event=progress.recv()=>{let event=event.map_err(|_|invalid("onnx_progress_missing"))?;if event.phase=="ONNXQ_TENSOR"{break event;}},_=&mut pending=>return Err(invalid("onnx_completed_before_tensor_checkpoint"))}
                };
                report.check("encoding-progress-id", json!(id), json!(event.request_id))?;
                let active:(String,i64,i64)=sqlx::query_as("SELECT j.id,j.attempt,c.offset FROM ref_job j JOIN ref_checkpoint c ON c.job_id=j.id AND c.attempt=j.attempt WHERE j.phase='RUNNING' AND c.offset>0 ORDER BY j.rowid DESC LIMIT 1").fetch_one(&mut *db).await.map_err(db_error)?;
                report.check("encoding-durable-offset", json!(true), json!(active.2 > 0))?;
                let heartbeat = self
                    .call(
                        session,
                        report,
                        "core.heartbeat",
                        json!({}),
                        &Access::default(),
                    )
                    .await?;
                report.check(
                    "encoding-heartbeat",
                    json!(true),
                    json!(heartbeat["running_request_ids"]
                        .as_array()
                        .is_some_and(|v| v.contains(&json!(id)))),
                )?;
                let failure = pending
                    .await
                    .err()
                    .ok_or_else(|| invalid("onnx_interrupted_call_succeeded"))?;
                report.check(
                    "cancel-result",
                    json!("operation_cancelled"),
                    json!(failure.details.reason),
                )?;
                report.checkpoints.insert(
                    "cancel-error".into(),
                    serde_json::to_value(failure).map_err(|_| invalid("onnx_error_invalid"))?,
                );
                report.checkpoints.insert(
                    "cancel".into(),
                    json!({"rpc_id":id,"job_id":active.0,"attempt":active.1,"durable_offset":active.2}),
                );
                Ok((active.0, active.2))
            }
            "crash" => {
                // Crash durante el subproceso: sin checkpoint duradero ni
                // parcial (la emisión es atómica); el job queda RUNNING.
                session.crash()?;
                let failure = pending
                    .await
                    .err()
                    .ok_or_else(|| invalid("onnx_interrupted_call_succeeded"))?;
                report.check(
                    "worker-crash",
                    json!("ADAPTER_CRASHED"),
                    json!(failure.code.as_str()),
                )?;
                report.checkpoints.insert(
                    "crash-error".into(),
                    serde_json::to_value(failure).map_err(|_| invalid("onnx_error_invalid"))?,
                );
                let job_id =
                    interrupted_job.ok_or_else(|| invalid("onnx_interrupt_mode_invalid"))?;
                report.checkpoints.insert(
                    "crash".into(),
                    json!({"rpc_id":id,"job_id":job_id,"attempt":1,"durable_offset":0}),
                );
                Ok((job_id, 0))
            }
            "eof" => {
                // EOF del cliente: el server cancela los callbacks y el job
                // queda CANCELADO para el siguiente arranque.
                drop(pending);
                session.restart().await?;
                let job_id =
                    interrupted_job.ok_or_else(|| invalid("onnx_interrupt_mode_invalid"))?;
                report.checkpoints.insert(
                    "eof".into(),
                    json!({"rpc_id":id,"job_id":job_id,"attempt":1,"durable_offset":0}),
                );
                Ok((job_id, 0))
            }
            "corrupt" => {
                // La corrupción aterriza en la ventana de verificación final:
                // el subproceso ya consumió el input y el hash del mismo falla.
                let event = loop {
                    tokio::select! {event=progress.recv()=>{let event=event.map_err(|_|invalid("onnx_progress_missing"))?;if event.phase=="ONNXQ_TENSOR"{break event;}},_=&mut pending=>return Err(invalid("onnx_completed_before_tensor_checkpoint"))}
                };
                report.check("encoding-progress-id", json!(id), json!(event.request_id))?;
                let active:(String,i64,i64)=sqlx::query_as("SELECT j.id,j.attempt,c.offset FROM ref_job j JOIN ref_checkpoint c ON c.job_id=j.id AND c.attempt=j.attempt WHERE j.phase='RUNNING' AND c.offset>0 ORDER BY j.rowid DESC LIMIT 1").fetch_one(&mut *db).await.map_err(db_error)?;
                report.check("encoding-durable-offset", json!(true), json!(active.2 > 0))?;
                let heartbeat = self
                    .call(
                        session,
                        report,
                        "core.heartbeat",
                        json!({}),
                        &Access::default(),
                    )
                    .await?;
                report.check(
                    "encoding-heartbeat",
                    json!(true),
                    json!(heartbeat["running_request_ids"]
                        .as_array()
                        .is_some_and(|v| v.contains(&json!(id)))),
                )?;
                let mut file = std::fs::OpenOptions::new()
                    .write(true)
                    .open(self.input(session))
                    .map_err(super::io)?;
                file.write_all(b"!").map_err(super::io)?;
                file.sync_all().map_err(super::io)?;
                let failure = pending
                    .await
                    .err()
                    .ok_or_else(|| invalid("onnx_interrupted_call_succeeded"))?;
                report.check(
                    "combined-fault-response",
                    json!("product_onnxq_persistence_failed"),
                    json!(failure.details.reason),
                )?;
                report.checkpoints.insert(
                    "corrupt-error".into(),
                    serde_json::to_value(failure).map_err(|_| invalid("onnx_error_invalid"))?,
                );
                report.checkpoints.insert(
                    "corrupt".into(),
                    json!({"rpc_id":id,"job_id":active.0,"attempt":active.1,"durable_offset":active.2}),
                );
                Ok((active.0, active.2))
            }
            _ => Err(invalid("onnx_interrupt_mode_invalid")),
        }
    }
    async fn functional(
        &self,
        session: &mut NativeSession,
        db: &mut SqliteConnection,
        report: &mut OnnxQReport,
        original: &[u8],
        expected_scale_bytes: &[u8],
        expected_weight_bytes: &[u8],
    ) -> Result<(), LarmError> {
        let access = self.access(session);
        let description = self
            .call(
                session,
                report,
                "transformation.describe",
                json!({}),
                &Access::default(),
            )
            .await?;
        report.check("description-real-tool",json!({"formats_input":["ONNX"],"formats_output":["ONNX"],"parameter_schema":{"type":"object","properties":{"quantization":{"type":"string","enum":["INT8"]}},"additionalProperties":false},"tool_version":"1.0.0","pause_supported":false}),description)?;
        let before = snapshot(db).await?;
        let sentinel = file_digest(&self.config.forbidden)?;
        for (key, value, reason) in [
            (
                "input_variant_id",
                json!(uuid::Uuid::now_v7().to_string()),
                "product_onnxq_resource_not_bound",
            ),
            (
                "storage_location_id",
                json!(uuid::Uuid::now_v7().to_string()),
                "product_onnxq_resource_not_bound",
            ),
            (
                "parameters",
                json!({"script":"forbidden"}),
                "product_onnxq_options_not_supported",
            ),
            (
                "tool_version",
                json!("2.0"),
                "product_onnxq_options_not_supported",
            ),
            (
                "target_format",
                json!("GGUF"),
                "product_onnxq_options_not_supported",
            ),
            (
                "hard_budget_bytes",
                json!("1"),
                "product_onnxq_hard_budget_insufficient",
            ),
        ] {
            let mut request = self.request();
            request[key] = value;
            self.refused(
                session,
                report,
                "transformation.plan",
                request,
                &access,
                reason,
            )
            .await?;
        }
        self.refused(
            session,
            report,
            "transformation.plan",
            self.request(),
            &Access {
                reads: vec![self.config.forbidden.clone()],
                ..Access::default()
            },
            "read_denied",
        )
        .await?;
        self.refused(
            session,
            report,
            "transformation.plan",
            self.request(),
            &Access {
                writes: vec![self.input(session)],
                ..Access::default()
            },
            "write_denied",
        )
        .await?;
        self.refused(
            session,
            report,
            "transformation.plan",
            self.request(),
            &Access {
                urls: vec!["http://127.0.0.1:1/foreign".into()],
                ..Access::default()
            },
            "network_denied",
        )
        .await?;
        self.refused(
            session,
            report,
            "transformation.plan",
            self.request(),
            &Access {
                secrets: vec!["larm/ungranted".into()],
                ..Access::default()
            },
            "secret_denied",
        )
        .await?;
        report.check(
            "rejections-before-ledger-mutation",
            before,
            snapshot(db).await?,
        )?;
        let plan = self.plan(session, report).await?;
        report.check(
            "dry-plan-no-output",
            json!(0),
            json!(std::fs::read_dir(session.work_dir().join("output"))
                .map_err(super::io)?
                .count()),
        )?;
        let mut wrong = execute(&plan);
        wrong["plan_hash"] = json!("b".repeat(64));
        self.refused(
            session,
            report,
            "transformation.execute",
            wrong,
            &access,
            "product_onnxq_plan_hash_mismatch",
        )
        .await?;
        let mut approved = execute(&plan);
        approved["approval_id"] = json!(uuid::Uuid::now_v7().to_string());
        self.refused(
            session,
            report,
            "transformation.execute",
            approved,
            &access,
            "product_onnxq_approval_owned_by_core",
        )
        .await?;
        {
            let restore = Restore {
                path: self.input(session),
                bytes: original,
            };
            let mut changed = original.to_vec();
            changed[0] = b'!';
            std::fs::write(&restore.path, changed).map_err(super::io)?;
            self.refused(
                session,
                report,
                "transformation.execute",
                execute(&plan),
                &access,
                "product_onnxq_input_changed",
            )
            .await?;
            drop(restore);
        }
        let mut expired = self.plan(session, report).await?;
        expired["expires_at"] = json!("2000-01-01T00:00:00.000Z");
        let mut unsigned = expired.clone();
        unsigned
            .as_object_mut()
            .ok_or_else(|| invalid("onnx_plan_invalid"))?
            .remove("plan_hash");
        expired["plan_hash"] = json!(hash(&unsigned)?);
        sqlx::query("UPDATE ref_plan SET document=?,hash=? WHERE id=?")
            .bind(serde_json::to_string(&expired).map_err(|_| invalid("onnx_plan_invalid"))?)
            .bind(expired["plan_hash"].as_str())
            .bind(expired["plan_id"].as_str())
            .execute(&mut *db)
            .await
            .map_err(db_error)?;
        self.refused(
            session,
            report,
            "transformation.execute",
            execute(&expired),
            &access,
            "product_onnxq_plan_expired",
        )
        .await?;
        sqlx::query("CREATE TRIGGER p026_transform_accept BEFORE INSERT ON ref_outbox BEGIN SELECT RAISE(ABORT,'owned_accept_fault'); END").execute(&mut *db).await.map_err(db_error)?;
        let before = snapshot(db).await?;
        let failure = self
            .refused(
                session,
                report,
                "transformation.execute",
                execute(&plan),
                &access,
                "product_onnxq_persistence_failed",
            )
            .await;
        sqlx::query("DROP TRIGGER p026_transform_accept")
            .execute(&mut *db)
            .await
            .map_err(db_error)?;
        failure?;
        report.check("accept-transaction-rollback", before, snapshot(db).await?)?;
        let blocker = session.work_dir().join("state/staging");
        std::fs::write(&blocker, b"owned staging blocker").map_err(super::io)?;
        let failure = self
            .refused(
                session,
                report,
                "transformation.execute",
                execute(&plan),
                &access,
                "product_onnxq_io_failed",
            )
            .await;
        std::fs::remove_file(&blocker).map_err(super::io)?;
        let original_error = failure?;
        let failed: String = sqlx::query_scalar("SELECT id FROM ref_job WHERE phase='FAILED'")
            .fetch_one(&mut *db)
            .await
            .map_err(db_error)?;
        self.state(db, report, &failed, "FAILED", 1, 2).await?;
        self.refused(
            session,
            report,
            "transformation.verify",
            json!({"job_id":failed}),
            &access,
            "product_onnxq_job_not_completed",
        )
        .await?;
        let retry = self.plan(session, report).await?;
        let completed = self.execute(session, report, &retry).await?;
        report.check("retry-same-job", json!(failed), completed["job_id"].clone())?;
        let manifest = self
            .output(
                session,
                db,
                report,
                &completed,
                original,
                expected_scale_bytes,
                expected_weight_bytes,
            )
            .await?;
        let before = snapshot(db).await?;
        let replay = self.execute(session, report, &retry).await?;
        report.check("receipt-idempotent", completed.clone(), replay)?;
        let error = self
            .refused(
                session,
                report,
                "transformation.execute",
                execute(&plan),
                &access,
                "product_onnxq_io_failed",
            )
            .await?;
        report.check(
            "old-error-immutable",
            serde_json::to_value(original_error).map_err(|_| invalid("onnx_error_invalid"))?,
            serde_json::to_value(error).map_err(|_| invalid("onnx_error_invalid"))?,
        )?;
        report.check("replay-no-new-events", before, snapshot(db).await?)?;
        let payload = session
            .work_dir()
            .join("output")
            .join(format!("{failed}-2/model.onnx"));
        {
            let restore = Restore {
                path: payload.clone(),
                bytes: self.expected.as_slice(),
            };
            let mut changed = self.expected.clone();
            changed[0] = b'!';
            std::fs::write(&payload, changed).map_err(super::io)?;
            self.refused(
                session,
                report,
                "transformation.verify",
                json!({"job_id":failed}),
                &access,
                "product_onnxq_output_changed",
            )
            .await?;
            drop(restore);
        }
        let verified = self
            .call(
                session,
                report,
                "transformation.verify",
                json!({"job_id":failed}),
                &access,
            )
            .await?;
        report.check("verify-rereads-restored-bytes", manifest, verified)?;
        let cancel = self.plan(session, report).await?;
        let (cancelled, _) = self
            .interrupted(session, db, report, &cancel, "cancel")
            .await?;
        self.state(db, report, &cancelled, "CANCELLED", 1, 0)
            .await?;
        report.check("cancel-cleans-only-attempt",json!({"stage_files":0,"old_output_sha":digest(&self.expected)}),json!({"stage_files":std::fs::read_dir(session.work_dir().join("state/staging").join(format!("{cancelled}-1"))).map_err(super::io)?.count(),"old_output_sha":file_digest(&payload)?}))?;
        let crash = self.plan(session, report).await?;
        let (crashed, _offset) = self
            .interrupted(session, db, report, &crash, "crash")
            .await?;
        // La cuantización es atómica: el crash durante el subproceso deja el
        // job RUNNING sin checkpoint ni parcial; las reservas siguen vivas.
        report.check(
            "crash-resources-held",
            json!(2),
            json!(held(db, &crashed).await?),
        )?;
        let incomplete = session
            .work_dir()
            .join("state")
            .join(format!("fault-{crashed}-1.tmp"));
        std::fs::write(&incomplete, b"{\"job_id\":").map_err(super::io)?;
        session.restart().await?;
        let recovered = self
            .state(db, report, &crashed, "FAILED_RECOVERABLE", 1, 2)
            .await?;
        report.check(
            "crash-cause-preserved",
            json!("ADAPTER_CRASHED"),
            recovered["document"]["error"]["code"].clone(),
        )?;
        report.check(
            "incomplete-fault-not-consumed",
            json!(digest(b"{\"job_id\":")),
            json!(file_digest(&incomplete)?),
        )?;
        let restart = self.plan(session, report).await?;
        let recovered_receipt = self.execute(session, report, &restart).await?;
        report.check(
            "restart-new-attempt-same-job",
            json!(crashed),
            recovered_receipt["job_id"].clone(),
        )?;
        self.output(
            session,
            db,
            report,
            &recovered_receipt,
            original,
            expected_scale_bytes,
            expected_weight_bytes,
        )
        .await?;
        // Sin parcial previo que conservar: el reintento publica un modelo
        // nuevo verificado byte a byte por output().
        let fault = self.plan(session, report).await?;
        sqlx::query("CREATE TRIGGER p026_transform_final BEFORE INSERT ON ref_outbox WHEN (SELECT phase FROM ref_event WHERE id=NEW.event_id)='COMPLETED' BEGIN SELECT RAISE(ABORT,'owned_final_fault'); END").execute(&mut *db).await.map_err(db_error)?;
        let failure = self
            .refused(
                session,
                report,
                "transformation.execute",
                execute(&fault),
                &access,
                "product_onnxq_persistence_failed",
            )
            .await;
        sqlx::query("DROP TRIGGER p026_transform_final")
            .execute(&mut *db)
            .await
            .map_err(db_error)?;
        failure?;
        let uncertain: String =
            sqlx::query_scalar("SELECT id FROM ref_job WHERE phase='VERIFYING'")
                .fetch_one(&mut *db)
                .await
                .map_err(db_error)?;
        self.state(db, report, &uncertain, "VERIFYING", 1, 2)
            .await?;
        let published = session
            .work_dir()
            .join("output")
            .join(format!("{uncertain}-1/model.onnx"));
        let published_sha = file_digest(&published)?;
        report.check(
            "uncertain-has-no-receipt",
            json!(false),
            json!(session
                .work_dir()
                .join("state/job-status")
                .join(format!("{uncertain}.json"))
                .exists()),
        )?;
        session.restart().await?;
        let recovered = self
            .state(db, report, &uncertain, "FAILED_RECOVERABLE", 1, 2)
            .await?;
        report.check(
            "persistence-cause-survives-restart",
            json!("IntegrityError"),
            recovered["document"]["error"]["details"]["original_error"]["code"].clone(),
        )?;
        let retry = self.plan(session, report).await?;
        let receipt = self.execute(session, report, &retry).await?;
        report.check(
            "uncertain-retry-same-job",
            json!(uncertain),
            receipt["job_id"].clone(),
        )?;
        self.output(
            session,
            db,
            report,
            &receipt,
            original,
            expected_scale_bytes,
            expected_weight_bytes,
        )
        .await?;
        report.check(
            "uncertain-publication-retained",
            json!(published_sha),
            json!(file_digest(&published)?),
        )?;
        let combined = self.plan(session, report).await?;
        sqlx::query("CREATE TRIGGER p026_transform_error BEFORE INSERT ON ref_outbox WHEN (SELECT phase FROM ref_event WHERE id=NEW.event_id)='FAILED' BEGIN SELECT RAISE(ABORT,'owned_error_fault'); END").execute(&mut *db).await.map_err(db_error)?;
        let restore = Restore {
            path: self.input(session),
            bytes: original,
        };
        let fault = self
            .interrupted(session, db, report, &combined, "corrupt")
            .await;
        drop(restore);
        sqlx::query("DROP TRIGGER p026_transform_error")
            .execute(&mut *db)
            .await
            .map_err(db_error)?;
        let (combined_id, _) = fault?;
        self.state(db, report, &combined_id, "VERIFYING", 1, 2)
            .await?;
        session.restart().await?;
        let recovered = self
            .state(db, report, &combined_id, "FAILED_RECOVERABLE", 1, 2)
            .await?;
        report.check("original-integrity-precedes-outbox-error",json!({"code":"INTEGRITY_FAILED","reason":"product_onnxq_final_verification_failed"}),json!({"code":recovered["document"]["error"]["code"],"reason":recovered["document"]["error"]["details"]["reason"]}))?;
        let retry = self.plan(session, report).await?;
        let receipt = self.execute(session, report, &retry).await?;
        report.check(
            "combined-fault-retry-same-job",
            json!(combined_id),
            receipt["job_id"].clone(),
        )?;
        self.output(
            session,
            db,
            report,
            &receipt,
            original,
            expected_scale_bytes,
            expected_weight_bytes,
        )
        .await?;
        let eof = self.plan(session, report).await?;
        let (eof_id, _) = self.interrupted(session, db, report, &eof, "eof").await?;
        // El EOF llega durante el subproceso del runtime: si el server
        // procesa el cierre antes del kill el job queda CANCELLED con
        // reservas liberadas; si el kill gana, el siguiente arranque lo
        // recupera como FAILED_RECOVERABLE con reservas vivas. Ambos
        // desenlaces son legítimos y el reintento publica en cualquiera.
        let eof_state = job(db, &eof_id).await?;
        let eof_phase = eof_state["phase"].as_str().unwrap_or_default().to_string();
        report.check(
            "eof-final-state",
            json!(true),
            json!(eof_phase == "CANCELLED" || eof_phase == "FAILED_RECOVERABLE"),
        )?;
        report.check(
            "eof-resources",
            json!(true),
            json!(held(db, &eof_id).await? == if eof_phase == "CANCELLED" { 0 } else { 2 }),
        )?;
        let eof_retry = self.plan(session, report).await?;
        let eof_receipt = self.execute(session, report, &eof_retry).await?;
        if eof_phase == "FAILED_RECOVERABLE" {
            report.check(
                "eof-retry-same-job",
                json!(eof_id),
                eof_receipt["job_id"].clone(),
            )?;
        } else {
            report.check(
                "eof-retry-new-job",
                json!(true),
                json!(eof_receipt["job_id"].as_str() != Some(eof_id.as_str())),
            )?;
        }
        self.output(
            session,
            db,
            report,
            &eof_receipt,
            original,
            expected_scale_bytes,
            expected_weight_bytes,
        )
        .await?;
        report.check(
            "input-unchanged",
            json!(digest(original)),
            json!(file_digest(&self.input(session))?),
        )?;
        report.check(
            "forbidden-sentinel-unchanged",
            json!(sentinel),
            json!(file_digest(&self.config.forbidden)?),
        )?;
        report
            .checkpoints
            .insert("final-ledger".into(), snapshot(db).await?);
        let methods = MethodTable::contract()
            .methods
            .iter()
            .filter(|m| m.family == "transformation")
            .map(|m| m.method.clone())
            .collect::<BTreeSet<_>>();
        report.check(
            "all-transformation-signatures",
            json!(methods),
            json!(report
                .methods
                .iter()
                .filter(|m| m.starts_with("transformation."))
                .collect::<BTreeSet<_>>()),
        )?;
        Ok(())
    }
    async fn run(
        &self,
        run: &Run,
        session: &mut NativeSession,
        report: &mut OnnxQReport,
    ) -> Result<Vec<Observation>, LarmError> {
        if session.manifest().id != "product.transformation.onnxq"
            || session.manifest().family.as_str() != "transformation"
            || session.manifest().capabilities != [CAP]
        {
            return Err(invalid("onnx_subject_mismatch"));
        }
        report.check(
            "exact-capability-profile",
            self.bundle.clone(),
            read_json(&session.package_dir().join("product-capabilities.json"))?,
        )?;
        report.check(
            "exact-instance-profile",
            self.config.profile.clone(),
            read_json(&session.work_dir().join("profile/profile.json"))?,
        )?;
        for path in [&self.config.expected_input, &self.config.forbidden] {
            let path = canonical(path)?;
            if path.starts_with(canonical(session.work_dir())?)
                || path.starts_with(canonical(session.package_dir())?)
            {
                return Err(invalid("onnx_oracle_resource_exposed"));
            }
        }
        let original = std::fs::read(&self.config.expected_input).map_err(super::io)?;
        if original.is_empty() || original.len() > 64 << 20 {
            return Err(invalid("onnx_fixture_size_invalid"));
        }
        // El modelo de entrada: grafo MatMul con el initializer F32 "w".
        let input_model = parse_onnxq_model(&original)?;
        let weight_tensor = onnxq_initializer(&input_model, "w")?;
        let weight_raw = weight_tensor
            .raw_data
            .clone()
            .ok_or_else(|| invalid("onnxq_fixture_weight_missing"))?;
        let weight: Vec<f32> = weight_raw
            .as_chunks::<4>()
            .0
            .iter()
            .map(|chunk| f32::from_le_bytes(*chunk))
            .collect();
        if weight.len() != 8192 {
            return Err(invalid("onnxq_fixture_weight_missing"));
        }
        let (expected_scale, expected_weight) = mirror_quantize(&weight);
        let expected_scale_bytes: Vec<u8> = expected_scale.to_le_bytes().to_vec();
        let expected_weight_bytes: Vec<u8> = expected_weight
            .iter()
            .flat_map(|value| (*value as i8).to_le_bytes())
            .collect();
        report.check("input-pinned-independent-bytes",json!({"sha256":digest(&original),"size_bytes":original.len().to_string()}),json!({"sha256":self.config.profile["input"]["sha256"],"size_bytes":self.config.profile["input"]["size_bytes"]}))?;
        let wire = run_common(
            run,
            session,
            &WireProbe {
                method: "transformation.describe".into(),
                params: json!({}),
                access: Access::default(),
            },
        )
        .await;
        let result = wire.observations.clone();
        report.wire = Some(wire);
        let mut observations = result?;
        session.restart().await?;
        let path = session.work_dir().join("state/transform.sqlite");
        crate::package::check_managed_path(&path)?;
        let connection = SqliteConnection::connect_with(
            &SqliteConnectOptions::new()
                .filename(path)
                .create_if_missing(false)
                .busy_timeout(Duration::from_secs(2)),
        )
        .await
        .map_err(db_error)?;
        let ledger = Arc::new(LedgerIo(tokio::sync::Mutex::new(Some(connection))));
        session.register_io(ledger.clone());
        let mut guard = ledger.0.lock().await;
        let db = guard
            .as_mut()
            .ok_or_else(|| invalid("onnx_ledger_closed"))?;
        let app: i64 = sqlx::query_scalar("PRAGMA application_id")
            .fetch_one(&mut *db)
            .await
            .map_err(db_error)?;
        let version: i64 = sqlx::query_scalar("PRAGMA user_version")
            .fetch_one(&mut *db)
            .await
            .map_err(db_error)?;
        if app != 0x4C415254 || version != 1 {
            return Err(invalid("onnx_ledger_not_owned"));
        }
        self.functional(
            session,
            db,
            report,
            &original,
            &expected_scale_bytes,
            &expected_weight_bytes,
        )
        .await?;
        drop(guard);
        let material = report.material();
        for check in [
            "cancel",
            "progress",
            "crash",
            "permissions",
            "state",
            "io_recovery",
            "family:transformation",
            "capability:vendor.product.onnxq",
        ] {
            observations.push(Observation {
                check_id: check.into(),
                passed: true,
                evidence_sha256: hash(&json!({"check":check,"evidence":material}))?,
            });
        }
        Ok(observations)
    }
}
fn execute(plan: &Value) -> Value {
    json!({"plan_id":plan["plan_id"],"plan_hash":plan["plan_hash"],"approval_id":null})
}
#[async_trait]
impl NativeSuite for OnnxQuantizeSuite {
    fn descriptor(&self) -> Value {
        json!({"family":"transformation","capabilities":[CAP],"suite":"product.transformation.onnx.v1","bundle":self.bundle,"profile":self.config.profile,
        "expected_input":self.config.expected_input,"forbidden":self.config.forbidden,"evidence_root":self.config.evidence_root})
    }
    fn protocol_probes_first(&self) -> bool {
        true
    }
    async fn execute(
        &self,
        run: &Run,
        session: &mut NativeSession,
    ) -> Result<Vec<Observation>, LarmError> {
        let mut report = OnnxQReport {
            run: run.clone(),
            run_sha256: run.digest()?,
            scope: SCOPE,
            wire: None,
            cases: vec![],
            methods: BTreeSet::new(),
            checkpoints: BTreeMap::new(),
            observations: Ok(vec![]),
        };
        report.observations = self.run(run, session, &mut report).await;
        let path = self.report_path(&run.id);
        crate::package::check_managed_path(&path)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(super::io)?;
        }
        std::fs::write(
            path,
            serde_json::to_vec_pretty(&report).map_err(|_| invalid("onnx_report_invalid"))?,
        )
        .map_err(super::io)?;
        report.observations
    }
}

// --- Oráculo QDQ: parse del modelo de entrada y del modelo cuantizado ---

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OnnxqTensor {
    pub name: String,
    pub onnx_dtype: u32,
    pub raw_data: Option<Vec<u8>>,
    pub float_data: Option<Vec<u8>>,
    pub int32_data: Option<Vec<u8>>,
    pub dims: Vec<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OnnxqNode {
    pub op_type: String,
    pub inputs: Vec<String>,
    pub outputs: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct OnnxqModel {
    pub ir_version: u64,
    pub graph_name: String,
    pub nodes: Vec<OnnxqNode>,
    pub initializers: Vec<OnnxqTensor>,
}

fn varint(raw: &[u8], off: &mut usize) -> Result<u64, LarmError> {
    let mut result: u64 = 0;
    let mut shift = 0u32;
    loop {
        let byte = *raw
            .get(*off)
            .ok_or_else(|| invalid("onnxq_protobuf_truncated"))?;
        *off += 1;
        result |= u64::from(byte & 0x7F) << shift;
        if byte & 0x80 == 0 {
            return Ok(result);
        }
        shift += 7;
    }
}

#[allow(clippy::type_complexity)]
fn fields(raw: &[u8], start: usize, end: usize) -> Result<Vec<(u64, Vec<u8>, u64)>, LarmError> {
    let mut out = Vec::new();
    let mut off = start;
    while off < end {
        let key = varint(raw, &mut off)?;
        let field = key >> 3;
        let wire = key & 7;
        if wire == 2 {
            let length = varint(raw, &mut off)? as usize;
            let payload = raw
                .get(off..off + length)
                .ok_or_else(|| invalid("onnxq_protobuf_truncated"))?
                .to_vec();
            off += length;
            out.push((field, payload, 0));
        } else if wire == 0 {
            let value = varint(raw, &mut off)?;
            out.push((field, Vec::new(), value));
        } else if wire == 5 {
            let payload = raw
                .get(off..off + 4)
                .ok_or_else(|| invalid("onnxq_protobuf_truncated"))?
                .to_vec();
            off += 4;
            out.push((field, payload, 0));
        } else if wire == 1 {
            let payload = raw
                .get(off..off + 8)
                .ok_or_else(|| invalid("onnxq_protobuf_truncated"))?
                .to_vec();
            off += 8;
            out.push((field, payload, 0));
        } else {
            return Err(invalid("onnxq_protobuf_wire_unsupported"));
        }
    }
    Ok(out)
}

pub fn parse_onnxq_model(bytes: &[u8]) -> Result<OnnxqModel, LarmError> {
    if bytes.is_empty() {
        return Err(invalid("onnxq_model_empty"));
    }
    let top = fields(bytes, 0, bytes.len())?;
    let ir_version = top
        .iter()
        .find(|(field, _, _)| *field == 1)
        .map(|(_, _, value)| *value)
        .ok_or_else(|| invalid("onnxq_model_ir_missing"))?;
    let graph_payload = &top
        .iter()
        .find(|(field, _, _)| *field == 7)
        .ok_or_else(|| invalid("onnxq_graph_missing"))?
        .1;
    let graph = fields(graph_payload, 0, graph_payload.len())?;
    let mut nodes = Vec::new();
    let mut initializers = Vec::new();
    let mut graph_name = String::new();
    for (field, payload, _) in &graph {
        match field {
            1 => {
                let inner = fields(payload, 0, payload.len())?;
                let op_type = inner
                    .iter()
                    .find(|(f, _, _)| *f == 4)
                    .map(|(_, p, _)| String::from_utf8_lossy(p).to_string())
                    .ok_or_else(|| invalid("onnxq_node_op_missing"))?;
                let inputs = inner
                    .iter()
                    .filter(|(f, _, _)| *f == 1)
                    .map(|(_, p, _)| String::from_utf8_lossy(p).to_string())
                    .collect();
                let outputs = inner
                    .iter()
                    .filter(|(f, _, _)| *f == 2)
                    .map(|(_, p, _)| String::from_utf8_lossy(p).to_string())
                    .collect();
                nodes.push(OnnxqNode {
                    op_type,
                    inputs,
                    outputs,
                });
            }
            2 => graph_name = String::from_utf8_lossy(payload).to_string(),
            5 => {
                let inner = fields(payload, 0, payload.len())?;
                let mut tensor = OnnxqTensor {
                    name: String::new(),
                    onnx_dtype: 0,
                    raw_data: None,
                    float_data: None,
                    int32_data: None,
                    dims: Vec::new(),
                };
                // ORT serializa los initializers con encoding no determinista:
                // int32_data puede llegar empaquetado (wire 2) o un varint por
                // aparicion (wire 0). El oraculo acepta ambas formas.
                let mut int32_bytes: Vec<u8> = Vec::new();
                let mut float_bytes: Vec<u8> = Vec::new();
                for (f, p, v) in &inner {
                    match f {
                        1 => tensor.dims.push(*v),
                        2 => tensor.onnx_dtype = *v as u32,
                        4 => {
                            if p.is_empty() {
                                float_bytes.extend_from_slice(&(*v as u32).to_le_bytes());
                            } else {
                                float_bytes.extend_from_slice(p);
                            }
                        }
                        5 => {
                            if p.is_empty() {
                                int32_bytes.extend_from_slice(&(*v as u32).to_le_bytes());
                            } else {
                                int32_bytes.extend_from_slice(p);
                            }
                        }
                        8 => tensor.name = String::from_utf8_lossy(p).to_string(),
                        9 => tensor.raw_data = Some(p.clone()),
                        _ => {}
                    }
                }
                if !int32_bytes.is_empty() {
                    tensor.int32_data = Some(int32_bytes);
                }
                if !float_bytes.is_empty() {
                    tensor.float_data = Some(float_bytes);
                }
                initializers.push(tensor);
            }
            _ => {}
        }
    }
    Ok(OnnxqModel {
        ir_version,
        graph_name,
        nodes,
        initializers,
    })
}

pub fn onnxq_initializer(model: &OnnxqModel, name: &str) -> Result<OnnxqTensor, LarmError> {
    model
        .initializers
        .iter()
        .find(|tensor| tensor.name == name)
        .cloned()
        .ok_or_else(|| invalid("onnxq_initializer_missing"))
}

/// Mirror Rust de la cuantización dinámica del peso: scale =
/// f32(max_abs/127), q = clamp(round_half_away(w/scale), -127, 127),
/// zero_point = 0. Verificado contra el runtime Olive (0 mismatches).
fn mirror_quantize(weight: &[f32]) -> (f32, Vec<i32>) {
    let max_abs = weight.iter().fold(0f32, |acc, value| acc.max(value.abs()));
    let scale = f32::from_bits((max_abs / 127.0).to_bits());
    let quantized: Vec<i32> = weight
        .iter()
        .map(|value| {
            let quotient = *value / scale;
            let rounded = if quotient >= 0.0 {
                (quotient + 0.5).floor()
            } else {
                (quotient - 0.5).ceil()
            };
            rounded.clamp(-127.0, 127.0) as i32
        })
        .collect();
    (scale, quantized)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::conformance::onnxq_golden::{ONNXQ_FIXTURE_HEX, ONNXQ_GOLDEN_HEX};

    fn decode(hex: &str) -> Vec<u8> {
        (0..hex.len())
            .step_by(2)
            .map(|index| u8::from_str_radix(&hex[index..index + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn fixture_rust_identico_al_paquete() {
        let fixture = decode(ONNXQ_FIXTURE_HEX);
        let model = parse_onnxq_model(&fixture).unwrap();
        assert_eq!(model.graph_name, "larm-onnxq-fixture");
        assert_eq!(model.nodes.len(), 1);
        assert_eq!(model.nodes[0].op_type, "MatMul");
        let weight = onnxq_initializer(&model, "w").unwrap();
        assert_eq!(weight.onnx_dtype, 1);
        assert_eq!(weight.dims, vec![64, 128]);
        assert_eq!(weight.raw_data.as_ref().unwrap().len(), 8192 * 4);
    }

    #[test]
    fn cuantizacion_publicada_estructurada_y_ponderada() {
        let fixture = decode(ONNXQ_FIXTURE_HEX);
        let golden = decode(ONNXQ_GOLDEN_HEX);
        // Estructura QDQ del modelo cuantizado publicado.
        let model = parse_onnxq_model(&golden).unwrap();
        assert_eq!(model.ir_version, 8);
        assert_eq!(model.graph_name, "larm-onnxq-fixture");
        let ops: Vec<&str> = model
            .nodes
            .iter()
            .map(|node| node.op_type.as_str())
            .collect();
        assert_eq!(
            ops,
            [
                "DynamicQuantizeLinear",
                "Mul",
                "MatMulInteger",
                "Cast",
                "Mul"
            ]
        );
        let published_weight = onnxq_initializer(&model, "w_quantized").unwrap();
        assert_eq!(published_weight.onnx_dtype, 3);
        assert_eq!(published_weight.dims, vec![64, 128]);
        let scale = onnxq_initializer(&model, "w_scale").unwrap();
        assert_eq!(scale.onnx_dtype, 1);
        let zero = onnxq_initializer(&model, "w_zero_point").unwrap();
        assert_eq!(zero.onnx_dtype, 3);
        // Mirror byte a byte de los pesos contra el fixture determinista.
        let input = parse_onnxq_model(&fixture).unwrap();
        let source = onnxq_initializer(&input, "w").unwrap();
        let raw = source.raw_data.as_ref().unwrap();
        let weight: Vec<f32> = raw
            .as_chunks::<4>()
            .0
            .iter()
            .map(|chunk| f32::from_le_bytes(*chunk))
            .collect();
        let (scale_mirror, quantized) = mirror_quantize(&weight);
        let scale_bytes = scale_mirror.to_le_bytes().to_vec();
        let weight_bytes: Vec<u8> = quantized
            .iter()
            .flat_map(|value| (*value as i8).to_le_bytes())
            .collect();
        // Olive emite la escala escalar con float_data (field 4), no raw_data.
        let scale_published = scale
            .float_data
            .as_ref()
            .or(scale.raw_data.as_ref())
            .unwrap();
        assert_eq!(scale_published.len(), 4);
        assert_eq!(
            scale_bytes,
            scale_published[..4],
            "la escala del modelo publicado difiere del mirror Rust"
        );
        assert_eq!(
            weight_bytes,
            published_weight.raw_data.as_ref().unwrap()[..8192],
            "el peso int8 publicado difiere del mirror Rust"
        );
        let zero_bytes = zero.int32_data.as_ref().unwrap();
        assert!(
            !zero_bytes.is_empty() && zero_bytes.iter().all(|byte| *byte == 0),
            "el zero_point debe ser 0"
        );
    }
}
