# P093 — conversor de producto ONNX: paquete, suite del host y aceptación pública

Fecha: 2026-11-08. Segunda fase de conversores de producto real según el [diseño fijado](P093_product_converters_design_2026-11-07.md): el pipeline público emite ahora modelos ONNX protobuf completos (ir_version 8, opset 13, nodos Identity, initializers con `raw_data` verbatim) desde safetensors, con la misma disciplina de cualificación, publicación CAS y oráculo en Rust que la fase GGUF. Contrato público sin cambios de superficie.

## Implementación

- **Paquete adaptador ONNX** (`adapters/transformation/onnx/`, sub-incremento 1): conversor safetensors→ONNX protobuf real en stdlib (núcleo `onnx.py` con una única ruta de emisión `stream` compartida por preflight-hash y conversión; ledger privado, protocolo y packaging derivados del worker de referencia con los puntos Base64 sustituidos; progreso `ONNX_TENSOR` por bloque con checkpoint duradero ANTES del progreso) y oráculo focal independiente (parser protobuf propio del test que verifica estructura completa del modelo: ir 8, productor `larm-onnx-product`, opset 13, nodos Identity con inputs/outputs, initializers con dims/dtype/`raw_data` verbatim del safetensors, ValueInfoProto tipados; + negativo de dtype no copiable).
- **Suite de cualificación del host** (`OnnxTransformationSuite` + `Recipe::Onnx` en `profiles.rs`, sub-incremento 2): espejo de la suite GGUF con oráculos ONNX en Rust — parseo safetensors, emisión esperada byte a byte (`expected_onnx`) validada contra un golden generado por el escritor Python corregido (`onnx_golden.rs`), parseo estructural del modelo publicado, y todos los checks de la suite de transformación (negativos, cancel, crash, recovery, eof, replay inmutable, verificación re-lectura). 55 pruebas del crate, 4 nuevas del módulo ONNX.
- **Composición** (sub-incremento 3): `Backend::load` acepta `Recipe::Onnx`; `planner_id` por receta (`product.onnx.v1`) sellado en el paso de publicación; el estimado del plan aplica el invariante por receta (tamaño declarado por el worker cualificado con consistencia upper=final+control).
- **Aceptación pública** (sub-incremento 4): `transformation_publica_onnx_conversor_cualificado_hasta_publicacion` — sonda P026 real con la suite ONNX del host, plan/confirmación/ejecución públicas con `adapter_id` product.transformation.onnx, 351 observaciones de progreso `ONNX_TENSOR` por `GET /jobs/{id}`, publicación CAS del modelo verificado por el oráculo Rust en doble plano: igualdad byte a byte con la emisión espejo `expected_onnx` + parseo estructural contra el safetensors fuente (3 initializers, dims/dtype/`raw_data` exactos). Negativos: adapter desconocido 404 y replay de plan idempotente 201; input intacto y principales revocados.

## Defectos encontrados y corregidos durante la fase

1. **Anidamiento de dimensiones en `_value_info`** (sub-incremento 1): el shape del ValueInfoProto envolvía TODAS las dims en UNA entrada `dim` (último-gana → forma corrupta del modelo cargado); cada dimensión es una entrada Dimension independiente.
2. **`capability_profile` de la receta ONNX** (`persistent_recipe`): el match solo contemplaba `Recipe::Gguf` para `product-capabilities.json`; la ONNX caía al comodín `reference-capabilities.json`, que no existe en el paquete ONNX — la sonda P026 fallaba sin llegar a abrir la suite (sin evidencia). Mismo defecto de clase que el corregido en la fase GGUF.
3. **Negativo `target_format` sin mutación efectiva**: el oráculo ONNX usaba `json!("ONNX")`, que es la propia petición base del conversor ONNX — éxito inesperado del worker; el espejo correcto muta a `"GGUF"` (como el GGUF mutaba a `"ONNX"`).
4. **Fase de progreso con etiqueta GGUF** (`files.py convert`): el sink emitía `"GGUF_TENSOR"` en vez de `"ONNX_TENSOR"` (deriva de copia); la suite nunca veía el primer `ONNX_TENSOR` y abortaba con `onnx_completed_before_tensor_checkpoint`. La documentación interna del mismo fichero arrastraba la misma deriva ("GGUF v3 emitido").

Los defectos 2-4 son de la clase «deriva de copia derivando X desde GGUF»; los destapó la aceptación pública real, no los tests unitarios del módulo.

## Validación congelada

[attempt2](P093_runs/public-onnx-20261108/attempt2/verification-synopsis.json) — **515 pruebas PASS / 0 FAIL / 0 ignoradas, 33/33 comprobaciones** (la 33ª nueva: public-onnx), auditor independiente PASS (12.583.357 bytes del modelo publicado re-verificados estructuralmente en Python por el auditor: ir 8, productor, opset 13, nodos Identity, 3 initializers con dims; reporte retenido con counts/negativos/integridad del input). [Attempt 1](P093_runs/public-onnx-20261108/attempt1/verification-synopsis.json) conservado como FAIL a los 13 pasos: el auditor (`audit.py`) se corrigió a mitad de ejecución (el `opset_import` se leía dentro del graph en vez de a nivel ModelProto) y el control de congelación abortó limpiamente por entradas modificadas — comportamiento correcto del wrapper, sin afectar a ninguna fuente del repositorio.

## Límites conservados

El conversor copia tensores sin recuantizar (F32/F16/BF16/I64/I32/I16/I8): la cuantización real, los conversores Olive/P065, la aceptación de runtimes (llama.cpp/Ollama/Python) y la UI siguen pendientes. P093 sigue `BLOCKED_REAL`/preparación sin `CODE_PASS` integral ni `REAL_PASS`; contrato 3.13.0 intacto.
