//! Escáner de carpetas de modelos (P017; spec §42). Reconoce por contenido
//! y nombre, sin ejecutar ni deserializar código (un `.bin`/`.pt` de
//! PyTorch es un pickle: se marca, nunca se abre):
//!
//! - GGUF (magia `GGUF`; lee `general.architecture` y `general.name` de la
//!   cabecera con límites), con shards `-NNNNN-of-NNNNN` y `mmproj`;
//! - SafeTensors (cabecera JSON con longitud) e índices `*.index.json`;
//! - ONNX y sus `external_data`;
//! - PyTorch (zip o pickle), configuración, tokenizador, procesador,
//!   Diffusers (`model_index.json`), MLC (`mlc-chat-config.json`) y
//!   manifiestos de Ollama;
//! - CTranslate2 (`model.bin` + `config.json` + vocabulario), NCNN (pareja
//!   `.param`/`.bin`) y MNN (FlatBuffer `.mnn` y sidecars reconocidos);
//! - documentación conocida (README, LICENSE) como opcional.
//!
//! Lo demás se ignora y se informa. Cada carpeta con pesos es una variante.

use std::collections::BTreeMap;

use larm_domain::model::ArtifactRole;
use serde::Serialize;

/// Clase de un fichero.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Gguf,
    Safetensors,
    WeightIndex,
    Onnx,
    OnnxExternalData,
    PyTorchZip,
    PyTorchPickle,
    Config,
    Tokenizer,
    Processor,
    DiffusersIndex,
    MlcConfig,
    OllamaManifest,
    Ctranslate2Weights,
    Ctranslate2Vocabulary,
    NcnnParam,
    NcnnWeights,
    MnnModel,
    MnnExternalWeights,
    MnnMetadata,
    MnnEmbedding,
    Documentation,
}

/// Un fichero reconocido.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Detected {
    pub relative: String,
    pub kind: Kind,
    pub role: ArtifactRole,
    pub required: bool,
    /// Datos leídos de la cabecera (arquitectura, nombre…) sin interpretar más.
    pub facts: BTreeMap<String, String>,
}

/// Un fichero que no se reconoce: se ignora y se informa.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Ignored {
    pub relative: String,
    pub reason: String,
}

fn ext(name: &str) -> String {
    name.rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default()
}

fn file_name(relative: &str) -> &str {
    relative.rsplit('/').next().unwrap_or(relative)
}

/// Lector acotado de la cabecera GGUF.
struct Cursor<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Cursor<'_> {
    fn take(&mut self, n: usize) -> Option<&[u8]> {
        let end = self.at.checked_add(n)?;
        let s = self.bytes.get(self.at..end)?;
        self.at = end;
        Some(s)
    }
    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }
    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }
    fn string(&mut self) -> Option<String> {
        let len = usize::try_from(self.u64()?).ok()?;
        if len > 4096 {
            return None;
        }
        String::from_utf8(self.take(len)?.to_vec()).ok()
    }
    /// Salta un valor GGUF del tipo dado (arrays incluidos, con límites).
    fn skip(&mut self, ty: u32, depth: u8) -> Option<()> {
        let fixed = match ty {
            0 | 1 | 7 => 1,
            2 | 3 => 2,
            4..=6 => 4,
            10..=12 => 8,
            _ => 0,
        };
        if fixed > 0 {
            return self.take(fixed).map(|_| ());
        }
        match ty {
            8 => self.string().map(|_| ()),
            9 if depth < 2 => {
                let inner = self.u32()?;
                let n = self.u64()?;
                if n > 1_000_000 {
                    return None;
                }
                for _ in 0..n {
                    self.skip(inner, depth + 1)?;
                }
                Some(())
            }
            _ => None,
        }
    }
}

/// Arquitectura y nombre de la cabecera GGUF (`None` si la cabecera no
/// cabe en los bytes leídos o no es válida).
#[must_use]
pub fn gguf_facts(head: &[u8]) -> Option<BTreeMap<String, String>> {
    let mut c = Cursor { bytes: head, at: 0 };
    if c.take(4)? != b"GGUF" {
        return None;
    }
    let version = c.u32()?;
    if !(2..=3).contains(&version) {
        return None;
    }
    let _tensors = c.u64()?;
    let kv = c.u64()?;
    let mut facts = BTreeMap::new();
    facts.insert("gguf_version".into(), version.to_string());
    for _ in 0..kv.min(512) {
        let Some(key) = c.string() else { break };
        let Some(ty) = c.u32() else { break };
        if ty == 8
            && matches!(
                key.as_str(),
                "general.architecture" | "general.name" | "general.basename"
            )
        {
            let Some(value) = c.string() else { break };
            facts.insert(key.trim_start_matches("general.").to_string(), value);
        } else if c.skip(ty, 0).is_none() {
            break;
        }
        if facts.contains_key("architecture") && facts.contains_key("name") {
            break;
        }
    }
    Some(facts)
}

fn safetensors_header(head: &[u8], size: u64) -> bool {
    let Some(len) = head
        .get(..8)
        .and_then(|b| b.try_into().ok())
        .map(u64::from_le_bytes)
    else {
        return false;
    };
    len >= 2 && len.saturating_add(8) <= size && head.get(8) == Some(&b'{')
}

fn json_has(head: &[u8], key: &str) -> bool {
    serde_json::from_slice::<serde_json::Value>(head)
        .ok()
        .and_then(|v| v.as_object().map(|o| o.contains_key(key)))
        .unwrap_or(false)
}

fn json_object(head: &[u8]) -> bool {
    serde_json::from_slice::<serde_json::Value>(head)
        .ok()
        .is_some_and(|value| value.is_object())
}

fn directory(relative: &str) -> &str {
    relative.rsplit_once('/').map_or("", |(dir, _)| dir)
}

type ScanCandidate<'a> = (&'a str, &'a [u8], u64);

fn join_relative(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.to_string()
    } else {
        format!("{dir}/{name}")
    }
}

fn is_ct2_vocabulary(relative: &str, size: u64) -> bool {
    let lower = relative.to_ascii_lowercase();
    let name = file_name(&lower);
    size > 0
        && name.contains("vocab")
        && [".json", ".txt", ".model", ".spm", ".vocab"]
            .iter()
            .any(|suffix| name.ends_with(suffix))
}

fn ct2_binary(head: &[u8], size: u64) -> bool {
    let Some(version) = head
        .get(..4)
        .and_then(|bytes| bytes.try_into().ok())
        .map(u32::from_le_bytes)
    else {
        return false;
    };
    // CTranslate2 4.8 serializes versions 1 through 6. Keep the detector
    // bounded to the published wire versions instead of accepting any .bin.
    if !(1..=6).contains(&version) || size < 8 {
        return false;
    }
    let mut at = 4usize;
    if version >= 2 {
        let Some(length) = head
            .get(at..at + 2)
            .and_then(|bytes| bytes.try_into().ok())
            .map(u16::from_le_bytes)
            .map(usize::from)
        else {
            return false;
        };
        at += 2;
        let Some(spec) = head.get(at..at.saturating_add(length)) else {
            return false;
        };
        if length < 2
            || spec.last() != Some(&0)
            || std::str::from_utf8(&spec[..length - 1]).is_err()
        {
            return false;
        }
        at = at.saturating_add(length);
        let Some(revision) = head
            .get(at..at.saturating_add(4))
            .and_then(|bytes| bytes.try_into().ok())
            .map(u32::from_le_bytes)
        else {
            return false;
        };
        if revision == 0 {
            return false;
        }
        at = at.saturating_add(4);
    }
    head.get(at..at.saturating_add(4))
        .and_then(|bytes| bytes.try_into().ok())
        .map(u32::from_le_bytes)
        .is_some_and(|variables| variables > 0)
}

fn ncnn_param(head: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(head) else {
        return false;
    };
    let mut lines = text.lines();
    if lines.next().map(str::trim) != Some("7767517") {
        return false;
    }
    let Some(counts) = lines.next() else {
        return false;
    };
    let mut counts = counts.split_whitespace();
    let (Some(layers), Some(blobs)) = (
        counts.next().and_then(|part| part.parse::<u32>().ok()),
        counts.next().and_then(|part| part.parse::<u32>().ok()),
    ) else {
        return false;
    };
    layers > 0 && blobs >= layers && counts.next().is_none()
}

fn mnn_flatbuffer(head: &[u8], size: u64) -> bool {
    let read_u16 = |at: usize| {
        head.get(at..at.saturating_add(2))
            .and_then(|bytes| bytes.try_into().ok())
            .map(u16::from_le_bytes)
    };
    let Some(root) = head
        .get(..4)
        .and_then(|bytes| bytes.try_into().ok())
        .map(u32::from_le_bytes)
        .and_then(|offset| usize::try_from(offset).ok())
    else {
        return false;
    };
    if root < 4 || root.saturating_add(4) > head.len() || root.saturating_add(4) as u64 > size {
        return false;
    }
    let Some(vtable_distance) = head
        .get(root..root.saturating_add(4))
        .and_then(|bytes| bytes.try_into().ok())
        .map(i32::from_le_bytes)
    else {
        return false;
    };
    let Some(vtable) = i64::try_from(root)
        .ok()
        .and_then(|root| root.checked_sub(i64::from(vtable_distance)))
        .and_then(|offset| usize::try_from(offset).ok())
    else {
        return false;
    };
    let (Some(vtable_size), Some(object_size)) =
        (read_u16(vtable), read_u16(vtable.saturating_add(2)))
    else {
        return false;
    };
    let vtable_end = vtable.saturating_add(usize::from(vtable_size));
    let object_end = root.saturating_add(usize::from(object_size));
    vtable_size >= 4
        && object_size >= 4
        && vtable_end <= head.len()
        && object_end <= head.len()
        && object_end as u64 <= size
}

/// Clasifica un directorio completo para reconocer formatos cuyo tipo depende
/// de sus sidecars. Solo consulta nombres y cabeceras ya limitadas por el walker.
pub fn detect_files(files: &[(&str, &[u8], u64)]) -> (Vec<Detected>, Vec<Ignored>) {
    let mut by_dir: BTreeMap<String, Vec<ScanCandidate<'_>>> = BTreeMap::new();
    for &(relative, head, size) in files {
        by_dir
            .entry(directory(relative).to_string())
            .or_default()
            .push((relative, head, size));
    }
    let mut detected = Vec::new();
    let mut ignored = Vec::new();
    for (_, mut entries) in by_dir {
        entries.sort_by(|a, b| a.0.cmp(b.0));
        let mut dir_detected = BTreeMap::<String, Detected>::new();
        let mut dir_ignored = BTreeMap::<String, Ignored>::new();
        for &(relative, head, size) in &entries {
            match detect(relative, head, size) {
                Ok(item) => {
                    dir_detected.insert(relative.to_string(), item);
                }
                Err(item) => {
                    dir_ignored.insert(relative.to_string(), item);
                }
            }
        }
        let lower_path = |relative: &str| relative.to_ascii_lowercase();
        let entry_by_lower: BTreeMap<String, (&str, &[u8], u64)> = entries
            .iter()
            .map(|&(relative, head, size)| (lower_path(relative), (relative, head, size)))
            .collect();
        let dir_lower = |relative: &str| directory(&lower_path(relative)).to_string();

        let has_ct2_vocabulary = entries
            .iter()
            .any(|(relative, _, size)| is_ct2_vocabulary(relative, *size));
        let ct2_model = entries.iter().find(|(relative, head, size)| {
            file_name(&relative.to_ascii_lowercase()) == "model.bin"
                && ct2_binary(head, *size)
                && entry_by_lower
                    .get(&join_relative(&dir_lower(relative), "config.json"))
                    .is_some_and(|(_, config, _)| json_object(config))
                && has_ct2_vocabulary
        });
        if let Some((relative, _, _)) = ct2_model {
            let relative = *relative;
            dir_ignored.remove(relative);
            dir_detected.insert(
                relative.to_string(),
                Detected {
                    relative: relative.to_string(),
                    kind: Kind::Ctranslate2Weights,
                    role: ArtifactRole::Weights,
                    required: true,
                    facts: BTreeMap::new(),
                },
            );
            for &(vocab_path, _, size) in &entries {
                if is_ct2_vocabulary(vocab_path, size) {
                    dir_ignored.remove(vocab_path);
                    dir_detected.insert(
                        vocab_path.to_string(),
                        Detected {
                            relative: vocab_path.to_string(),
                            kind: Kind::Ctranslate2Vocabulary,
                            role: ArtifactRole::Tokenizer,
                            required: true,
                            facts: BTreeMap::new(),
                        },
                    );
                }
            }
        }

        let ncnn_params: Vec<_> = entries
            .iter()
            .filter(|(relative, head, _)| {
                file_name(&relative.to_ascii_lowercase()).ends_with(".param") && ncnn_param(head)
            })
            .map(|&(relative, head, size)| (relative, head, size))
            .collect();
        for (param_path, _, _) in &ncnn_params {
            let param_lower = param_path.to_ascii_lowercase();
            let expected_bin = format!("{}.bin", param_lower.trim_end_matches(".param"));
            if let Some((bin_path, _, bin_size)) = entry_by_lower.get(&expected_bin) {
                if *bin_size > 0 {
                    dir_ignored.remove(*param_path);
                    dir_ignored.remove(*bin_path);
                    dir_detected.insert(
                        param_path.to_string(),
                        Detected {
                            relative: param_path.to_string(),
                            kind: Kind::NcnnParam,
                            role: ArtifactRole::Config,
                            required: true,
                            facts: BTreeMap::new(),
                        },
                    );
                    dir_detected.insert(
                        (*bin_path).to_string(),
                        Detected {
                            relative: (*bin_path).to_string(),
                            kind: Kind::NcnnWeights,
                            role: ArtifactRole::Weights,
                            required: true,
                            facts: BTreeMap::new(),
                        },
                    );
                }
            } else {
                dir_ignored.insert(
                    param_path.to_string(),
                    Ignored {
                        relative: param_path.to_string(),
                        reason: "ncnn_companion_missing".into(),
                    },
                );
            }
        }

        let mnn_models: Vec<_> = entries
            .iter()
            .filter(|(relative, head, size)| {
                file_name(&relative.to_ascii_lowercase()).ends_with(".mnn")
                    && mnn_flatbuffer(head, *size)
            })
            .map(|&(relative, head, size)| (relative, head, size))
            .collect();
        if !mnn_models.is_empty() {
            for (model_path, _, _) in &mnn_models {
                dir_ignored.remove(*model_path);
                dir_detected.insert(
                    model_path.to_string(),
                    Detected {
                        relative: model_path.to_string(),
                        kind: Kind::MnnModel,
                        role: ArtifactRole::Weights,
                        required: true,
                        facts: BTreeMap::new(),
                    },
                );
                for (suffix, kind, role, required) in [
                    (
                        ".weight",
                        Kind::MnnExternalWeights,
                        ArtifactRole::Weights,
                        true,
                    ),
                    (".json", Kind::MnnMetadata, ArtifactRole::Config, false),
                ] {
                    let sidecar = format!("{}{}", model_path.to_ascii_lowercase(), suffix);
                    if let Some((sidecar_path, _, size)) = entry_by_lower.get(&sidecar) {
                        if *size > 0 {
                            dir_ignored.remove(*sidecar_path);
                            dir_detected.insert(
                                (*sidecar_path).to_string(),
                                Detected {
                                    relative: (*sidecar_path).to_string(),
                                    kind,
                                    role,
                                    required,
                                    facts: BTreeMap::new(),
                                },
                            );
                        }
                    }
                }
            }
            for &(relative, _, size) in &entries {
                let lower = relative.to_ascii_lowercase();
                let name = file_name(&lower);
                let is_mnn_config = name == "llm_config.json";
                let is_mnn_tokenizer = matches!(name, "tokenizer.txt" | "tokenizer.mtok");
                let is_embedding = name.starts_with("embeddings_") && name.ends_with(".bin");
                if size > 0 && (is_mnn_config || is_mnn_tokenizer || is_embedding) {
                    dir_ignored.remove(relative);
                    dir_detected.insert(
                        relative.to_string(),
                        Detected {
                            relative: relative.to_string(),
                            kind: if is_mnn_config {
                                Kind::MnnMetadata
                            } else if is_mnn_tokenizer {
                                Kind::Tokenizer
                            } else {
                                Kind::MnnEmbedding
                            },
                            role: if is_mnn_config {
                                ArtifactRole::Config
                            } else if is_mnn_tokenizer {
                                ArtifactRole::Tokenizer
                            } else {
                                ArtifactRole::Weights
                            },
                            required: true,
                            facts: BTreeMap::new(),
                        },
                    );
                }
            }
        }
        detected.extend(dir_detected.into_values());
        ignored.extend(dir_ignored.into_values());
    }
    detected.sort_by(|a, b| a.relative.cmp(&b.relative));
    ignored.sort_by(|a, b| a.relative.cmp(&b.relative));
    (detected, ignored)
}

/// Clasifica un fichero por su nombre y su cabecera.
pub fn detect(relative: &str, head: &[u8], size: u64) -> Result<Detected, Ignored> {
    let name = file_name(relative).to_ascii_lowercase();
    let e = ext(&name);
    let found =
        |kind: Kind, role: ArtifactRole, required: bool, facts: BTreeMap<String, String>| {
            Ok(Detected {
                relative: relative.to_string(),
                kind,
                role,
                required,
                facts,
            })
        };
    let ignored = |reason: &str| {
        Err(Ignored {
            relative: relative.to_string(),
            reason: reason.to_string(),
        })
    };
    if head.starts_with(b"GGUF") {
        let facts = gguf_facts(head).unwrap_or_default();
        let role = if name.contains("mmproj") {
            ArtifactRole::Mmproj
        } else {
            ArtifactRole::Weights
        };
        return found(Kind::Gguf, role, true, facts);
    }
    if e == "gguf" {
        return ignored("gguf_magic_missing");
    }
    if e == "safetensors" {
        return if safetensors_header(head, size) {
            found(
                Kind::Safetensors,
                ArtifactRole::Weights,
                true,
                BTreeMap::new(),
            )
        } else {
            ignored("safetensors_header_invalid")
        };
    }
    if name.ends_with(".index.json") && json_has(head, "weight_map") {
        return found(
            Kind::WeightIndex,
            ArtifactRole::Config,
            true,
            BTreeMap::new(),
        );
    }
    if e == "onnx" {
        // Protobuf ModelProto: el primer campo suele ser ir_version (tag 0x08).
        return if head.first() == Some(&0x08) {
            found(Kind::Onnx, ArtifactRole::Weights, true, BTreeMap::new())
        } else {
            ignored("onnx_header_invalid")
        };
    }
    if name.ends_with(".onnx.data") || name.ends_with(".onnx_data") {
        return found(
            Kind::OnnxExternalData,
            ArtifactRole::ExternalData,
            true,
            BTreeMap::new(),
        );
    }
    if matches!(e.as_str(), "bin" | "pt" | "pth") {
        if head.starts_with(b"PK\x03\x04") {
            return found(
                Kind::PyTorchZip,
                ArtifactRole::Weights,
                true,
                BTreeMap::new(),
            );
        }
        if head.first() == Some(&0x80) {
            let mut facts = BTreeMap::new();
            facts.insert("unsafe_pickle".into(), "true".into());
            return found(Kind::PyTorchPickle, ArtifactRole::Weights, true, facts);
        }
        return ignored("unknown_binary");
    }
    match name.as_str() {
        "config.json" | "generation_config.json" => {
            let mut facts = BTreeMap::new();
            if let Ok(v) = serde_json::from_slice::<serde_json::Value>(head) {
                if let Some(t) = v.get("model_type").and_then(serde_json::Value::as_str) {
                    facts.insert("architecture".into(), t.to_string());
                }
            }
            return found(
                Kind::Config,
                ArtifactRole::Config,
                name == "config.json",
                facts,
            );
        }
        "tokenizer.json"
        | "tokenizer.model"
        | "tokenizer_config.json"
        | "special_tokens_map.json"
        | "vocab.json"
        | "merges.txt"
        | "vocab.txt"
        | "spiece.model" => {
            let required = matches!(name.as_str(), "tokenizer.json" | "tokenizer.model");
            return found(
                Kind::Tokenizer,
                ArtifactRole::Tokenizer,
                required,
                BTreeMap::new(),
            );
        }
        "preprocessor_config.json" | "processor_config.json" => {
            return found(
                Kind::Processor,
                ArtifactRole::Processor,
                true,
                BTreeMap::new(),
            );
        }
        "model_index.json" => {
            return found(
                Kind::DiffusersIndex,
                ArtifactRole::Config,
                true,
                BTreeMap::new(),
            )
        }
        "mlc-chat-config.json" => {
            return found(Kind::MlcConfig, ArtifactRole::Config, true, BTreeMap::new())
        }
        _ => {}
    }
    if relative.contains("manifests/") && json_has(head, "layers") {
        return found(
            Kind::OllamaManifest,
            ArtifactRole::Config,
            true,
            BTreeMap::new(),
        );
    }
    if name.starts_with("readme") || name.starts_with("license") || e == "md" {
        return found(
            Kind::Documentation,
            ArtifactRole::Other,
            false,
            BTreeMap::new(),
        );
    }
    ignored("unknown_format")
}

/// Variante encontrada en una carpeta.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ScannedVariant {
    /// Carpeta (POSIX, relativa a la raíz escaneada; vacía = la raíz).
    pub dir: String,
    pub name: String,
    pub format: String,
    pub quantization: Option<String>,
    pub architecture: Option<String>,
    pub files: Vec<Detected>,
}

/// Cuantizaciones reconocibles en nombres de fichero GGUF.
const QUANTS: [&str; 18] = [
    "IQ1_S", "IQ2_XXS", "IQ2_XS", "IQ3_XXS", "IQ4_XS", "IQ4_NL", "Q2_K", "Q3_K_M", "Q3_K_S",
    "Q4_0", "Q4_K_M", "Q4_K_S", "Q5_K_M", "Q5_K_S", "Q6_K", "Q8_0", "F16", "BF16",
];

fn quantization(name: &str) -> Option<String> {
    let upper = name.to_ascii_uppercase();
    QUANTS
        .iter()
        .filter(|q| upper.contains(*q))
        .max_by_key(|q| q.len())
        .map(|q| (*q).to_string())
}

/// Base de un shard GGUF: `x-00001-of-00002.gguf` → `x`.
fn shard_base(name: &str) -> String {
    let stem = name.trim_end_matches(".gguf").trim_end_matches(".GGUF");
    match stem.rsplit_once("-of-") {
        Some((left, total)) if total.chars().all(|c| c.is_ascii_digit()) => left
            .rsplit_once('-')
            .filter(|(_, n)| n.chars().all(|c| c.is_ascii_digit()))
            .map_or(stem, |(base, _)| base)
            .to_string(),
        _ => stem.to_string(),
    }
}

/// Agrupa lo reconocido en variantes, carpeta a carpeta: cada grupo de
/// shards GGUF es una variante (el mmproj y la documentación de su carpeta
/// la acompañan); el resto de formatos forman una variante por carpeta.
#[must_use]
pub fn group(detected: &[Detected]) -> Vec<ScannedVariant> {
    let mut by_dir: BTreeMap<String, Vec<&Detected>> = BTreeMap::new();
    for d in detected {
        let dir = d
            .relative
            .rsplit_once('/')
            .map_or(String::new(), |(d, _)| d.to_string());
        by_dir.entry(dir).or_default().push(d);
    }
    let mut out = Vec::new();
    for (dir, files) in by_dir {
        let folder = dir.rsplit('/').next().unwrap_or("").to_string();
        let ggufs: Vec<&&Detected> = files
            .iter()
            .filter(|d| d.kind == Kind::Gguf && d.role == ArtifactRole::Weights)
            .collect();
        let extras: Vec<Detected> = files
            .iter()
            .filter(|d| {
                (d.kind == Kind::Gguf && d.role == ArtifactRole::Mmproj)
                    || d.kind == Kind::Documentation
            })
            .map(|d| (*d).clone())
            .collect();
        let mut gguf_groups: BTreeMap<String, Vec<Detected>> = BTreeMap::new();
        for g in &ggufs {
            gguf_groups
                .entry(shard_base(file_name(&g.relative)))
                .or_default()
                .push((**g).clone());
        }
        for (base, mut shards) in gguf_groups {
            let architecture = shards
                .iter()
                .find_map(|s| s.facts.get("architecture").cloned());
            let name = shards
                .iter()
                .find_map(|s| s.facts.get("name").cloned())
                .unwrap_or_else(|| base.clone());
            shards.extend(extras.iter().cloned());
            shards.sort_by(|a, b| a.relative.cmp(&b.relative));
            out.push(ScannedVariant {
                dir: dir.clone(),
                name,
                format: "GGUF".into(),
                quantization: quantization(&base),
                architecture,
                files: shards,
            });
        }
        let rest: Vec<Detected> = files
            .iter()
            .filter(|d| d.kind != Kind::Gguf)
            .map(|d| (*d).clone())
            .collect();
        let has = |k: Kind| rest.iter().any(|d| d.kind == k);
        let format = if has(Kind::MnnModel) {
            Some("MNN")
        } else if has(Kind::NcnnParam) {
            Some("NCNN")
        } else if has(Kind::Ctranslate2Weights) {
            Some("CTranslate2")
        } else if has(Kind::DiffusersIndex) {
            Some("Diffusers")
        } else if has(Kind::Safetensors) {
            Some("SafeTensors")
        } else if has(Kind::Onnx) {
            Some("ONNX")
        } else if has(Kind::PyTorchZip) || has(Kind::PyTorchPickle) {
            Some("PyTorch")
        } else if has(Kind::MlcConfig) {
            Some("MLC")
        } else if has(Kind::OllamaManifest) {
            Some("Ollama")
        } else {
            None
        };
        if let Some(format) = format {
            // Si ya hubo variantes GGUF en la carpeta, la documentación va con ellas.
            let files: Vec<Detected> = if gguf_groups_empty(&ggufs) {
                rest
            } else {
                rest.into_iter()
                    .filter(|d| d.kind != Kind::Documentation)
                    .collect()
            };
            let architecture = files
                .iter()
                .find_map(|d| d.facts.get("architecture").cloned());
            out.push(ScannedVariant {
                dir: dir.clone(),
                name: if folder.is_empty() {
                    "modelo".into()
                } else {
                    folder.clone()
                },
                format: format.into(),
                quantization: None,
                architecture,
                files,
            });
        }
    }
    out
}

fn gguf_groups_empty(ggufs: &[&&Detected]) -> bool {
    ggufs.is_empty()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    /// Cabecera GGUF v3 mínima con claves de texto y un array que saltar.
    pub fn gguf_head(arch: &str, name: &str) -> Vec<u8> {
        let mut b = b"GGUF".to_vec();
        b.extend(3u32.to_le_bytes());
        b.extend(0u64.to_le_bytes());
        b.extend(3u64.to_le_bytes());
        let s = |b: &mut Vec<u8>, v: &str| {
            b.extend((v.len() as u64).to_le_bytes());
            b.extend(v.as_bytes());
        };
        s(&mut b, "tokenizer.ggml.scores");
        b.extend(9u32.to_le_bytes());
        b.extend(6u32.to_le_bytes());
        b.extend(2u64.to_le_bytes());
        b.extend(1.0f32.to_le_bytes());
        b.extend(2.0f32.to_le_bytes());
        s(&mut b, "general.architecture");
        b.extend(8u32.to_le_bytes());
        s(&mut b, arch);
        s(&mut b, "general.name");
        b.extend(8u32.to_le_bytes());
        s(&mut b, name);
        b
    }

    #[test]
    fn cabecera_gguf_con_limites() {
        let facts = gguf_facts(&gguf_head("qwen2", "Qwen X")).unwrap();
        assert_eq!(facts["architecture"], "qwen2");
        assert_eq!(facts["name"], "Qwen X");
        // Truncada: lo que se pudo leer, sin pánico.
        let head = gguf_head("qwen2", "Qwen X");
        assert!(gguf_facts(&head[..30]).is_some());
        assert!(
            gguf_facts(b"GGUF\x09\x00\x00\x00").is_none(),
            "versión desconocida"
        );
        assert!(gguf_facts(b"NOPE").is_none());
    }

    #[test]
    fn deteccion_por_contenido() {
        let st = {
            let mut b = 12u64.to_le_bytes().to_vec();
            b.extend(b"{\"a\":{}}    ");
            b
        };
        let ok = |rel: &str, head: &[u8]| detect(rel, head, 1_000).unwrap();
        assert_eq!(ok("m/x.gguf", &gguf_head("llama", "x")).kind, Kind::Gguf);
        assert_eq!(
            ok("m/mmproj-f16.gguf", &gguf_head("clip", "p")).role,
            ArtifactRole::Mmproj
        );
        assert_eq!(ok("m/w.safetensors", &st).kind, Kind::Safetensors);
        assert_eq!(
            ok("m/model.safetensors.index.json", br#"{"weight_map":{}}"#).kind,
            Kind::WeightIndex
        );
        assert_eq!(ok("m/model.onnx", &[0x08, 0x07]).kind, Kind::Onnx);
        assert_eq!(
            ok("m/model.onnx.data", b"xx").role,
            ArtifactRole::ExternalData
        );
        let pickle = ok("m/pytorch_model.bin", &[0x80, 0x02]);
        assert_eq!(
            (pickle.kind, pickle.facts["unsafe_pickle"].as_str()),
            (Kind::PyTorchPickle, "true")
        );
        assert_eq!(ok("m/model.pt", b"PK\x03\x04").kind, Kind::PyTorchZip);
        assert_eq!(
            ok("m/config.json", br#"{"model_type":"qwen2"}"#).facts["architecture"],
            "qwen2"
        );
        assert!(ok("m/tokenizer.json", b"{}").required);
        assert!(!ok("m/tokenizer_config.json", b"{}").required);
        assert_eq!(ok("m/README.md", b"#").role, ArtifactRole::Other);
        let no = |rel: &str, head: &[u8]| detect(rel, head, 1_000).unwrap_err().reason;
        assert_eq!(no("m/fake.gguf", b"NOPE"), "gguf_magic_missing");
        assert_eq!(
            no("m/bad.safetensors", b"\xff\xff\xff\xff\xff\xff\xff\xff{"),
            "safetensors_header_invalid"
        );
        assert_eq!(no("m/setup.exe", b"MZ"), "unknown_format");
        assert_eq!(no("m/data.bin", b"zzz"), "unknown_binary");
        assert_eq!(no("m/notas.txt", b"hola"), "unknown_format");
    }

    #[test]
    fn detecta_bundles_ct2_ncnn_y_mnn_con_contexto_de_carpeta() {
        let mut ct2 = 6u32.to_le_bytes().to_vec();
        ct2.extend(12u16.to_le_bytes());
        ct2.extend(b"transformer\0");
        ct2.extend(1u32.to_le_bytes());
        ct2.extend(1u32.to_le_bytes());

        let ncnn_param = b"7767517\n1 1\nInput input 0 1 data\n";
        let ct2_config = br#"{"model_type":"nllb"}"#;
        let mnn_model = [
            12, 0, 0, 0, // root table at byte 12
            6, 0, 4, 0, 0, 0, // vtable with one field
            0, 0, // alignment padding
            8, 0, 0, 0, // vtable distance
        ];
        assert!(mnn_flatbuffer(&mnn_model, mnn_model.len() as u64));
        let files: Vec<(&str, &[u8], u64)> = vec![
            ("ct2/config.json", ct2_config, ct2_config.len() as u64),
            ("ct2/model.bin", &ct2, ct2.len() as u64),
            ("ct2/source_vocabulary.json", b"{}", 2),
            ("ncnn/net.param", ncnn_param, ncnn_param.len() as u64),
            ("ncnn/net.bin", b"raw ncnn weights", 16),
            ("mnn/config.json", b"{}", 2),
            ("mnn/llm.mnn", &mnn_model, mnn_model.len() as u64),
            ("mnn/llm.mnn.weight", b"external weights", 16),
            ("mnn/llm_config.json", b"{}", 2),
            ("mnn/tokenizer.txt", b"tokens", 6),
            ("mnn/embeddings_bf16.bin", b"embeddings", 10),
        ];
        let (detected, ignored) = detect_files(&files);
        assert!(ignored.is_empty(), "{ignored:?}");
        let variants = group(&detected);
        assert_eq!(variants.len(), 3);
        for (format, dir) in [("CTranslate2", "ct2"), ("NCNN", "ncnn"), ("MNN", "mnn")] {
            let variant = variants.iter().find(|v| v.dir == dir).unwrap();
            assert_eq!(variant.format, format);
            assert!(!variant.files.is_empty());
        }
        let ct2 = variants.iter().find(|v| v.format == "CTranslate2").unwrap();
        assert_eq!(ct2.files.len(), 3);
        let ncnn = variants.iter().find(|v| v.format == "NCNN").unwrap();
        assert_eq!(ncnn.files.len(), 2);
        let mnn = variants.iter().find(|v| v.format == "MNN").unwrap();
        assert_eq!(mnn.files.len(), 6);
    }

    #[test]
    fn los_bins_sueltos_y_bundles_incompletos_no_se_importan_como_modelo() {
        let ncnn_param = b"7767517\n1 1\nInput input 0 1 data\n";
        let malformed_param = b"7767517\n1 invalid 1\n";
        let files = [
            (
                "orphan/model.param",
                ncnn_param.as_slice(),
                ncnn_param.len() as u64,
            ),
            ("random/data.bin", b"arbitrary".as_slice(), 9),
            ("ct2/model.bin", b"not CTranslate2".as_slice(), 16),
            ("mnn/model.mnn", b"not a FlatBuffer".as_slice(), 16),
            (
                "bad-ncnn/model.param",
                malformed_param.as_slice(),
                malformed_param.len() as u64,
            ),
            ("bad-ncnn/model.bin", b"weights".as_slice(), 7),
        ];
        let (detected, ignored) = detect_files(&files);
        assert!(group(&detected).is_empty());
        assert_eq!(ignored.len(), files.len());
        assert!(ignored.iter().any(|item| {
            item.relative == "bad-ncnn/model.param" && item.reason == "unknown_format"
        }));
        assert_eq!(
            ignored
                .iter()
                .find(|item| item.relative == "orphan/model.param")
                .unwrap()
                .reason,
            "ncnn_companion_missing"
        );
        assert!(ignored
            .iter()
            .any(|item| item.relative.ends_with("data.bin") && item.reason == "unknown_binary"));
    }

    #[test]
    fn agrupacion_en_variantes() {
        let d = |rel: &str, head: &[u8]| detect(rel, head, 1_000).unwrap();
        let st = {
            let mut b = 12u64.to_le_bytes().to_vec();
            b.extend(b"{\"a\":{}}    ");
            b
        };
        let files = vec![
            d(
                "qwen/qwen-x-Q4_K_M-00001-of-00002.gguf",
                &gguf_head("qwen2", "Qwen X"),
            ),
            d("qwen/qwen-x-Q4_K_M-00002-of-00002.gguf", b"GGUF"),
            d("qwen/qwen-x-Q8_0.gguf", &gguf_head("qwen2", "Qwen X")),
            d("qwen/mmproj-qwen-x-f16.gguf", &gguf_head("clip", "proj")),
            d("qwen/README.md", b"#"),
            d("st/model-00001-of-00002.safetensors", &st),
            d("st/model-00002-of-00002.safetensors", &st),
            d("st/config.json", br#"{"model_type":"llama"}"#),
            d("st/tokenizer.json", b"{}"),
        ];
        let v = group(&files);
        assert_eq!(v.len(), 3);
        let q4 = v
            .iter()
            .find(|x| x.quantization.as_deref() == Some("Q4_K_M"))
            .unwrap();
        assert_eq!(q4.files.len(), 4, "2 shards + mmproj + README");
        assert_eq!(
            (q4.architecture.as_deref(), q4.name.as_str()),
            (Some("qwen2"), "Qwen X")
        );
        let q8 = v
            .iter()
            .find(|x| x.quantization.as_deref() == Some("Q8_0"))
            .unwrap();
        assert_eq!(q8.files.len(), 3, "su fichero + mmproj + README");
        let st = v.iter().find(|x| x.format == "SafeTensors").unwrap();
        assert_eq!(
            (st.name.as_str(), st.architecture.as_deref(), st.files.len()),
            ("st", Some("llama"), 4)
        );
        assert_eq!(group(&files), v, "determinista");
    }
}
