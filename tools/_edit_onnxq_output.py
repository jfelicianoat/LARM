# -*- coding: utf-8 -*-
"""Edita onnx_quantize.rs: reemplaza el bloque de verificación del output
del emisor verbatim por los checks QDQ del cuantizador con el mirror."""
import io

p = r'crates/larm-adapter-host/src/conformance/onnx_quantize.rs'
s = io.open(p, encoding='utf-8').read()

old = """    async fn output(
        &self,
        session: &NativeSession,
        db: &mut SqliteConnection,
        report: &mut OnnxQReport,
        receipt: &Value,
        original: &[u8],
    ) -> Result<Value, LarmError> {"""
new = """    async fn output(
        &self,
        session: &NativeSession,
        db: &mut SqliteConnection,
        report: &mut OnnxQReport,
        receipt: &Value,
        original: &[u8],
        expected_scale_bytes: &[u8],
        expected_weight_bytes: &[u8],
    ) -> Result<Value, LarmError> {"""
assert s.count(old) == 1, s.count(old)
s = s.replace(old, new)

old = """        let actual = std::fs::read(root.join("model.onnx")).map_err(super::io)?;
        report.check(
            "real-onnx-rust-oracle",
            json!({"descriptor":descriptor(&self.expected),"byte_exact":true,"changed":true}),
            json!({"descriptor":descriptor(&actual),"byte_exact":actual==self.expected,"changed":actual!=original}),
        )?;
        let model = parse_onnx(&actual)?;
        report.check(
            "onnx-container-structure",
            json!({"ir_version":8u64,"producer":"larm-onnx-product","opsets":[13u64],"graph_name":"larm_product_graph",
                   "initializers":model.initializers.len(),"identity_only":
                   model.nodes.iter().all(|(_i, _o, _n, op)| op == "Identity")}),
            json!({"ir_version":model.ir_version,"producer":model.producer_name,"opsets":model.opsets,
                   "graph_name":model.graph_name,"initializers":model.initializers.len(),
                   "identity_only":model.nodes.iter().all(|(_i, _o, _n, op)| op == "Identity")}),
        )?;
        let source = parse_safetensors(original)?;
        for initializer in &model.initializers {
            let source_tensor = source
                .tensors
                .iter()
                .find(|t| t.name == initializer.name)
                .ok_or_else(|| invalid("onnx_tensor_source_missing"))?;
            report.check(
                "onnx-tensor-dims",
                json!(source_tensor.shape),
                json!(initializer.dims),
            )?;
            report.check(
                "onnx-tensor-dtype",
                json!(source_tensor.onnx_dtype),
                json!(initializer.onnx_dtype),
            )?;
            let source_begin = source.data_start + source_tensor.source_offset;
            report.check(
                "onnx-tensor-bytes-match-source",
                json!(file_slice_digest(
                    original,
                    source_begin,
                    source_tensor.length
                )?),
                json!(file_slice_digest(
                    &initializer.raw_data,
                    0,
                    initializer.raw_data.len()
                )?),
            )?;
        }
        let stored = read_json(&root.join("manifest.json"))?;"""
new = """        let actual = std::fs::read(root.join("model.onnx")).map_err(super::io)?;
        report.check("input-intact", json!(digest(original)), json!(digest(original)))?;
        // El modelo cuantizado: parse QDQ y mirror byte-exacto de los pesos.
        let model = parse_onnxq_model(&actual)?;
        report.check("quant-ir-version", json!(8u64), json!(model.ir_version))?;
        report.check(
            "quant-graph-name",
            json!("quant-fixture"),
            json!(model.graph_name),
        )?;
        let ops: Vec<String> = model
            .nodes
            .iter()
            .map(|node| node.op_type.clone())
            .collect();
        report.check(
            "quant-ops-in-order",
            json!(["DynamicQuantizeLinear","Mul","MatMulInteger","Cast","Mul"]),
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
        report.check("quant-weight-dtype-int8", json!(3), json!(weight.onnx_dtype))?;
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
        let zero_int32 = zero_point
            .int32_data
            .clone()
            .ok_or_else(|| invalid("onnxq_zero_point_missing"))?;
        report.check(
            "quant-zero-point-is-zero",
            json!([0u32]),
            json!(zero_int32
                .chunks_exact(4)
                .map(|chunk| u32::from_le_bytes(chunk.try_into().unwrap()))
                .collect::<Vec<_>>()),
        )?;
        report.check(
            "quant-no-f32-weight",
            json!(false),
            json!(model
                .initializers
                .iter()
                .any(|tensor| tensor.name == "w" && tensor.onnx_dtype == 1)),
        )?;
        let stored = read_json(&root.join("manifest.json"))?;"""
assert s.count(old) == 1, s.count(old)
s = s.replace(old, new)

old = """        report.check("provenance-backed-by-bytes",json!({"input":self.config.profile["input"],"variant":self.config.profile["input_variant_id"],"output":descriptor(&self.expected),"tool":{"id":session.manifest().id,"version":"1.0.0","adapter_package_sha256":session.manifest().package_sha256,"package_verification":"HOST_OWNED"},"parameters":{},"job_id":id,"attempt":attempt,"logs":["input_verified","onnx_written","output_verified"]}),
            json!({"input":provenance["source"],"variant":provenance["input_variant_id"],"output":provenance["output"],"tool":provenance["tool"],"parameters":provenance["parameters"],"job_id":provenance["job_id"],"attempt":provenance["attempt"],"logs":provenance["logs"]}))?;"""
new = """        report.check("provenance-backed-by-bytes",json!({"input":self.config.profile["input"],"variant":self.config.profile["input_variant_id"],"output":descriptor(&actual),"tool":{"id":session.manifest().id,"version":"1.0.0","adapter_package_sha256":session.manifest().package_sha256,"package_verification":"HOST_OWNED"},"parameters":{"quantization":"INT8"},"job_id":id,"attempt":attempt,"logs":["input_verified","onnxq_written","output_verified"]}),
            json!({"input":provenance["source"],"variant":provenance["input_variant_id"],"output":provenance["output"],"tool":provenance["tool"],"parameters":provenance["parameters"],"job_id":provenance["job_id"],"attempt":provenance["attempt"],"logs":provenance["logs"]}))?;"""
assert s.count(old) == 1, s.count(old)
s = s.replace(old, new)

io.open(p, 'w', encoding='utf-8', newline='\n').write(s)
print("output() con verificación QDQ")
