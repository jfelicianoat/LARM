//! Conversor de producto GGUF (safetensors→GGUF v3 real). Ledger e intentos
//! privados; no acredita cuantización de modelos ni admisión global de Core.
//! Espejo de la suite de referencia con oráculos GGUF en Rust: el contenedor
//! publicado se contrasta byte a byte con la emisión esperada del oráculo y
//! sus tensores con los bytes del safetensors fuente.
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

const CAP: &str = "vendor.product.gguf";
const SCOPE:&str="product streaming safetensors/GGUF v3 conversion and private ledger; real container emit; no model quantization or Core admission";
#[derive(Clone, Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GgufConfig {
    pub profile: Value,
    pub expected_input: PathBuf,
    pub forbidden: PathBuf,
    pub evidence_root: PathBuf,
}
pub struct GgufTransformationSuite {
    config: GgufConfig,
    bundle: Value,
    expected: Vec<u8>,
    expected_quant: Vec<u8>,
    expected_quant4: Vec<u8>,
    expected_quant5: Vec<u8>,
    expected_quantk: Vec<u8>,
    expected_quant5k: Vec<u8>,
    expected_quant6k: Vec<u8>,
}
#[derive(Serialize)]
pub struct GgufReport {
    pub run: Run,
    pub run_sha256: String,
    pub scope: &'static str,
    pub wire: Option<WireReport>,
    pub cases: Vec<Value>,
    pub methods: BTreeSet<String>,
    pub checkpoints: BTreeMap<String, Value>,
    pub observations: Result<Vec<Observation>, LarmError>,
}
impl GgufReport {
    fn check(&mut self, id: &str, expected: Value, observed: Value) -> Result<(), LarmError> {
        let passed = expected == observed;
        self.cases
            .push(json!({"id":id,"expected":expected,"observed":observed,"passed":passed}));
        if passed {
            Ok(())
        } else {
            Err(error(
                ErrorCode::ValidationFailed,
                &format!("conformance_gguf_{id}"),
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
    use crate::conformance::gguf_golden::{FIXTURE_HEX, GOLDEN_GGUF_HEX};
    let schema = |maximum: usize| {
        json!({"type":"object","additionalProperties":false,"required":["sha256","size_bytes"],"properties":{
        "sha256":{"type":"string","pattern":"^[0-9a-f]{64}$"},"size_bytes":{"type":"string","pattern":"^(0|[1-9][0-9]*)$","maxLength":19,"x-maximum-decimal":maximum.to_string()}}})
    };
    let input_schema = schema(64 << 20);
    let output_schema = schema(80 << 20);
    let fixture_input =
        hex::decode(FIXTURE_HEX).map_err(|_| invalid("gguf_golden_fixture_invalid"))?;
    let converted =
        hex::decode(GOLDEN_GGUF_HEX).map_err(|_| invalid("gguf_golden_output_invalid"))?;
    let fixture = json!({"input":descriptor(&fixture_input),"output":descriptor(&converted)});
    Ok(
        json!({"schema_version":"1.0","entries":[{"id":CAP,"version":"1.0","input_schema":input_schema,"output_schema":output_schema,
        "input_sha256":hash(&input_schema)?,"output_sha256":hash(&output_schema)?,"mapping":"product.safetensors.gguf.v1","estimator_id":"product.gguf.buffers.v1","fixture":fixture,"fixture_sha256":hash(&fixture)?}]}),
    )
}

/// Oráculo GGUF en Rust: parseo safetensors, emisión esperada byte a byte
/// (misma ruta que el escritor Python del paquete) y parseo estructural del
/// contenedor publicado. Los dtypes son códigos ggml reales.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SafetensorsTensor {
    pub name: String,
    pub dtype: String,
    pub shape: Vec<u64>,
    pub source_offset: usize,
    pub length: usize,
    pub ggml_type: u32,
}
#[derive(Debug, Clone)]
pub struct SafetensorsDocument {
    pub data_start: usize,
    pub tensors: Vec<SafetensorsTensor>,
}
#[derive(Debug, Clone, PartialEq)]
pub enum MetadataValue {
    Str(String),
    U32(u32),
}
const GGML_F32: u32 = 0;
const GGML_F16: u32 = 1;
const GGML_Q8_0: u32 = 8;
const GGML_I8: u32 = 16;
const GGML_I16: u32 = 17;
const GGML_I32: u32 = 18;
const GGML_I64: u32 = 26;
const GGML_F64: u32 = 30;
const GGML_BF16: u32 = 31;
const QK8_0: usize = 32;
const Q8_0_BLOCK_BYTES: usize = 2 + QK8_0;
const GGML_Q4_0: u32 = 2;
const QK4_0: usize = 32;
const Q4_0_BLOCK_BYTES: usize = 2 + 16;
const GGML_Q5_0: u32 = 6;
const QK5_0: usize = 32;
const Q5_0_BLOCK_BYTES: usize = 2 + 4 + 16;
const GGML_Q4_K: u32 = 12;
const QK_K: usize = 256;
const K_SCALE_SIZE: usize = 12;
const Q4_K_BLOCK_BYTES: usize = 2 + 2 + K_SCALE_SIZE + QK_K / 2;
const GGML_Q5_K: u32 = 13;
const Q5_K_BLOCK_BYTES: usize = 2 + 2 + K_SCALE_SIZE + QK_K / 8 + QK_K / 2;
const GGML_Q6_K: u32 = 14;
const Q6_K_BLOCK_BYTES: usize = 2 + QK_K / 16 + QK_K / 2 + QK_K / 4;

/// f16 → f32 exacto (widening sin pérdida). Cubre normal, subnormal,
/// infinito y NaN según la especificación IEEE 754 half.
fn f16_to_f32(bits: u16) -> f32 {
    let sign = ((bits & 0x8000) as u32) << 16;
    let exp = ((bits >> 10) & 0x1F) as u32;
    let man = (bits & 0x3FF) as u32;
    let out = if exp == 0 {
        if man == 0 {
            sign
        } else {
            let mut mantissa = man;
            let mut unbiased = 113u32; // 127 - 14
            loop {
                mantissa <<= 1;
                unbiased -= 1;
                if mantissa & 0x400 != 0 {
                    break;
                }
            }
            sign | (unbiased << 23) | ((mantissa & 0x3FF) << 13)
        }
    } else if exp == 0x1F {
        sign | (0xFF << 23) | (man << 13)
    } else {
        sign | ((exp + 127 - 15) << 23) | (man << 13)
    };
    f32::from_bits(out)
}

/// f32 → bits f16 con redondeo round-to-nearest-even (misma semántica que
/// `struct '<e'` de Python). El desbordamiento produce infinito; el oráculo
/// sólo opera sobre fixtures con escala representable.
fn f32_to_f16_bits(value: f32) -> u16 {
    let bits = value.to_bits();
    let sign = ((bits >> 16) & 0x8000) as u16;
    let exp = ((bits >> 23) & 0xFF) as i32 - 127 + 15;
    let man = bits & 0x7F_FFFF;
    if ((bits >> 23) & 0xFF) == 0xFF {
        // Infinito/NaN f32: el conversor lo rechaza antes (escala no
        // representable); aquí se preserva el patrón igual que '<e'.
        return sign | 0x7C00 | if man != 0 { 0x0200 } else { 0 };
    }
    if exp >= 0x1F {
        return sign | 0x7C00;
    }
    if exp <= 0 {
        if exp < -10 {
            return sign;
        }
        let shift = (14 - exp) as u32;
        let full = man | 0x800_000;
        let round_bit = 1u32 << (shift - 1);
        let mut half = full >> shift;
        let rest = full & ((round_bit << 1) - 1);
        if rest > round_bit || (rest == round_bit && half & 1 == 1) {
            half += 1;
        }
        return sign | half as u16;
    }
    let round_bit = 1u32 << 12;
    let rest = man & ((round_bit << 1) - 1);
    let mut half = (man >> 13) as u16;
    if rest > round_bit || (rest == round_bit && half & 1 == 1) {
        half += 1;
    }
    let mut exp16 = exp as u16;
    if half == 0x400 {
        half = 0;
        exp16 += 1;
    }
    sign | (exp16 << 10) | half
}

fn f16_bytes_to_f32(raw: &[u8]) -> f32 {
    f16_to_f32(u16::from_le_bytes([raw[0], raw[1]]))
}
fn ggml_type(dtype: &str) -> Option<u32> {
    Some(match dtype {
        "F64" => GGML_F64,
        "F32" => GGML_F32,
        "F16" => GGML_F16,
        "BF16" => GGML_BF16,
        "I64" => GGML_I64,
        "I32" => GGML_I32,
        "I16" => GGML_I16,
        "I8" => GGML_I8,
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
        return Err(invalid("gguf_safetensors_prefix_invalid"));
    }
    let header_len = u64::from_le_bytes(bytes[..8].try_into().unwrap()) as usize;
    if header_len == 0 || header_len > 16 << 20 || 8 + header_len > bytes.len() {
        return Err(invalid("gguf_safetensors_header_invalid"));
    }
    let header: Value = serde_json::from_slice(&bytes[8..8 + header_len])
        .map_err(|_| invalid("gguf_safetensors_header_invalid"))?;
    let data_start = 8 + header_len;
    let data_size = bytes.len() - data_start;
    let mut tensors = Vec::new();
    for (name, entry) in header
        .as_object()
        .ok_or_else(|| invalid("gguf_safetensors_header_invalid"))?
    {
        if name == "__metadata__" {
            continue;
        }
        let dtype = entry["dtype"]
            .as_str()
            .ok_or_else(|| invalid("gguf_safetensors_dtype_unsupported"))?;
        let ggml = ggml_type(dtype).ok_or_else(|| invalid("gguf_safetensors_dtype_unsupported"))?;
        let element =
            element_bytes(dtype).ok_or_else(|| invalid("gguf_safetensors_dtype_unsupported"))?;
        let shape: Vec<u64> = entry["shape"]
            .as_array()
            .ok_or_else(|| invalid("gguf_safetensors_shape_invalid"))?
            .iter()
            .map(|v| {
                v.as_u64()
                    .ok_or_else(|| invalid("gguf_safetensors_shape_invalid"))
            })
            .collect::<Result<_, _>>()?;
        if shape.is_empty() || shape.len() > 4 {
            return Err(invalid("gguf_safetensors_shape_invalid"));
        }
        let offsets = entry["data_offsets"]
            .as_array()
            .ok_or_else(|| invalid("gguf_safetensors_offsets_invalid"))?;
        if offsets.len() != 2 {
            return Err(invalid("gguf_safetensors_offsets_invalid"));
        }
        let start = offsets[0]
            .as_u64()
            .ok_or_else(|| invalid("gguf_safetensors_offsets_invalid"))?;
        let end = offsets[1]
            .as_u64()
            .ok_or_else(|| invalid("gguf_safetensors_offsets_invalid"))?;
        let elements: u64 = shape.iter().product();
        if end < start || end > data_size as u64 || end - start != elements * element {
            return Err(invalid("gguf_safetensors_offsets_invalid"));
        }
        tensors.push(SafetensorsTensor {
            name: name.clone(),
            dtype: dtype.to_string(),
            shape,
            source_offset: start as usize,
            length: (end - start) as usize,
            ggml_type: ggml,
        });
    }
    if tensors.is_empty() {
        return Err(invalid("gguf_safetensors_tensor_missing"));
    }
    // El orden del contenedor es el de los datos (offsets monótonos en un
    // safetensors bien formado); serde_json ordena las claves, así que el
    // orden de emisión se fija por offset y no por nombre.
    tensors.sort_by_key(|tensor| tensor.source_offset);
    Ok(SafetensorsDocument {
        data_start,
        tensors,
    })
}
#[derive(Debug, Clone, PartialEq)]
pub struct GgufTensor {
    pub name: String,
    pub dims: Vec<u64>,
    pub ggml_type: u32,
    pub offset: u64,
}
#[derive(Debug, Clone)]
pub struct GgufContainer {
    pub version: u32,
    pub alignment: u32,
    pub metadata: BTreeMap<String, (Value, u32)>,
    pub tensors: Vec<GgufTensor>,
    pub data_begin: usize,
}
#[allow(clippy::unwrap_used)] // las rebanadas tienen longitud verificada antes de cada try_into
pub fn parse_gguf(bytes: &[u8]) -> Result<GgufContainer, LarmError> {
    if bytes.len() < 24 || &bytes[..4] != b"GGUF" {
        return Err(invalid("gguf_magic_invalid"));
    }
    let version = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
    if version != 3 {
        return Err(invalid("gguf_version_invalid"));
    }
    let tensor_count = u64::from_le_bytes(bytes[8..16].try_into().unwrap());
    let kv_count = u64::from_le_bytes(bytes[16..24].try_into().unwrap());
    let mut offset = 24usize;
    let mut metadata = BTreeMap::new();
    for _ in 0..kv_count {
        let len = u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap()) as usize;
        offset += 8;
        let key = String::from_utf8(bytes[offset..offset + len].to_vec())
            .map_err(|_| invalid("gguf_string_invalid"))?;
        offset += len;
        let kind = u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap());
        offset += 4;
        let value = match kind {
            4 => {
                let v = u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap());
                offset += 4;
                Value::from(v)
            }
            11 => {
                let v = i64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap());
                offset += 8;
                Value::from(v)
            }
            8 => {
                let len =
                    u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap()) as usize;
                offset += 8;
                let text = String::from_utf8(bytes[offset..offset + len].to_vec())
                    .map_err(|_| invalid("gguf_string_invalid"))?;
                offset += len;
                Value::from(text)
            }
            _ => return Err(invalid("gguf_metadata_type_unsupported")),
        };
        metadata.insert(key, (value, kind));
    }
    let mut tensors = Vec::new();
    for _ in 0..tensor_count {
        let len = u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap()) as usize;
        offset += 8;
        let name = String::from_utf8(bytes[offset..offset + len].to_vec())
            .map_err(|_| invalid("gguf_string_invalid"))?;
        offset += len;
        let n_dims = u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap());
        offset += 4;
        let mut dims = Vec::new();
        for _ in 0..n_dims {
            dims.push(u64::from_le_bytes(
                bytes[offset..offset + 8].try_into().unwrap(),
            ));
            offset += 8;
        }
        let ggml_type = u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap());
        offset += 4;
        let gguf_offset = u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap());
        offset += 8;
        tensors.push(GgufTensor {
            name,
            dims,
            ggml_type,
            offset: gguf_offset,
        });
    }
    let alignment = match metadata.get("general.alignment") {
        Some((Value::Number(v), 4)) => {
            v.as_u64()
                .ok_or_else(|| invalid("gguf_alignment_invalid"))? as u32
        }
        _ => 32,
    };
    // `offset` ya es absoluto (empezó en 24 tras los contadores): alinear sin
    // volver a sumar el prefijo.
    let data_begin = offset.div_ceil(alignment as usize) * alignment as usize;
    Ok(GgufContainer {
        version,
        alignment,
        metadata,
        tensors,
        data_begin,
    })
}
fn metadata_entries() -> Vec<(&'static str, MetadataValue)> {
    metadata_entries_quantized(false)
}
fn metadata_entries_quantized(quantized: bool) -> Vec<(&'static str, MetadataValue)> {
    // general.file_type (u32) según la tabla ftype de GGUF: 0 ALL_F32,
    // 7 MOSTLY_Q8_0 cuando el contenedor lleva tensores cuantizados.
    vec![
        (
            "general.architecture",
            MetadataValue::Str("product.test".into()),
        ),
        (
            "general.name",
            MetadataValue::Str("larm-gguf-product".into()),
        ),
        (
            "general.file_type",
            MetadataValue::U32(if quantized { 7 } else { 0 }),
        ),
        ("general.alignment", MetadataValue::U32(32)),
        (
            "general.source.format",
            MetadataValue::Str("safetensors".into()),
        ),
    ]
}
fn write_string(out: &mut Vec<u8>, value: &str) {
    out.extend_from_slice(&(value.len() as u64).to_le_bytes());
    out.extend_from_slice(value.as_bytes());
}

/// Regla de cuantización por tensor, espejo del paquete: F32/F16 con eje
/// rápido (última dim del shape) múltiplo de QK8_0 se cuantiza a Q8_0; todo
/// lo demás va verbatim. Determinista y total.
fn quantized_tensor(tensor: &SafetensorsTensor, quant: Option<&str>) -> bool {
    if !(quant == Some("Q8_0")
        || quant == Some("Q4_0")
        || quant == Some("Q5_0")
        || quant == Some("Q4_K")
        || quant == Some("Q5_K")
        || quant == Some("Q6_K"))
    {
        return false;
    }
    if tensor.dtype != "F32" && tensor.dtype != "F16" {
        return false;
    }
    if quant == Some("Q4_K") || quant == Some("Q5_K") || quant == Some("Q6_K") {
        // El super-bloque cubre 256 elementos consecutivos: el total debe
        // ser múltiplo de QK_K.
        let width = if tensor.dtype == "F32" { 4 } else { 2 };
        let elements = tensor.length / width;
        return elements > 0 && elements.is_multiple_of(QK_K);
    }
    !tensor.shape.is_empty()
        && tensor.shape[tensor.shape.len() - 1] > 0
        && tensor.shape[tensor.shape.len() - 1].is_multiple_of(QK8_0 as u64)
}

/// Un bloque Q8_0 desde 32 elementos F32/F16 en `source_bytes[offset..]`.
/// d = f16(amax/127 en f32, RNE); id = 1/d en f32 (0 si d==0);
/// q[i] = clamp(roundf(v_f32 * id en f32), -127, 127). La aritmética replica
/// el paquete Python: cada producto/cociente se computa en f64 (exacto para
/// operandos f32) y se redondea UNA vez a f32; roundf es half-away-from-zero.
#[allow(clippy::unwrap_used)] // las rebanadas tienen longitud verificada antes de cada try_into
fn quantize_q8_0_block(source_bytes: &[u8], offset: usize, dtype: &str) -> Vec<u8> {
    let read = |index: usize| -> f32 {
        if dtype == "F32" {
            f32::from_le_bytes(
                source_bytes[offset + index * 4..offset + index * 4 + 4]
                    .try_into()
                    .unwrap(),
            )
        } else {
            f16_bytes_to_f32(&source_bytes[offset + index * 2..offset + index * 2 + 2])
        }
    };
    let values: Vec<f32> = (0..QK8_0).map(read).collect();
    let mut amax = 0f64;
    for value in &values {
        let magnitude = f64::from(value.abs());
        if magnitude > amax {
            amax = magnitude;
        }
    }
    let d = (amax / 127.0) as f32;
    let idiv = if d == 0.0 {
        0f32
    } else {
        (1.0 / f64::from(d)) as f32
    };
    let mut block = Vec::with_capacity(Q8_0_BLOCK_BYTES);
    block.extend_from_slice(&f32_to_f16_bits(d).to_le_bytes());
    for value in &values {
        let product = (f64::from(*value) * f64::from(idiv)) as f32;
        // f32::round: half away from zero; el rango f32 no excede la clamp.
        let q = product.round().clamp(-127.0, 127.0);
        block.push(q as i8 as u8);
    }
    block
}

/// Un bloque Q4_0 (type 2) desde 32 elementos F32/F16: escala CON signo
/// d = f32(max/-8), truncado del cast C sobre f32(v*id)+8.5 (cada op f32 es
/// un solo redondeo; el producto/cociente se computa en f64 exacto), clamp 15
/// y nibbles par-bajo/impar-alto. Espejo del paquete Python.
#[allow(clippy::unwrap_used)] // las rebanadas tienen longitud verificada antes de cada try_into
fn quantize_q4_0_block(source_bytes: &[u8], offset: usize, dtype: &str) -> Vec<u8> {
    let read = |index: usize| -> f32 {
        if dtype == "F32" {
            f32::from_le_bytes(
                source_bytes[offset + index * 4..offset + index * 4 + 4]
                    .try_into()
                    .unwrap(),
            )
        } else {
            f16_bytes_to_f32(&source_bytes[offset + index * 2..offset + index * 2 + 2])
        }
    };
    let values: Vec<f32> = (0..QK4_0).map(read).collect();
    let mut max_value = 0f64;
    let mut max_magnitude = 0f64;
    for value in &values {
        let magnitude = f64::from(value.abs());
        if magnitude > max_magnitude {
            max_magnitude = magnitude;
            max_value = f64::from(*value);
        }
    }
    let d = (max_value / -8.0) as f32;
    let idiv = if d == 0.0 {
        0f32
    } else {
        (1.0 / f64::from(d)) as f32
    };
    let mut block = Vec::with_capacity(Q4_0_BLOCK_BYTES);
    block.extend_from_slice(&f32_to_f16_bits(d).to_le_bytes());
    let mut nibbles = [0u8; 16];
    let mut xis = [0usize; QK4_0];
    for (index, value) in values.iter().enumerate() {
        let scaled = (f64::from(*value) * f64::from(idiv)) as f32;
        let shifted = (f64::from(scaled) + 8.5) as f32;
        let mut xi = shifted as i32; // trunc hacia cero, como el cast C
        xi = xi.clamp(0, 15);
        xis[index] = xi as usize;
    }
    // Empaquetado de MITADES según la referencia real de ggml: el byte j une
    // los elementos j (bajo) y j+16 (alto) del bloque.
    for j in 0..16 {
        nibbles[j] = (xis[j] as u8) | ((xis[j + 16] as u8) << 4);
    }
    block.extend_from_slice(&nibbles);
    block
}

/// Un bloque Q5_0 (type 6) desde 32 elementos F32/F16: escala CON signo
/// d = f32(max/-16), trunc sobre f32(v*id)+16.5, byte j de qs une los
/// elementos j/j+16 en nibbles y el 5º bit del elemento k va al bit k del
/// uint32 qh. Espejo del paquete Python y de la referencia real de ggml.
#[allow(clippy::unwrap_used)] // las rebanadas tienen longitud verificada antes de cada try_into
fn quantize_q5_0_block(source_bytes: &[u8], offset: usize, dtype: &str) -> Vec<u8> {
    let read = |index: usize| -> f32 {
        if dtype == "F32" {
            f32::from_le_bytes(
                source_bytes[offset + index * 4..offset + index * 4 + 4]
                    .try_into()
                    .unwrap(),
            )
        } else {
            f16_bytes_to_f32(&source_bytes[offset + index * 2..offset + index * 2 + 2])
        }
    };
    let values: Vec<f32> = (0..QK5_0).map(read).collect();
    let mut max_value = 0f64;
    let mut max_magnitude = 0f64;
    for value in &values {
        let magnitude = f64::from(value.abs());
        if magnitude > max_magnitude {
            max_magnitude = magnitude;
            max_value = f64::from(*value);
        }
    }
    let d = (max_value / -16.0) as f32;
    let idiv = if d == 0.0 {
        0f32
    } else {
        (1.0 / f64::from(d)) as f32
    };
    let mut block = Vec::with_capacity(Q5_0_BLOCK_BYTES);
    block.extend_from_slice(&f32_to_f16_bits(d).to_le_bytes());
    let mut xis = [0usize; QK5_0];
    for (index, value) in values.iter().enumerate() {
        let scaled = (f64::from(*value) * f64::from(idiv)) as f32;
        let shifted = (f64::from(scaled) + 16.5) as f32;
        let mut xi = shifted as i32; // trunc hacia cero, como el cast C
        xi = xi.clamp(0, 31);
        xis[index] = xi as usize;
    }
    let mut qh = [0u8; 4];
    let mut nibbles = [0u8; 16];
    for j in 0..16 {
        nibbles[j] = ((xis[j] & 0x0F) as u8) | (((xis[j + 16] & 0x0F) as u8) << 4);
        qh[j / 8] |= (((xis[j] >> 4) & 1) as u8) << (j % 8);
        qh[(j + 16) / 8] |= (((xis[j + 16] >> 4) & 1) as u8) << ((j + 16) % 8);
    }
    block.extend_from_slice(&qh);
    block.extend_from_slice(&nibbles);
    block
}

/// `nearest_int` de ggml: punto mágico — f32(fval+12582912) reinterpretado
/// como int32; (i & 0x007fffff) − 0x00400000. Bit a bit idéntico al C.
fn nearest_int(fval: f32) -> i32 {
    let val = fval + 12582912.0f32;
    let bits = val.to_bits() as i32;
    (bits & 0x007F_FFFF) - 0x0040_0000
}

/// Espejo verbatim de make_qkx2_quants de ggml (use_mad=false): cada producto
/// y cada acumulación se redondea a f32 paso a paso, como el C.
fn make_qkx2_quants(
    values: &[f32],
    weights: &[f32],
    nmax: f32,
    rmin: f32,
    rdelta: f32,
    nstep: usize,
) -> (f32, f32, Vec<usize>) {
    let n = values.len();
    let mut minimum = values[0];
    let mut maximum = values[0];
    let mut sum_w = weights[0];
    let mut sum_x = sum_w * values[0];
    for i in 1..n {
        if values[i] < minimum {
            minimum = values[i];
        }
        if values[i] > maximum {
            maximum = values[i];
        }
        let w = weights[i];
        sum_w += w;
        sum_x += w * values[i];
    }
    if minimum > 0.0 {
        minimum = 0.0;
    }
    if maximum == minimum {
        return (0.0, -minimum, vec![0; n]);
    }
    let mut iscale = nmax / (maximum - minimum);
    let mut scale = 1.0f32 / iscale;
    let mut best_error = 0.0f32;
    let mut l_levels = Vec::with_capacity(n);
    for i in 0..n {
        let l = nearest_int(iscale * (values[i] - minimum)).clamp(0, nmax as i32) as usize;
        l_levels.push(l);
        let diff = scale * l as f32 + minimum - values[i];
        let w = weights[i];
        best_error += w * (diff * diff);
    }
    if nstep < 1 {
        return (scale, -minimum, l_levels);
    }
    let mut laux = vec![0usize; n];
    for is_step in 0..=nstep {
        iscale = (rmin + rdelta * is_step as f32 + nmax) / (maximum - minimum);
        let mut sum_l = 0.0f32;
        let mut sum_l2 = 0.0f32;
        let mut sum_xl = 0.0f32;
        for i in 0..n {
            let l = nearest_int(iscale * (values[i] - minimum)).clamp(0, nmax as i32) as usize;
            laux[i] = l;
            let w = weights[i];
            sum_l += w * l as f32;
            sum_l2 += w * (l as f32) * (l as f32);
            sum_xl += w * (l as f32) * values[i];
        }
        let d = sum_w * sum_l2 - sum_l * sum_l;
        if d > 0.0 {
            let mut this_scale = (sum_w * sum_xl - sum_x * sum_l) / d;
            let mut this_min = (sum_l2 * sum_x - sum_l * sum_xl) / d;
            if this_min > 0.0 {
                this_min = 0.0;
                this_scale = sum_xl / sum_l2;
            }
            let mut cur_error = 0.0f32;
            for i in 0..n {
                let diff = this_scale * laux[i] as f32 + this_min - values[i];
                let w = weights[i];
                cur_error += w * (diff * diff);
            }
            if cur_error < best_error {
                l_levels.copy_from_slice(&laux);
                best_error = cur_error;
                scale = this_scale;
                minimum = this_min;
            }
        }
    }
    (scale, -minimum, l_levels)
}

/// Un super-bloque Q4_K (type 12) desde 256 elementos F32/F16: 144 bytes con
/// 8 sub-bloques de escala y mínimo propios (make_qkx2_quants), escalas de 6
/// bits en scales[12] y re-cuantización final con la escala del super-bloque.
/// Espejo verbatim del C de la referencia ggml.
#[allow(clippy::unwrap_used)] // las rebanadas tienen longitud verificada antes de cada try_into
fn quantize_q4_k_block(source_bytes: &[u8], offset: usize, dtype: &str) -> Vec<u8> {
    let read = |index: usize| -> f32 {
        if dtype == "F32" {
            f32::from_le_bytes(
                source_bytes[offset + index * 4..offset + index * 4 + 4]
                    .try_into()
                    .unwrap(),
            )
        } else {
            f16_bytes_to_f32(&source_bytes[offset + index * 2..offset + index * 2 + 2])
        }
    };
    let values: Vec<f32> = (0..QK_K).map(read).collect();
    let mut mins = [0f32; 8];
    let mut sub_scales = [0f32; 8];
    let mut l_all = vec![0usize; QK_K];
    let mut max_scale = 0f32;
    let mut max_min = 0f32;
    for sub in 0..8 {
        let chunk = &values[32 * sub..32 * sub + 32];
        let mut sum_x2 = 0.0f32;
        for value in chunk {
            sum_x2 += value * value;
        }
        let av_x = (sum_x2 / 32.0).sqrt();
        let weights: Vec<f32> = chunk.iter().map(|v| av_x + v.abs()).collect();
        let (scale, the_min, levels) = make_qkx2_quants(chunk, &weights, 15.0, -1.0, 0.1, 20);
        sub_scales[sub] = scale;
        mins[sub] = the_min;
        l_all[32 * sub..32 * sub + 32].copy_from_slice(&levels);
        if scale > max_scale {
            max_scale = scale;
        }
        if the_min > max_min {
            max_min = the_min;
        }
    }
    let inv_scale = if max_scale > 0.0 {
        63.0f32 / max_scale
    } else {
        0.0
    };
    let inv_min = if max_min > 0.0 {
        63.0f32 / max_min
    } else {
        0.0
    };
    let mut ls_values = [0usize; 8];
    let mut lm_values = [0usize; 8];
    for j in 0..8 {
        ls_values[j] = nearest_int(inv_scale * sub_scales[j]).clamp(0, 63) as usize;
        lm_values[j] = nearest_int(inv_min * mins[j]).clamp(0, 63) as usize;
    }
    let mut scales = [0u8; K_SCALE_SIZE];
    for j in 0..8 {
        let (ls, lm) = (ls_values[j] as u8, lm_values[j] as u8);
        if j < 4 {
            scales[j] = ls;
            scales[j + 4] = lm;
        } else {
            scales[j + 4] = (ls & 0xF) | ((lm & 0xF) << 4);
            scales[j - 4] |= (ls >> 4) << 6;
            scales[j] |= (lm >> 4) << 6;
        }
    }
    let mut block = Vec::with_capacity(Q4_K_BLOCK_BYTES);
    block.extend_from_slice(&f32_to_f16_bits(max_scale / 63.0).to_le_bytes());
    block.extend_from_slice(&f32_to_f16_bits(max_min / 63.0).to_le_bytes());
    block.extend_from_slice(&scales);
    let d_block = f16_bytes_to_f32(&block[0..2]);
    let dmin_block = f16_bytes_to_f32(&block[2..4]);
    for j in 0..8 {
        let (sc, m) = if j < 4 {
            ((scales[j] & 63) as u32, (scales[j + 4] & 63) as u32)
        } else {
            (
                ((scales[j + 4] & 0xF) | ((scales[j - 4] >> 6) << 4)) as u32,
                ((scales[j + 4] >> 4) | ((scales[j] >> 6) << 4)) as u32,
            )
        };
        let sub_d = d_block * sc as f32;
        if sub_d == 0.0 {
            continue;
        }
        let sub_dm = dmin_block * m as f32;
        for ii in 0..32 {
            let index = 32 * j + ii;
            let l = nearest_int((values[index] + sub_dm) / sub_d).clamp(0, 15) as usize;
            l_all[index] = l;
        }
    }
    let mut qs = [0u8; QK_K / 2];
    let mut cursor = 0usize;
    for j in (0..QK_K).step_by(64) {
        for l in 0..32 {
            qs[cursor + l] = (l_all[j + l] as u8) | ((l_all[j + l + 32] as u8) << 4);
        }
        cursor += 32;
    }
    block.extend_from_slice(&qs);
    block
}

/// Un super-bloque Q5_K (type 13) desde 256 elementos F32/F16: 176 bytes
/// (d f16 + dmin f16 + scales[12] + qh[32] + qs[128]). Misma maquinaria de
/// Q4_K con niveles de 5 bits: make_qkx2_quants(32, 31, ..., -0.5, 0.1, 15),
/// re-cuantización final con clamp 0..31, nibbles por mitades y el 5º bit en
/// los bits 2·chunk/2·chunk+1 de qh[j]. Espejo verbatim de la referencia ggml.
#[allow(clippy::unwrap_used)] // las rebanadas tienen longitud verificada antes de cada try_into
fn quantize_q5_k_block(source_bytes: &[u8], offset: usize, dtype: &str) -> Vec<u8> {
    let read = |index: usize| -> f32 {
        if dtype == "F32" {
            f32::from_le_bytes(
                source_bytes[offset + index * 4..offset + index * 4 + 4]
                    .try_into()
                    .unwrap(),
            )
        } else {
            f16_bytes_to_f32(&source_bytes[offset + index * 2..offset + index * 2 + 2])
        }
    };
    let values: Vec<f32> = (0..QK_K).map(read).collect();
    let mut mins = [0f32; 8];
    let mut sub_scales = [0f32; 8];
    let mut l_all = vec![0usize; QK_K];
    let mut max_scale = 0f32;
    let mut max_min = 0f32;
    for sub in 0..8 {
        let chunk = &values[32 * sub..32 * sub + 32];
        let mut sum_x2 = 0.0f32;
        for value in chunk {
            sum_x2 += value * value;
        }
        let av_x = (sum_x2 / 32.0).sqrt();
        let weights: Vec<f32> = chunk.iter().map(|v| av_x + v.abs()).collect();
        let (scale, the_min, levels) = make_qkx2_quants(chunk, &weights, 31.0, -0.5, 0.1, 15);
        sub_scales[sub] = scale;
        mins[sub] = the_min;
        l_all[32 * sub..32 * sub + 32].copy_from_slice(&levels);
        if scale > max_scale {
            max_scale = scale;
        }
        if the_min > max_min {
            max_min = the_min;
        }
    }
    let inv_scale = if max_scale > 0.0 {
        63.0f32 / max_scale
    } else {
        0.0
    };
    let inv_min = if max_min > 0.0 {
        63.0f32 / max_min
    } else {
        0.0
    };
    let mut ls_values = [0usize; 8];
    let mut lm_values = [0usize; 8];
    for j in 0..8 {
        ls_values[j] = nearest_int(inv_scale * sub_scales[j]).clamp(0, 63) as usize;
        lm_values[j] = nearest_int(inv_min * mins[j]).clamp(0, 63) as usize;
    }
    let mut scales = [0u8; K_SCALE_SIZE];
    for j in 0..8 {
        let (ls, lm) = (ls_values[j] as u8, lm_values[j] as u8);
        if j < 4 {
            scales[j] = ls;
            scales[j + 4] = lm;
        } else {
            scales[j + 4] = (ls & 0xF) | ((lm & 0xF) << 4);
            scales[j - 4] |= (ls >> 4) << 6;
            scales[j] |= (lm >> 4) << 6;
        }
    }
    let mut block = Vec::with_capacity(Q5_K_BLOCK_BYTES);
    block.extend_from_slice(&f32_to_f16_bits(max_scale / 63.0).to_le_bytes());
    block.extend_from_slice(&f32_to_f16_bits(max_min / 63.0).to_le_bytes());
    block.extend_from_slice(&scales);
    let d_block = f16_bytes_to_f32(&block[0..2]);
    let dmin_block = f16_bytes_to_f32(&block[2..4]);
    for j in 0..8 {
        let (sc, m) = if j < 4 {
            ((scales[j] & 63) as u32, (scales[j + 4] & 63) as u32)
        } else {
            (
                ((scales[j + 4] & 0xF) | ((scales[j - 4] >> 6) << 4)) as u32,
                ((scales[j + 4] >> 4) | ((scales[j] >> 6) << 4)) as u32,
            )
        };
        let sub_d = d_block * sc as f32;
        if sub_d == 0.0 {
            continue;
        }
        let sub_dm = dmin_block * m as f32;
        for ii in 0..32 {
            let index = 32 * j + ii;
            let l = nearest_int((values[index] + sub_dm) / sub_d).clamp(0, 31) as usize;
            l_all[index] = l;
        }
    }
    let mut qh = [0u8; QK_K / 8];
    let mut qs = [0u8; QK_K / 2];
    let mut m1 = 1u8;
    let mut m2 = 2u8;
    let mut cursor = 0usize;
    for n in (0..QK_K).step_by(64) {
        for j in 0..32 {
            let mut l1 = l_all[n + j];
            if l1 > 15 {
                l1 -= 16;
                qh[j] |= m1;
            }
            let mut l2 = l_all[n + j + 32];
            if l2 > 15 {
                l2 -= 16;
                qh[j] |= m2;
            }
            qs[cursor + j] = (l1 as u8) | ((l2 as u8) << 4);
        }
        cursor += 32;
        m1 <<= 2;
        m2 <<= 2;
    }
    block.extend_from_slice(&qh);
    block.extend_from_slice(&qs);
    block
}

/// Espejo verbatim de make_qx_quants de ggml (rmse_type=1, qw=NULL):
/// cuantización simétrica con ajuste por mínimos cuadrados ponderados por x².
/// Cada producto y cada acumulación se redondea a f32 paso a paso, como el C.
fn make_qx_quants(values: &[f32], nmax: i32) -> (f32, Vec<i32>) {
    let n = values.len();
    let mut max_value = 0f32;
    let mut amax = 0f32;
    for value in values {
        let ax = value.abs();
        if ax > amax {
            amax = ax;
            max_value = *value;
        }
    }
    if amax < 1e-15f32 {
        return (0.0, vec![0; n]);
    }
    let mut iscale = -nmax as f32 / max_value;
    let mut levels: Vec<i32> = Vec::with_capacity(n);
    let mut sumlx = 0f32;
    let mut suml2 = 0f32;
    for value in values {
        let l = nearest_int(iscale * value).clamp(-nmax, nmax - 1);
        levels.push(l + nmax);
        let w = value * value;
        sumlx += w * value * l as f32;
        suml2 += w * (l as f32) * (l as f32);
    }
    let mut scale = if suml2 != 0.0 { sumlx / suml2 } else { 0.0 };
    let mut best = scale * sumlx;
    for is_step in -9..=9 {
        if is_step == 0 {
            continue;
        }
        iscale = -(nmax as f32 + 0.1f32 * is_step as f32) / max_value;
        sumlx = 0.0;
        suml2 = 0.0;
        for value in values {
            let l = nearest_int(iscale * value).clamp(-nmax, nmax - 1);
            let w = value * value;
            sumlx += w * value * l as f32;
            suml2 += w * (l as f32) * (l as f32);
        }
        if suml2 > 0.0 && sumlx * sumlx > best * suml2 {
            levels.clear();
            for value in values {
                let l = nearest_int(iscale * value).clamp(-nmax, nmax - 1);
                levels.push(l + nmax);
            }
            scale = sumlx / suml2;
            best = scale * sumlx;
        }
    }
    (scale, levels)
}

/// Un super-bloque Q6_K (type 14) desde 256 elementos F32/F16: 210 bytes
/// (d f16 + scales int8[16] + ql[128] + qh[64]). 16 sub-bloques de 16
/// elementos con cuantización SIMÉTRICA (make_qx_quants, niveles [-32,31]
/// desplazados a [0,63]), escalas de 8 bits con signo y empaquetado de 6
/// bits. Espejo verbatim de la referencia ggml.
#[allow(clippy::unwrap_used)] // las rebanadas tienen longitud verificada antes de cada try_into
fn quantize_q6_k_block(source_bytes: &[u8], offset: usize, dtype: &str) -> Vec<u8> {
    let read = |index: usize| -> f32 {
        if dtype == "F32" {
            f32::from_le_bytes(
                source_bytes[offset + index * 4..offset + index * 4 + 4]
                    .try_into()
                    .unwrap(),
            )
        } else {
            f16_bytes_to_f32(&source_bytes[offset + index * 2..offset + index * 2 + 2])
        }
    };
    let values: Vec<f32> = (0..QK_K).map(read).collect();
    let mut sub_scales = [0f32; 16];
    let mut l_all = vec![0i32; QK_K];
    let mut max_scale = 0f32;
    for sub in 0..16 {
        let chunk = &values[16 * sub..16 * sub + 16];
        let (scale, levels) = make_qx_quants(chunk, 32);
        sub_scales[sub] = scale;
        l_all[16 * sub..16 * sub + 16].copy_from_slice(&levels);
        if scale > max_scale {
            max_scale = scale;
        }
    }
    let mut block = Vec::with_capacity(Q6_K_BLOCK_BYTES);
    if max_scale < 1e-15f32 {
        block.extend_from_slice(&0u16.to_le_bytes());
        block.extend_from_slice(&[0u8; 16]);
        block.extend_from_slice(&[0u8; QK_K / 2]);
        block.extend_from_slice(&[0u8; QK_K / 4]);
        return block;
    }
    let iscale = -128.0f32 / max_scale;
    block.extend_from_slice(&f32_to_f16_bits(1.0f32 / iscale).to_le_bytes());
    let mut scales = [0i32; 16];
    for (j, sub_scale) in sub_scales.iter().enumerate() {
        scales[j] = nearest_int(iscale * sub_scale).min(127);
    }
    for scale in &scales {
        block.push(*scale as u8);
    }
    let d_block = f16_bytes_to_f32(&block[0..2]);
    for (j, scale) in scales.iter().enumerate() {
        let d_sub = d_block * *scale as f32;
        if d_sub == 0.0 {
            continue;
        }
        for ii in 0..16 {
            let index = 16 * j + ii;
            let l = nearest_int(values[index] / d_sub).clamp(-32, 31);
            l_all[index] = l + 32;
        }
    }
    let mut ql = [0u8; QK_K / 2];
    let mut qh = [0u8; QK_K / 4];
    for j in (0..QK_K).step_by(128) {
        let base = j / 128;
        for l in 0..32 {
            let q1 = (l_all[j + l] & 0xF) as u8;
            let q2 = (l_all[j + l + 32] & 0xF) as u8;
            let q3 = (l_all[j + l + 64] & 0xF) as u8;
            let q4 = (l_all[j + l + 96] & 0xF) as u8;
            ql[base * 64 + l] = q1 | (q3 << 4);
            ql[base * 64 + l + 32] = q2 | (q4 << 4);
            qh[base * 32 + l] = (((l_all[j + l] >> 4) & 3) as u8)
                | ((((l_all[j + l + 32] >> 4) & 3) as u8) << 2)
                | ((((l_all[j + l + 64] >> 4) & 3) as u8) << 4)
                | ((((l_all[j + l + 96] >> 4) & 3) as u8) << 6);
        }
    }
    block.extend_from_slice(&ql);
    block.extend_from_slice(&qh);
    block
}

/// Emisión espejo del contenedor con cuantización opcional (Q8_0/Q4_0/Q5_0).
/// El file_type pasa a 7 (MOSTLY_Q8_0) cuando quedó al menos un tensor
/// cuantizado; los lengths y el layout siguen la misma disciplina del
/// escritor Python (offsets en una sola pasada, alineación a 32).
pub fn expected_gguf_quantized(
    document: &SafetensorsDocument,
    source: &[u8],
    quant: Option<&str>,
) -> Vec<u8> {
    let quantized: Vec<bool> = document
        .tensors
        .iter()
        .map(|tensor| quantized_tensor(tensor, quant))
        .collect();
    let any_quantized = quantized.iter().any(|flag| *flag);
    let (block_bytes, emitted_type) = match quant {
        Some("Q4_0") => (Q4_0_BLOCK_BYTES, GGML_Q4_0),
        Some("Q5_0") => (Q5_0_BLOCK_BYTES, GGML_Q5_0),
        Some("Q4_K") => (Q4_K_BLOCK_BYTES, GGML_Q4_K),
        Some("Q5_K") => (Q5_K_BLOCK_BYTES, GGML_Q5_K),
        Some("Q6_K") => (Q6_K_BLOCK_BYTES, GGML_Q6_K),
        _ => (Q8_0_BLOCK_BYTES, GGML_Q8_0),
    };
    let lengths: Vec<usize> = document
        .tensors
        .iter()
        .zip(&quantized)
        .map(|(tensor, flag)| {
            if *flag {
                let width = if tensor.dtype == "F32" { 4 } else { 2 };
                let elements = tensor.length / width;
                if quant == Some("Q4_K") || quant == Some("Q5_K") || quant == Some("Q6_K") {
                    (elements / QK_K) * block_bytes
                } else {
                    (elements / QK8_0) * block_bytes
                }
            } else {
                tensor.length
            }
        })
        .collect();
    let mut infos = Vec::new();
    let metadata = metadata_entries_quantized(any_quantized);
    for (key, value) in &metadata {
        write_string(&mut infos, key);
        match value {
            MetadataValue::Str(text) => {
                infos.extend_from_slice(&8u32.to_le_bytes());
                write_string(&mut infos, text);
            }
            MetadataValue::U32(number) => {
                infos.extend_from_slice(&4u32.to_le_bytes());
                infos.extend_from_slice(&number.to_le_bytes());
            }
        }
    }
    let mut offset = 0u64;
    for (index, (tensor, length)) in document.tensors.iter().zip(&lengths).enumerate() {
        write_string(&mut infos, &tensor.name);
        let dims: Vec<u64> = tensor.shape.iter().rev().copied().collect();
        infos.extend_from_slice(&(dims.len() as u32).to_le_bytes());
        for dim in &dims {
            infos.extend_from_slice(&dim.to_le_bytes());
        }
        let emitted_type = if quantized[index] {
            emitted_type
        } else {
            tensor.ggml_type
        };
        infos.extend_from_slice(&emitted_type.to_le_bytes());
        infos.extend_from_slice(&offset.to_le_bytes());
        offset += *length as u64;
        offset += (32 - (offset % 32)) % 32;
    }
    if let Some(last) = lengths.last() {
        offset -= (32 - ((*last as u64) % 32)) % 32;
    }
    let data_size = offset;
    let header_padding = (32 - ((24 + infos.len()) % 32)) % 32;
    let mut out = Vec::with_capacity(24 + infos.len() + header_padding + data_size as usize);
    out.extend_from_slice(b"GGUF");
    out.extend_from_slice(&3u32.to_le_bytes());
    out.extend_from_slice(&(document.tensors.len() as u64).to_le_bytes());
    out.extend_from_slice(&(metadata.len() as u64).to_le_bytes());
    out.extend_from_slice(&infos);
    out.extend_from_slice(&vec![0u8; header_padding]);
    for (index, tensor) in document.tensors.iter().enumerate() {
        let begin = document.data_start + tensor.source_offset;
        if quantized[index] {
            let width = if tensor.dtype == "F32" { 4 } else { 2 };
            let raw = &source[begin..begin + tensor.length];
            let step = if quant == Some("Q4_K") || quant == Some("Q5_K") || quant == Some("Q6_K") {
                QK_K
            } else {
                QK8_0
            } * width;
            for offset in (0..raw.len()).step_by(step) {
                let block = match quant {
                    Some("Q4_0") => quantize_q4_0_block(raw, offset, &tensor.dtype),
                    Some("Q5_0") => quantize_q5_0_block(raw, offset, &tensor.dtype),
                    Some("Q4_K") => quantize_q4_k_block(raw, offset, &tensor.dtype),
                    Some("Q5_K") => quantize_q5_k_block(raw, offset, &tensor.dtype),
                    Some("Q6_K") => quantize_q6_k_block(raw, offset, &tensor.dtype),
                    _ => quantize_q8_0_block(raw, offset, &tensor.dtype),
                };
                out.extend_from_slice(&block);
            }
        } else {
            out.extend_from_slice(&source[begin..begin + tensor.length]);
        }
        if index + 1 != document.tensors.len() {
            let padding = (32 - (lengths[index] % 32)) % 32;
            out.extend_from_slice(&vec![0u8; padding]);
        }
    }
    out
}
pub fn expected_gguf(document: &SafetensorsDocument, source: &[u8]) -> Vec<u8> {
    let mut infos = Vec::new();
    let metadata = metadata_entries();
    // Cuerpo de infos SIN los contadores: `out` los emite explícitamente
    // antes (PREFIX_BYTES ya los cuenta), igual que el escritor Python.
    for (key, value) in &metadata {
        write_string(&mut infos, key);
        match value {
            MetadataValue::Str(text) => {
                infos.extend_from_slice(&8u32.to_le_bytes());
                write_string(&mut infos, text);
            }
            MetadataValue::U32(number) => {
                infos.extend_from_slice(&4u32.to_le_bytes());
                infos.extend_from_slice(&number.to_le_bytes());
            }
        }
    }
    let mut offset = 0u64;
    for tensor in &document.tensors {
        write_string(&mut infos, &tensor.name);
        let dims: Vec<u64> = tensor.shape.iter().rev().copied().collect();
        infos.extend_from_slice(&(dims.len() as u32).to_le_bytes());
        for dim in &dims {
            infos.extend_from_slice(&dim.to_le_bytes());
        }
        infos.extend_from_slice(&tensor.ggml_type.to_le_bytes());
        infos.extend_from_slice(&offset.to_le_bytes());
        offset += tensor.length as u64;
        offset += (32 - (offset % 32)) % 32;
    }
    if let Some(last) = document.tensors.last() {
        offset -= (32 - ((last.length as u64) % 32)) % 32;
    }
    let data_size = offset;
    let header_padding = (32 - ((24 + infos.len()) % 32)) % 32;
    let mut out = Vec::with_capacity(24 + infos.len() + header_padding + data_size as usize);
    out.extend_from_slice(b"GGUF");
    out.extend_from_slice(&3u32.to_le_bytes());
    out.extend_from_slice(&(document.tensors.len() as u64).to_le_bytes());
    out.extend_from_slice(&(metadata.len() as u64).to_le_bytes());
    out.extend_from_slice(&infos);
    out.extend_from_slice(&vec![0u8; header_padding]);
    for (index, tensor) in document.tensors.iter().enumerate() {
        let begin = document.data_start + tensor.source_offset;
        out.extend_from_slice(&source[begin..begin + tensor.length]);
        if index + 1 != document.tensors.len() {
            let padding = (32 - (tensor.length % 32)) % 32;
            out.extend_from_slice(&vec![0u8; padding]);
        }
    }
    out
}
fn file_slice_digest(bytes: &[u8], start: usize, length: usize) -> Result<Value, LarmError> {
    let slice = bytes
        .get(start..start + length)
        .ok_or_else(|| invalid("gguf_slice_out_of_bounds"))?;
    Ok(json!({"sha256":digest(slice),"size_bytes":length.to_string()}))
}
fn read_json(path: &Path) -> Result<Value, LarmError> {
    crate::package::check_managed_path(path)?;
    let raw = std::fs::read(path).map_err(super::io)?;
    if raw.len() > 1 << 20 {
        return Err(invalid("gguf_oracle_document_too_large"));
    }
    serde_json::from_slice(&raw).map_err(|_| invalid("gguf_oracle_document_invalid"))
}
fn db_error(_: sqlx::Error) -> LarmError {
    error(ErrorCode::ResourceUnavailable, "conformance_gguf_ledger_io")
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
        value.insert(name.into(),serde_json::from_str(&raw).map_err(|_|invalid("gguf_snapshot_invalid"))?);
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
        json!({"phase":row.get::<String,_>(0),"attempt":row.get::<i64,_>(1),"document":serde_json::from_str::<Value>(&row.get::<String,_>(2)).map_err(|_|invalid("gguf_job_invalid"))?}),
    )
}
impl GgufTransformationSuite {
    pub fn new(config: GgufConfig) -> Result<Self, LarmError> {
        for key in ["input_variant_id", "storage_location_id"] {
            if !config.profile[key]
                .as_str()
                .is_some_and(larm_domain::bridge::is_uuid_v7)
            {
                return Err(invalid("gguf_config_invalid"));
            }
        }
        if config.profile["input"]["relative_path"] != "fixture.safetensors" {
            return Err(invalid("gguf_config_invalid"));
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
            return Err(invalid("gguf_fixture_size_invalid"));
        }
        let document = parse_safetensors(&input)?;
        let suite = Self {
            config,
            bundle: bundle()?,
            expected: expected_gguf(&document, &input),
            expected_quant: expected_gguf_quantized(&document, &input, Some("Q8_0")),
            expected_quant4: expected_gguf_quantized(&document, &input, Some("Q4_0")),
            expected_quant5: expected_gguf_quantized(&document, &input, Some("Q5_0")),
            expected_quantk: expected_gguf_quantized(&document, &input, Some("Q4_K")),
            expected_quant5k: expected_gguf_quantized(&document, &input, Some("Q5_K")),
            expected_quant6k: expected_gguf_quantized(&document, &input, Some("Q6_K")),
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
        json!({"input_variant_id":self.config.profile["input_variant_id"],"adapter_id":"product.transformation.gguf","tool_version":"1.1.0","target_format":"GGUF","parameters":{},"storage_location_id":self.config.profile["storage_location_id"],"hard_budget_bytes":null})
    }
    fn input(&self, session: &NativeSession) -> PathBuf {
        session.work_dir().join("input/fixture.safetensors")
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
        report: &mut GgufReport,
        method: &str,
        params: Value,
        access: &Access,
    ) -> Result<Value, LarmError> {
        self.call_with_timeout(
            session,
            report,
            method,
            params,
            access,
            Duration::from_secs(30),
        )
        .await
    }
    async fn call_with_timeout(
        &self,
        session: &NativeSession,
        report: &mut GgufReport,
        method: &str,
        params: Value,
        access: &Access,
        timeout: Duration,
    ) -> Result<Value, LarmError> {
        Contracts::shared()?.request(method, &params)?;
        report.methods.insert(method.into());
        session
            .call(Request::new(method, params), access, Some(timeout), None)
            .await
    }
    async fn refused(
        &self,
        session: &NativeSession,
        report: &mut GgufReport,
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
                Err(invalid("gguf_unexpected_success"))
            }
        }
    }
    async fn plan(
        &self,
        session: &NativeSession,
        report: &mut GgufReport,
    ) -> Result<Value, LarmError> {
        self.plan_with(session, report, json!({}), &self.expected)
            .await
    }
    /// Plan con parámetros explícitos (p.ej. cuantización Q8_0): el predicted
    /// del binding se contrasta contra la emisión espejo del oráculo para ese
    /// parámetro, y la cota privada con el tamaño esperado correspondiente.
    async fn plan_with(
        &self,
        session: &NativeSession,
        report: &mut GgufReport,
        parameters: Value,
        expected: &[u8],
    ) -> Result<Value, LarmError> {
        // Los formatos K cuantizan el fixture completo en el preflight
        // (ruta de emisión única): su plan/execute legítimos tardan minutos
        // en Python stdlib — el timeout es un detector de cuelgue.
        self.plan_with_timeout(
            session,
            report,
            parameters,
            expected,
            Duration::from_secs(30),
        )
        .await
    }
    async fn plan_with_timeout(
        &self,
        session: &NativeSession,
        report: &mut GgufReport,
        parameters: Value,
        expected: &[u8],
        timeout: Duration,
    ) -> Result<Value, LarmError> {
        let mut request = self.request();
        request["parameters"] = parameters;
        let plan = self
            .call_with_timeout(
                session,
                report,
                "transformation.plan",
                request,
                &self.access(session),
                timeout,
            )
            .await?;
        let mut unsigned = plan.clone();
        unsigned
            .as_object_mut()
            .ok_or_else(|| invalid("gguf_plan_invalid"))?
            .remove("plan_hash");
        report.check(
            "plan-jcs",
            json!(hash(&unsigned)?),
            plan["plan_hash"].clone(),
        )?;
        report.check(
            "plan-predicted-by-rust-gguf-oracle",
            descriptor(expected),
            plan["steps"][0]["request"]["binding"]["predicted"].clone(),
        )?;
        let upper = expected.len() as u64 + (3 << 20);
        let margin = (1 << 30).max(upper.div_ceil(20));
        report.check("private-storage-estimate",json!({"upper":upper.to_string(),"required":(upper+margin).to_string(),"confidence":"ESTIMATED","eligible":false}),
            json!({"upper":plan["storage_estimate"]["upper_bound_bytes"],"required":plan["storage_estimate"]["required_free_bytes"],"confidence":plan["storage_estimate"]["confidence"],"eligible":plan["storage_estimate"]["per_volume"][0]["eligible"]}))?;
        Ok(plan)
    }
    async fn execute(
        &self,
        session: &NativeSession,
        report: &mut GgufReport,
        plan: &Value,
    ) -> Result<Value, LarmError> {
        self.execute_with_timeout(session, report, plan, Duration::from_secs(30))
            .await
    }
    async fn execute_with_timeout(
        &self,
        session: &NativeSession,
        report: &mut GgufReport,
        plan: &Value,
        timeout: Duration,
    ) -> Result<Value, LarmError> {
        self.call_with_timeout(
            session,
            report,
            "transformation.execute",
            execute(plan),
            &self.access(session),
            timeout,
        )
        .await
    }
    async fn state(
        &self,
        db: &mut SqliteConnection,
        report: &mut GgufReport,
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
    async fn output(
        &self,
        session: &NativeSession,
        db: &mut SqliteConnection,
        report: &mut GgufReport,
        receipt: &Value,
        original: &[u8],
    ) -> Result<Value, LarmError> {
        let id = receipt["job_id"]
            .as_str()
            .filter(|s| larm_domain::bridge::is_uuid_v7(s))
            .ok_or_else(|| invalid("gguf_job_invalid"))?;
        let state = job(db, id).await?;
        let attempt = state["attempt"]
            .as_i64()
            .filter(|v| *v > 0)
            .ok_or_else(|| invalid("gguf_attempt_invalid"))?;
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
            .ok_or_else(|| invalid("gguf_manifest_invalid"))?;
        let names = artifacts
            .iter()
            .filter_map(|a| a["relative_path"].as_str())
            .collect::<BTreeSet<_>>();
        report.check(
            "manifest-only-owned-artifacts",
            json!([
                format!("{relative}/model.gguf"),
                format!("{relative}/provenance.json")
            ]),
            json!(names),
        )?;
        let actual = std::fs::read(root.join("model.gguf")).map_err(super::io)?;
        report.check(
            "real-gguf-rust-oracle",
            json!({"descriptor":descriptor(&self.expected),"byte_exact":true,"changed":true}),
            json!({"descriptor":descriptor(&actual),"byte_exact":actual==self.expected,"changed":actual!=original}),
        )?;
        let container = parse_gguf(&actual)?;
        report.check(
            "gguf-container-structure",
            json!({"version":3u32,"alignment":32u32,"tensor_count":container.tensors.len(),"kv":
                json!({"general.architecture":"product.test","general.name":"larm-gguf-product",
                       "general.file_type":0u32,"general.alignment":32u32,
                       "general.source.format":"safetensors"})}),
            json!({"version":container.version,"alignment":container.alignment,"tensor_count":container.tensors.len(),
                   "kv":container.metadata.iter().map(|(k,(v,_))|(k.clone(),v.clone())).collect::<BTreeMap<_,_>>()}),
        )?;
        let source = parse_safetensors(original)?;
        for tensor in &container.tensors {
            let source_tensor = source
                .tensors
                .iter()
                .find(|t| t.name == tensor.name)
                .ok_or_else(|| invalid("gguf_tensor_source_missing"))?;
            report.check(
                "gguf-tensor-dims-reversed",
                json!(source_tensor.shape.iter().rev().collect::<Vec<_>>()),
                json!(tensor.dims),
            )?;
            report.check(
                "gguf-tensor-dtype",
                json!(source_tensor.ggml_type),
                json!(tensor.ggml_type),
            )?;
            let begin = container.data_begin + tensor.offset as usize;
            report.check(
                "gguf-tensor-bytes-match-source",
                json!(file_slice_digest(
                    original,
                    source.data_start + source_tensor.source_offset,
                    source_tensor.length
                )?),
                json!(file_slice_digest(&actual, begin, source_tensor.length)?),
            )?;
        }
        let stored = read_json(&root.join("manifest.json"))?;
        report.check("manifest-file-and-rpc", stored, manifest.clone())?;
        let mut descriptors = Vec::new();
        for artifact in artifacts {
            let name = artifact["relative_path"]
                .as_str()
                .ok_or_else(|| invalid("gguf_artifact_invalid"))?;
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
        report.check("provenance-backed-by-bytes",json!({"input":self.config.profile["input"],"variant":self.config.profile["input_variant_id"],"output":descriptor(&self.expected),"tool":{"id":session.manifest().id,"version":"1.1.0","adapter_package_sha256":session.manifest().package_sha256,"package_verification":"HOST_OWNED"},"parameters":{},"job_id":id,"attempt":attempt,"logs":["input_verified","gguf_written","output_verified"]}),
            json!({"input":provenance["source"],"variant":provenance["input_variant_id"],"output":provenance["output"],"tool":provenance["tool"],"parameters":provenance["parameters"],"job_id":provenance["job_id"],"attempt":provenance["attempt"],"logs":provenance["logs"]}))?;
        let path = session
            .work_dir()
            .join("state/job-status")
            .join(format!("{id}.json"));
        let plain = PathBuf::from(path.to_string_lossy().trim_start_matches(r"\\?\"));
        let uri =
            url::Url::from_file_path(&plain).map_err(|_| invalid("gguf_status_uri_invalid"))?;
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
        report.checkpoints.insert(format!("output-{id}-{attempt}"),json!({"manifest":manifest,"provenance":provenance,"gguf_sha256":digest(&actual),"gguf_bytes":actual.len().to_string()}));
        Ok(manifest)
    }
    async fn interrupted(
        &self,
        session: &mut NativeSession,
        db: &mut SqliteConnection,
        report: &mut GgufReport,
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
        let event = loop {
            tokio::select! {event=progress.recv()=>{let event=event.map_err(|_|invalid("gguf_progress_missing"))?;if event.phase=="GGUF_TENSOR"{break event;}},_=&mut pending=>return Err(invalid("gguf_completed_before_tensor_checkpoint"))}
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
        if how == "eof" {
            drop(pending);
            session.restart().await?;
        } else {
            if how == "cancel" {
                report.check("cancel-ack", json!(true), json!(session.cancel(&id).await?))?;
            } else if how == "crash" {
                session.crash()?;
            } else if how == "corrupt" {
                let mut file = std::fs::OpenOptions::new()
                    .write(true)
                    .open(self.input(session))
                    .map_err(super::io)?;
                file.write_all(b"!").map_err(super::io)?;
                file.sync_all().map_err(super::io)?;
            } else {
                return Err(invalid("gguf_interrupt_mode_invalid"));
            }
            let failure = pending
                .await
                .err()
                .ok_or_else(|| invalid("gguf_interrupted_call_succeeded"))?;
            report.check(
                if how == "crash" {
                    "worker-crash"
                } else if how == "corrupt" {
                    "combined-fault-response"
                } else {
                    "cancel-result"
                },
                if how == "crash" {
                    json!("ADAPTER_CRASHED")
                } else if how == "corrupt" {
                    json!("product_gguf_persistence_failed")
                } else {
                    json!("operation_cancelled")
                },
                if how == "crash" {
                    json!(failure.code.as_str())
                } else {
                    json!(failure.details.reason)
                },
            )?;
            report.checkpoints.insert(
                format!("{how}-error"),
                serde_json::to_value(failure).map_err(|_| invalid("gguf_error_invalid"))?,
            );
        }
        report.checkpoints.insert(
            how.into(),
            json!({"rpc_id":id,"job_id":active.0,"attempt":active.1,"durable_offset":active.2}),
        );
        Ok((active.0, active.2))
    }
    async fn functional(
        &self,
        session: &mut NativeSession,
        db: &mut SqliteConnection,
        report: &mut GgufReport,
        original: &[u8],
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
        report.check("description-real-tool",json!({"formats_input":["SAFETENSORS"],"formats_output":["GGUF"],"parameter_schema":{"type":"object","properties":{"quantization":{"type":"string","enum":["Q8_0","Q4_0","Q5_0","Q4_K","Q5_K","Q6_K"]}},"additionalProperties":false},"tool_version":"1.1.0","pause_supported":false}),description)?;
        let before = snapshot(db).await?;
        let sentinel = file_digest(&self.config.forbidden)?;
        for (key, value, reason) in [
            (
                "input_variant_id",
                json!(uuid::Uuid::now_v7().to_string()),
                "product_gguf_resource_not_bound",
            ),
            (
                "storage_location_id",
                json!(uuid::Uuid::now_v7().to_string()),
                "product_gguf_resource_not_bound",
            ),
            (
                "parameters",
                json!({"script":"forbidden"}),
                "product_gguf_options_not_supported",
            ),
            (
                "parameters",
                json!({"quantization":"Q2_K"}),
                "product_gguf_quant_unsupported",
            ),
            (
                "tool_version",
                json!("2.0"),
                "product_gguf_options_not_supported",
            ),
            (
                "target_format",
                json!("ONNX"),
                "product_gguf_options_not_supported",
            ),
            (
                "hard_budget_bytes",
                json!("1"),
                "product_gguf_hard_budget_insufficient",
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
            "product_gguf_plan_hash_mismatch",
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
            "product_gguf_approval_owned_by_core",
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
                "product_gguf_input_changed",
            )
            .await?;
            drop(restore);
        }
        let mut expired = self.plan(session, report).await?;
        expired["expires_at"] = json!("2000-01-01T00:00:00.000Z");
        let mut unsigned = expired.clone();
        unsigned
            .as_object_mut()
            .ok_or_else(|| invalid("gguf_plan_invalid"))?
            .remove("plan_hash");
        expired["plan_hash"] = json!(hash(&unsigned)?);
        sqlx::query("UPDATE ref_plan SET document=?,hash=? WHERE id=?")
            .bind(serde_json::to_string(&expired).map_err(|_| invalid("gguf_plan_invalid"))?)
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
            "product_gguf_plan_expired",
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
                "product_gguf_persistence_failed",
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
                "product_gguf_io_failed",
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
            "product_gguf_job_not_completed",
        )
        .await?;
        let retry = self.plan(session, report).await?;
        let completed = self.execute(session, report, &retry).await?;
        report.check("retry-same-job", json!(failed), completed["job_id"].clone())?;
        let manifest = self
            .output(session, db, report, &completed, original)
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
                "product_gguf_io_failed",
            )
            .await?;
        report.check(
            "old-error-immutable",
            serde_json::to_value(original_error).map_err(|_| invalid("gguf_error_invalid"))?,
            serde_json::to_value(error).map_err(|_| invalid("gguf_error_invalid"))?,
        )?;
        report.check("replay-no-new-events", before, snapshot(db).await?)?;
        let payload = session
            .work_dir()
            .join("output")
            .join(format!("{failed}-2/model.gguf"));
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
                "product_gguf_output_changed",
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
        // Cuantización real Q8_0: plan/execute con el parámetro declarado y
        // contenedor publicado contrastado byte a byte contra la emisión
        // espejo del oráculo (el fixture de conformidad es mixto: weight.big
        // F32 [983040] → Q8_0; weight.scale F16 [4] y n_positions I64 →
        // verbatim).
        let quant_plan = self
            .plan_with(
                session,
                report,
                json!({"quantization":"Q8_0"}),
                &self.expected_quant,
            )
            .await?;
        let quant_job = self.execute(session, report, &quant_plan).await?;
        let quant_id = quant_job["job_id"].as_str().unwrap_or_default().to_owned();
        let quant_payload = session
            .work_dir()
            .join("output")
            .join(format!("{quant_id}-1/model.gguf"));
        let quant_raw = std::fs::read(&quant_payload).map_err(super::io)?;
        report.check(
            "quant-container-bytes-match-oracle",
            json!(digest(&self.expected_quant)),
            json!(digest(&quant_raw)),
        )?;
        {
            let quant_parsed = parse_gguf(&quant_raw)?;
            let mut observed_types = Vec::new();
            for tensor in &quant_parsed.tensors {
                observed_types.push(json!({"name":tensor.name,"type":tensor.ggml_type}));
            }
            report.check(
                "quant-tensor-types",
                json!([
                    {"name":"weight.big","type":GGML_Q8_0},
                    {"name":"weight.scale","type":GGML_F16},
                    {"name":"n_positions","type":GGML_I64},
                ]),
                json!(observed_types),
            )?;
            report.check(
                "quant-file-type-kv",
                json!(7),
                quant_parsed
                    .metadata
                    .get("general.file_type")
                    .map(|(value, _kind)| value.clone())
                    .unwrap_or(json!(null)),
            )?;
        }
        // Cuantización real Q4_0: segundo formato del enum, contenedor
        // publicado byte-exacto contra la emisión espejo (weight.big → type 2).
        let quant4_plan = self
            .plan_with(
                session,
                report,
                json!({"quantization":"Q4_0"}),
                &self.expected_quant4,
            )
            .await?;
        let quant4_job = self.execute(session, report, &quant4_plan).await?;
        let quant4_id = quant4_job["job_id"].as_str().unwrap_or_default().to_owned();
        let quant4_payload = session
            .work_dir()
            .join("output")
            .join(format!("{quant4_id}-1/model.gguf"));
        let quant4_raw = std::fs::read(&quant4_payload).map_err(super::io)?;
        report.check(
            "quant4-container-bytes-match-oracle",
            json!(digest(&self.expected_quant4)),
            json!(digest(&quant4_raw)),
        )?;
        {
            let quant4_parsed = parse_gguf(&quant4_raw)?;
            let mut observed_types = Vec::new();
            for tensor in &quant4_parsed.tensors {
                observed_types.push(json!({"name":tensor.name,"type":tensor.ggml_type}));
            }
            report.check(
                "quant4-tensor-types",
                json!([
                    {"name":"weight.big","type":GGML_Q4_0},
                    {"name":"weight.scale","type":GGML_F16},
                    {"name":"n_positions","type":GGML_I64},
                ]),
                json!(observed_types),
            )?;
            report.check(
                "quant4-file-type-kv",
                json!(7),
                quant4_parsed
                    .metadata
                    .get("general.file_type")
                    .map(|(value, _kind)| value.clone())
                    .unwrap_or(json!(null)),
            )?;
        }
        // Cuantización real Q5_0: tercer formato del enum, contenedor
        // publicado byte-exacto contra la emisión espejo (weight.big → type 6).
        let quant5_plan = self
            .plan_with(
                session,
                report,
                json!({"quantization":"Q5_0"}),
                &self.expected_quant5,
            )
            .await?;
        let quant5_job = self.execute(session, report, &quant5_plan).await?;
        let quant5_id = quant5_job["job_id"].as_str().unwrap_or_default().to_owned();
        let quant5_payload = session
            .work_dir()
            .join("output")
            .join(format!("{quant5_id}-1/model.gguf"));
        let quant5_raw = std::fs::read(&quant5_payload).map_err(super::io)?;
        report.check(
            "quant5-container-bytes-match-oracle",
            json!(digest(&self.expected_quant5)),
            json!(digest(&quant5_raw)),
        )?;
        {
            let quant5_parsed = parse_gguf(&quant5_raw)?;
            let mut observed_types = Vec::new();
            for tensor in &quant5_parsed.tensors {
                observed_types.push(json!({"name":tensor.name,"type":tensor.ggml_type}));
            }
            report.check(
                "quant5-tensor-types",
                json!([
                    {"name":"weight.big","type":GGML_Q5_0},
                    {"name":"weight.scale","type":GGML_F16},
                    {"name":"n_positions","type":GGML_I64},
                ]),
                json!(observed_types),
            )?;
            report.check(
                "quant5-file-type-kv",
                json!(7),
                quant5_parsed
                    .metadata
                    .get("general.file_type")
                    .map(|(value, _kind)| value.clone())
                    .unwrap_or(json!(null)),
            )?;
        }
        // Cuantización real Q4_K: super-bloques K (type 12, 144 bytes por
        // bloque de 256 elementos) — weight.big (3.145.728 elementos) cuantiza;
        // weight.scale/n_positions verbatim.
        let quantk_plan = self
            .plan_with_timeout(
                session,
                report,
                json!({"quantization":"Q4_K"}),
                &self.expected_quantk,
                Duration::from_secs(600),
            )
            .await?;
        let quantk_job = self
            .execute_with_timeout(session, report, &quantk_plan, Duration::from_secs(600))
            .await?;
        let quantk_id = quantk_job["job_id"].as_str().unwrap_or_default().to_owned();
        let quantk_payload = session
            .work_dir()
            .join("output")
            .join(format!("{quantk_id}-1/model.gguf"));
        let quantk_raw = std::fs::read(&quantk_payload).map_err(super::io)?;
        report.check(
            "quantk-container-bytes-match-oracle",
            json!(digest(&self.expected_quantk)),
            json!(digest(&quantk_raw)),
        )?;
        {
            let quantk_parsed = parse_gguf(&quantk_raw)?;
            let mut observed_types = Vec::new();
            for tensor in &quantk_parsed.tensors {
                observed_types.push(json!({"name":tensor.name,"type":tensor.ggml_type}));
            }
            report.check(
                "quantk-tensor-types",
                json!([
                    {"name":"weight.big","type":GGML_Q4_K},
                    {"name":"weight.scale","type":GGML_F16},
                    {"name":"n_positions","type":GGML_I64},
                ]),
                json!(observed_types),
            )?;
            report.check(
                "quantk-file-type-kv",
                json!(7),
                quantk_parsed
                    .metadata
                    .get("general.file_type")
                    .map(|(value, _kind)| value.clone())
                    .unwrap_or(json!(null)),
            )?;
        }
        // Cuantización real Q5_K: super-bloques K de 5 bits (type 13, 176
        // bytes por bloque de 256 elementos).
        let quant5k_plan = self
            .plan_with_timeout(
                session,
                report,
                json!({"quantization":"Q5_K"}),
                &self.expected_quant5k,
                Duration::from_secs(600),
            )
            .await?;
        let quant5k_job = self
            .execute_with_timeout(session, report, &quant5k_plan, Duration::from_secs(600))
            .await?;
        let quant5k_id = quant5k_job["job_id"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        let quant5k_payload = session
            .work_dir()
            .join("output")
            .join(format!("{quant5k_id}-1/model.gguf"));
        let quant5k_raw = std::fs::read(&quant5k_payload).map_err(super::io)?;
        report.check(
            "quant5k-container-bytes-match-oracle",
            json!(digest(&self.expected_quant5k)),
            json!(digest(&quant5k_raw)),
        )?;
        {
            let quant5k_parsed = parse_gguf(&quant5k_raw)?;
            let mut observed_types = Vec::new();
            for tensor in &quant5k_parsed.tensors {
                observed_types.push(json!({"name":tensor.name,"type":tensor.ggml_type}));
            }
            report.check(
                "quant5k-tensor-types",
                json!([
                    {"name":"weight.big","type":GGML_Q5_K},
                    {"name":"weight.scale","type":GGML_F16},
                    {"name":"n_positions","type":GGML_I64},
                ]),
                json!(observed_types),
            )?;
            report.check(
                "quant5k-file-type-kv",
                json!(7),
                quant5k_parsed
                    .metadata
                    .get("general.file_type")
                    .map(|(value, _kind)| value.clone())
                    .unwrap_or(json!(null)),
            )?;
        }
        // Cuantización real Q6_K: tercer super-bloque K (type 14, 210 bytes
        // por bloque de 256 elementos, cuantización simétrica).
        let quant6k_plan = self
            .plan_with_timeout(
                session,
                report,
                json!({"quantization":"Q6_K"}),
                &self.expected_quant6k,
                Duration::from_secs(600),
            )
            .await?;
        let quant6k_job = self
            .execute_with_timeout(session, report, &quant6k_plan, Duration::from_secs(600))
            .await?;
        let quant6k_id = quant6k_job["job_id"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        let quant6k_payload = session
            .work_dir()
            .join("output")
            .join(format!("{quant6k_id}-1/model.gguf"));
        let quant6k_raw = std::fs::read(&quant6k_payload).map_err(super::io)?;
        report.check(
            "quant6k-container-bytes-match-oracle",
            json!(digest(&self.expected_quant6k)),
            json!(digest(&quant6k_raw)),
        )?;
        {
            let quant6k_parsed = parse_gguf(&quant6k_raw)?;
            let mut observed_types = Vec::new();
            for tensor in &quant6k_parsed.tensors {
                observed_types.push(json!({"name":tensor.name,"type":tensor.ggml_type}));
            }
            report.check(
                "quant6k-tensor-types",
                json!([
                    {"name":"weight.big","type":GGML_Q6_K},
                    {"name":"weight.scale","type":GGML_F16},
                    {"name":"n_positions","type":GGML_I64},
                ]),
                json!(observed_types),
            )?;
            report.check(
                "quant6k-file-type-kv",
                json!(7),
                quant6k_parsed
                    .metadata
                    .get("general.file_type")
                    .map(|(value, _kind)| value.clone())
                    .unwrap_or(json!(null)),
            )?;
        }
        let cancel = self.plan(session, report).await?;
        let (cancelled, _) = self
            .interrupted(session, db, report, &cancel, "cancel")
            .await?;
        self.state(db, report, &cancelled, "CANCELLED", 1, 0)
            .await?;
        report.check("cancel-cleans-only-attempt",json!({"stage_files":0,"old_output_sha":digest(&self.expected)}),json!({"stage_files":std::fs::read_dir(session.work_dir().join("state/staging").join(format!("{cancelled}-1"))).map_err(super::io)?.count(),"old_output_sha":file_digest(&payload)?}))?;
        let crash = self.plan(session, report).await?;
        let (crashed, offset) = self
            .interrupted(session, db, report, &crash, "crash")
            .await?;
        let partial = session
            .work_dir()
            .join("state/staging")
            .join(format!("{crashed}-1/model.gguf"));
        report.check("crash-durable-bytes-and-resources",json!({"durable":true,"held":2}),json!({"durable":std::fs::metadata(&partial).map_err(super::io)?.len()>=offset as u64,"held":held(db,&crashed).await?}))?;
        let partial_sha = file_digest(&partial)?;
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
        self.output(session, db, report, &recovered_receipt, original)
            .await?;
        report.check(
            "restart-does-not-resume-old-partial",
            json!(partial_sha),
            json!(file_digest(&partial)?),
        )?;
        let fault = self.plan(session, report).await?;
        sqlx::query("CREATE TRIGGER p026_transform_final BEFORE INSERT ON ref_outbox WHEN (SELECT phase FROM ref_event WHERE id=NEW.event_id)='COMPLETED' BEGIN SELECT RAISE(ABORT,'owned_final_fault'); END").execute(&mut *db).await.map_err(db_error)?;
        let failure = self
            .refused(
                session,
                report,
                "transformation.execute",
                execute(&fault),
                &access,
                "product_gguf_persistence_failed",
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
            .join(format!("{uncertain}-1/model.gguf"));
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
        self.output(session, db, report, &receipt, original).await?;
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
        report.check("original-integrity-precedes-outbox-error",json!({"code":"INTEGRITY_FAILED","reason":"product_gguf_final_verification_failed"}),json!({"code":recovered["document"]["error"]["code"],"reason":recovered["document"]["error"]["details"]["reason"]}))?;
        let retry = self.plan(session, report).await?;
        let receipt = self.execute(session, report, &retry).await?;
        report.check(
            "combined-fault-retry-same-job",
            json!(combined_id),
            receipt["job_id"].clone(),
        )?;
        self.output(session, db, report, &receipt, original).await?;
        let eof = self.plan(session, report).await?;
        let (eof_id, _) = self.interrupted(session, db, report, &eof, "eof").await?;
        self.state(db, report, &eof_id, "CANCELLED", 1, 0).await?;
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
        report: &mut GgufReport,
    ) -> Result<Vec<Observation>, LarmError> {
        if session.manifest().id != "product.transformation.gguf"
            || session.manifest().family.as_str() != "transformation"
            || session.manifest().capabilities != [CAP]
        {
            return Err(invalid("gguf_subject_mismatch"));
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
                return Err(invalid("gguf_oracle_resource_exposed"));
            }
        }
        let original = std::fs::read(&self.config.expected_input).map_err(super::io)?;
        if original.is_empty() || original.len() > 64 << 20 {
            return Err(invalid("gguf_fixture_size_invalid"));
        }
        parse_safetensors(&original)?;
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
            .ok_or_else(|| invalid("gguf_ledger_closed"))?;
        let app: i64 = sqlx::query_scalar("PRAGMA application_id")
            .fetch_one(&mut *db)
            .await
            .map_err(db_error)?;
        let version: i64 = sqlx::query_scalar("PRAGMA user_version")
            .fetch_one(&mut *db)
            .await
            .map_err(db_error)?;
        if app != 0x4C415254 || version != 1 {
            return Err(invalid("gguf_ledger_not_owned"));
        }
        self.functional(session, db, report, &original).await?;
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
            "capability:vendor.product.gguf",
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
impl NativeSuite for GgufTransformationSuite {
    fn descriptor(&self) -> Value {
        json!({"family":"transformation","capabilities":[CAP],"suite":"product.transformation.gguf.v1","bundle":self.bundle,"profile":self.config.profile,
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
        let mut report = GgufReport {
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
            serde_json::to_vec_pretty(&report).map_err(|_| invalid("gguf_report_invalid"))?,
        )
        .map_err(super::io)?;
        report.observations
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::conformance::gguf_golden::{
        FIXTURE_HEX, GOLDEN_GGUF_HEX, QUANT4_GOLDEN_GGUF_HEX, QUANT5K_GOLDEN_GGUF_HEX,
        QUANT5_GOLDEN_GGUF_HEX, QUANT6K_GOLDEN_GGUF_HEX, QUANTK_GOLDEN_GGUF_HEX, QUANT_FIXTURE_HEX,
        QUANT_GOLDEN_GGUF_HEX, QUANT_K_FIXTURE_HEX,
    };

    fn decode(hex: &str) -> Vec<u8> {
        (0..hex.len())
            .step_by(2)
            .map(|index| u8::from_str_radix(&hex[index..index + 2], 16).unwrap())
            .collect()
    }

    /// Réplica byte a byte del fixture de cuantización del paquete Python
    /// (build_quantization_fixture): F32 [64], bloque cero + bloque variado
    /// (ldexp(1+k, -8-(k%7)) con signo alterno), JSON compacto relleno a 8.
    fn build_quantization_fixture() -> Vec<u8> {
        let mut values = Vec::new();
        values.extend_from_slice(&[0.0f32; 32]);
        for index in 0..32usize {
            let magnitude = (1.0 + index as f64) * f64::powi(2.0, -((8 + index % 7) as i32));
            values.push(if index % 2 == 0 {
                magnitude as f32
            } else {
                -(magnitude as f32)
            });
        }
        let mut raw = Vec::new();
        for value in &values {
            raw.extend_from_slice(&value.to_le_bytes());
        }
        let header = format!(
            "{{\"q.weight\":{{\"dtype\":\"F32\",\"shape\":[64],\"data_offsets\":[0,{}]}}}}",
            raw.len()
        );
        let mut document = header.into_bytes();
        document.extend_from_slice(&vec![b' '; (8 - (document.len() % 8)) % 8]);
        let mut out = Vec::new();
        out.extend_from_slice(&(document.len() as u64).to_le_bytes());
        out.extend_from_slice(&document);
        out.extend_from_slice(&raw);
        out
    }

    #[test]
    fn fixture_rust_identico_al_python_q8_0() {
        assert_eq!(build_quantization_fixture(), decode(QUANT_FIXTURE_HEX));
    }

    #[test]
    fn emision_rust_identica_al_escritor_python_q8_0() {
        let fixture = build_quantization_fixture();
        let document = parse_safetensors(&fixture).unwrap();
        assert_eq!(document.tensors.len(), 1);
        let golden = expected_gguf_quantized(&document, &fixture, Some("Q8_0"));
        assert_eq!(golden, decode(QUANT_GOLDEN_GGUF_HEX));
        // El contenedor cuantizado: tensor Q8_0 y file_type 7 (kv u32).
        let container = parse_gguf(&golden).unwrap();
        assert_eq!(container.tensors[0].ggml_type, GGML_Q8_0);
        assert_eq!(container.tensors[0].dims, vec![64]);
        assert_eq!(
            container.metadata["general.file_type"],
            (serde_json::json!(7), 4)
        );
        // Sin cuantizables (None o fixture no divisible): idéntico al verbatim
        // y file_type 0.
        let plain = parse_safetensors(&decode(FIXTURE_HEX)).unwrap();
        assert_eq!(
            expected_gguf_quantized(&plain, &decode(FIXTURE_HEX), Some("Q8_0")),
            expected_gguf(&plain, &decode(FIXTURE_HEX))
        );
    }

    #[test]
    fn emision_rust_identica_al_escritor_python_q4_0() {
        let fixture = build_quantization_fixture();
        let document = parse_safetensors(&fixture).unwrap();
        let golden = expected_gguf_quantized(&document, &fixture, Some("Q4_0"));
        assert_eq!(golden, decode(QUANT4_GOLDEN_GGUF_HEX));
        let container = parse_gguf(&golden).unwrap();
        assert_eq!(container.tensors[0].ggml_type, GGML_Q4_0);
        assert_eq!(container.tensors[0].dims, vec![64]);
        assert_eq!(
            container.metadata["general.file_type"],
            (serde_json::json!(7), 4)
        );
        // El contenedor Q4_0 (18 bytes/bloque) es menor que el Q8_0 (34).
        let golden_q8 = expected_gguf_quantized(&document, &fixture, Some("Q8_0"));
        assert!(golden.len() < golden_q8.len());
    }

    #[test]
    fn emision_rust_identica_al_escritor_python_q5_0() {
        let fixture = build_quantization_fixture();
        let document = parse_safetensors(&fixture).unwrap();
        let golden = expected_gguf_quantized(&document, &fixture, Some("Q5_0"));
        assert_eq!(golden, decode(QUANT5_GOLDEN_GGUF_HEX));
        let container = parse_gguf(&golden).unwrap();
        assert_eq!(container.tensors[0].ggml_type, GGML_Q5_0);
        assert_eq!(container.tensors[0].dims, vec![64]);
        assert_eq!(
            container.metadata["general.file_type"],
            (serde_json::json!(7), 4)
        );
        // Orden de tamaños: Q4_0 < Q5_0 < Q8_0.
        let golden_q4 = expected_gguf_quantized(&document, &fixture, Some("Q4_0"));
        assert!(golden_q4.len() < golden.len());
        let golden_q8 = expected_gguf_quantized(&document, &fixture, Some("Q8_0"));
        assert!(golden.len() < golden_q8.len());
    }

    /// Réplica byte a byte del fixture K del paquete Python
    /// (build_quantization_k_fixture): F32 [256] con 8 sub-bloques de rangos
    /// distintos, JSON compacto relleno a 8.
    fn build_quantization_k_fixture() -> Vec<u8> {
        let mut values: Vec<f32> = Vec::new();
        for sub in 0..8usize {
            for index in 0..32usize {
                let value = match sub {
                    0 => 0.0f32,
                    1 => {
                        ((1 + index) as f32)
                            * f32::powi(2.0, -4)
                            * (if index % 2 == 0 { 1.0 } else { -1.0 })
                    }
                    2 => ((1 + index % 8) as f32) * f32::powi(2.0, -14),
                    3 => -(((1 + index % 8) as f32) * f32::powi(2.0, -14)),
                    4 => ((1 + index) as f32) * f32::powi(2.0, -3),
                    5 => 0.5f32 + (index as f32) * f32::powi(2.0, -6),
                    6 => -0.25f32 - (index as f32) * f32::powi(2.0, -8),
                    _ => ((33 - index) as f32) * f32::powi(2.0, -5),
                };
                values.push(value);
            }
        }
        let mut raw = Vec::new();
        for value in &values {
            raw.extend_from_slice(&value.to_le_bytes());
        }
        let header = format!(
            "{{\"q.kweight\":{{\"dtype\":\"F32\",\"shape\":[256],\"data_offsets\":[0,{}]}}}}",
            raw.len()
        );
        let mut document = header.into_bytes();
        document.extend_from_slice(&vec![b' '; (8 - (document.len() % 8)) % 8]);
        let mut out = Vec::new();
        out.extend_from_slice(&(document.len() as u64).to_le_bytes());
        out.extend_from_slice(&document);
        out.extend_from_slice(&raw);
        out
    }

    #[test]
    fn fixture_rust_identico_al_python_q4_k() {
        assert_eq!(build_quantization_k_fixture(), decode(QUANT_K_FIXTURE_HEX));
    }

    #[test]
    fn emision_rust_identica_al_escritor_python_q4_k() {
        let fixture = build_quantization_k_fixture();
        let document = parse_safetensors(&fixture).unwrap();
        assert_eq!(document.tensors.len(), 1);
        let golden = expected_gguf_quantized(&document, &fixture, Some("Q4_K"));
        assert_eq!(golden, decode(QUANTK_GOLDEN_GGUF_HEX));
        let container = parse_gguf(&golden).unwrap();
        assert_eq!(container.tensors[0].ggml_type, GGML_Q4_K);
        assert_eq!(container.tensors[0].dims, vec![256]);
        assert_eq!(
            container.metadata["general.file_type"],
            (serde_json::json!(7), 4)
        );
    }

    #[test]
    fn fixture_rust_identico_al_python_q5_k() {
        assert_eq!(build_quantization_k_fixture(), decode(QUANT_K_FIXTURE_HEX));
    }

    #[test]
    fn emision_rust_identica_al_escritor_python_q5_k() {
        let fixture = build_quantization_k_fixture();
        let document = parse_safetensors(&fixture).unwrap();
        let golden = expected_gguf_quantized(&document, &fixture, Some("Q5_K"));
        assert_eq!(golden, decode(QUANT5K_GOLDEN_GGUF_HEX));
        let container = parse_gguf(&golden).unwrap();
        assert_eq!(container.tensors[0].ggml_type, GGML_Q5_K);
        assert_eq!(container.tensors[0].dims, vec![256]);
        assert_eq!(
            container.metadata["general.file_type"],
            (serde_json::json!(7), 4)
        );
    }

    #[test]
    fn emision_rust_identica_al_escritor_python_q6_k() {
        let fixture = build_quantization_k_fixture();
        let document = parse_safetensors(&fixture).unwrap();
        let golden = expected_gguf_quantized(&document, &fixture, Some("Q6_K"));
        assert_eq!(golden, decode(QUANT6K_GOLDEN_GGUF_HEX));
        let container = parse_gguf(&golden).unwrap();
        assert_eq!(container.tensors[0].ggml_type, GGML_Q6_K);
        assert_eq!(container.tensors[0].dims, vec![256]);
        assert_eq!(
            container.metadata["general.file_type"],
            (serde_json::json!(7), 4)
        );
    }

    #[test]
    fn cuantizacion_bytes_de_bloque_coinciden_con_python() {
        // Bloque exacto del golden: escala f16 + 32 int8 del segundo bloque
        // (el primero es cero exacto); referencia independiente re-derivada
        // desde el fixture con la aritmética del diseño.
        let golden = decode(QUANT_GOLDEN_GGUF_HEX);
        let container = parse_gguf(&golden).unwrap();
        let begin = container.data_begin + container.tensors[0].offset as usize;
        let blocks = &golden[begin..begin + 2 * Q8_0_BLOCK_BYTES];
        assert_eq!(&blocks[..2], &0u16.to_le_bytes(), "bloque cero exacto");
        assert!(blocks[2..Q8_0_BLOCK_BYTES].iter().all(|byte| *byte == 0));
        let fixture = decode(QUANT_FIXTURE_HEX);
        let document = parse_safetensors(&fixture).unwrap();
        let source_begin = document.data_start + document.tensors[0].source_offset;
        let second = quantize_q8_0_block(&fixture[source_begin..source_begin + 256], 128, "F32");
        assert_eq!(
            &blocks[Q8_0_BLOCK_BYTES..2 * Q8_0_BLOCK_BYTES],
            second.as_slice(),
            "segundo bloque distinto del derivado en Rust"
        );
    }

    /// Réplica byte a byte del fixture safetensors del paquete Python
    /// (build_safetensors_fixture): mismo orden de claves, JSON compacto,
    /// relleno con espacios a 8 bytes y los mismos tensores.
    fn build_safetensors_fixture() -> Vec<u8> {
        let mut header = String::from(
            "{\"weight.embedding\":{\"dtype\":\"F32\",\"shape\":[2,3],\"data_offsets\":[0,24]},",
        );
        header.push_str(
            "\"weight.scale\":{\"dtype\":\"F16\",\"shape\":[4],\"data_offsets\":[24,32]},",
        );
        header.push_str(
            "\"n_positions\":{\"dtype\":\"I64\",\"shape\":[3],\"data_offsets\":[32,56]}}",
        );
        let mut document = header.into_bytes();
        document.extend_from_slice(&vec![b' '; (8 - (document.len() % 8)) % 8]);
        let mut out = Vec::new();
        out.extend_from_slice(&(document.len() as u64).to_le_bytes());
        out.extend_from_slice(&document);
        out.extend_from_slice(&1.0f32.to_le_bytes());
        out.extend_from_slice(&2.0f32.to_le_bytes());
        out.extend_from_slice(&3.0f32.to_le_bytes());
        out.extend_from_slice(&4.0f32.to_le_bytes());
        out.extend_from_slice(&5.0f32.to_le_bytes());
        out.extend_from_slice(&6.0f32.to_le_bytes());
        out.extend_from_slice(&15360u16.to_le_bytes());
        out.extend_from_slice(&16384u16.to_le_bytes());
        out.extend_from_slice(&17408u16.to_le_bytes());
        out.extend_from_slice(&18432u16.to_le_bytes());
        out.extend_from_slice(&7i64.to_le_bytes());
        out.extend_from_slice(&8i64.to_le_bytes());
        out.extend_from_slice(&9i64.to_le_bytes());
        out
    }

    #[test]
    fn fixture_rust_identico_al_python() {
        assert_eq!(build_safetensors_fixture(), decode(FIXTURE_HEX));
    }

    #[test]
    fn emision_rust_identica_al_escritor_python() {
        let fixture = build_safetensors_fixture();
        let document = parse_safetensors(&fixture).unwrap();
        assert_eq!(document.tensors.len(), 3);
        assert_eq!(expected_gguf(&document, &fixture), decode(GOLDEN_GGUF_HEX));
    }

    #[test]
    fn parse_gguf_estructura_del_golden() {
        let golden = decode(GOLDEN_GGUF_HEX);
        let container = parse_gguf(&golden).unwrap();
        assert_eq!(container.version, 3);
        assert_eq!(container.alignment, 32);
        assert_eq!(container.tensors.len(), 3);
        assert_eq!(
            container.metadata["general.architecture"],
            (json!("product.test"), 8)
        );
        assert_eq!(container.metadata["general.alignment"], (json!(32), 4));
        assert_eq!(container.metadata["general.file_type"], (json!(0), 4));
        let embedding = &container.tensors[0];
        assert_eq!(embedding.name, "weight.embedding");
        assert_eq!(embedding.dims, vec![3, 2], "dims ggml: eje rápido primero");
        assert_eq!(embedding.ggml_type, GGML_F32);
        let scale = &container.tensors[1];
        assert_eq!(scale.dims, vec![4]);
        assert_eq!(scale.ggml_type, GGML_F16);
        let positions = &container.tensors[2];
        assert_eq!(positions.dims, vec![3]);
        assert_eq!(positions.ggml_type, GGML_I64);
        // Los datos del contenedor coinciden con los tensores del safetensors.
        let fixture = build_safetensors_fixture();
        let source = parse_safetensors(&fixture).unwrap();
        for tensor in &container.tensors {
            let source_tensor = source
                .tensors
                .iter()
                .find(|t| t.name == tensor.name)
                .unwrap();
            let begin = container.data_begin + tensor.offset as usize;
            let end = begin + source_tensor.length;
            let source_begin = source.data_start + source_tensor.source_offset;
            assert_eq!(
                golden[begin..end],
                fixture[source_begin..source_begin + source_tensor.length],
                "bytes del tensor {} alterados",
                tensor.name
            );
        }
    }

    #[test]
    fn negativos_del_oraculo_safetensors() {
        let fixture = build_safetensors_fixture();
        for at in [0usize, 8] {
            let mut damaged = fixture.clone();
            damaged[at] ^= 0x21;
            assert!(parse_safetensors(&damaged).is_err(), "byte {at}");
        }
        // dtype no copiable
        let mut damaged = fixture.clone();
        let dtype_at = fixture
            .windows(5)
            .position(|window| window == b"\"F32\"")
            .unwrap();
        damaged[dtype_at + 2] ^= 0x01;
        assert!(parse_safetensors(&damaged).is_err());
    }
}
