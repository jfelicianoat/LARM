# P093 — paquete del cuantizador ONNX→ONNX-INT8 vía Olive (P065): avance

Fecha: 2026-11-13. Primera fase del [desbloqueo de P065](P093_adquisiciones_runtimes_2026-11-13.md): el paquete `product.transformation.onnx-quantize` con el runtime gestionado por ruta está construido y su ciclo completo funciona.

## Implementación

- **Empaquetado runtime por ruta** (decisión del operador): el paquete lleva el worker y el lock de dependencias (`dependencies.lock.json`), NO el runtime de 1,2 GB. El runtime sellado (`C:/Desarrollo/runtimes/python-olive-0.13.0`, 26.872 archivos con sha256) se consume por la variable `OLIVE_RUNTIME`.
- **`quantize.py`**: el script que ejecuta el runtime sellado como subproceso del worker — `workflows.run` con el pass `OnnxDynamicQuantization` (weights-only int8, sin calibración), JSON controlado de resultado, staging limpio (solo el modelo, el footprint/cache del runtime se elimina).
- **`onnxq_proto.py`**: parser protobuf stdlib del worker — valida la estructura del input (grafo MatMul con initializer F32) y del output cuantizado (nodos `DynamicQuantizeLinear` + `MatMulInteger` + `Mul` + `Cast`, initializers `w_quantized` int8, `w_scale` f32, `w_zero_point` int8).
- **`files.py`/`adapter.py`/`state.py`/`main.py`**: derivados del patrón GGUF con las sustituciones (CAPABILITY `vendor.product.onnxq`, fase `ONNXQ`, formato ONNX→ONNX, enum `["INT8"]`, errores `product_onnxq_*`); el preflight ejecuta la **misma cuantización** que la conversión a staging efímero (predicción exacta), `storage_estimate` con el formato contractual completo, `volume_key` de Windows.
- **Oráculo focal** (`tests/test_onnxq_quantization.py`, 2 pruebas): E2E plan→execute→verify con el paquete generado en vivo, parser protobuf propio del test, **dequantización independiente** del peso (initializers int8 + escala f32 con sus campos de datos reales de Olive — raw_data para el peso, float_data para la escala, int32_data para el zero_point) dentro de la cota int8, compresión verificada (menos de la mitad), provenance con el parámetro; negativos (enum inválido, campo extra, input corrupto en el op_type del nodo) fallando cerrado.

## Defectos encontrados durante la fase (todos del paquete nuevo, corregidos)

1. La jerarquía del ValueInfoProto: `type` es el **field 2** (el field 3 es `doc_string`) y el TypeProto envuelve el Tensor en `tensor_type` (field 1) — la emisión inicial los ponía al revés.
2. El nodo MatMul requiere **dos inputs** (x y w) — sin el segundo, el quantizer de ORT lanza IndexError y Olive no produce output.
3. El `ir_version` y las dims como **varint** (no int64 fijo).
4. El `GraphProto.name` es el field 2 (no el 1, que es `node`).
5. El initializer de la escala se emite con `float_data` (field 4) y el zero_point con `int32_data` (field 5) — el test extrae por campo real según el nombre.

## Segunda fase (2026-11-14): suite del host, registro, aceptación pública y congelado

Cierre del sub-incremento pendiente del verifier. **Suite del host** (`crates/larm-adapter-host/src/conformance/onnx_quantize.rs`, derivada de la ONNX con el oráculo QDQ en Rust: parseo protobuf propio de módulo — nodos y initializers con los campos de datos reales — y **espejo byte-exacto** de la cuantización con la fórmula ORT dynamic verificada empíricamente: escala `f32(max_abs/127)`, redondeo half-away con clamp ±127, zero_point 0); 2 tests unitarios del oráculo (fixture idéntico al paquete, cuantización publicada estructurada y ponderada). **Registro completo**: `Recipe::OnnxQuantize` en `profiles.rs` (receta `product.transformation.onnxq.v1`) con inyección de entorno en el WorkerSpec (OLIVE_RUNTIME + USERPROFILE/APPDATA para el scratch del runtime), `Fixture::new` con el kind `onnxq-conformance` del p025 (runtime leído de `environment.lock.json`), roots `product-onnxq-*` del manifest, planner del daemon (`product.onnxq.v1` en backend.rs y planning.rs con etiquetas propias) y test público nuevo en `reference_adapters.rs`. **Aceptación pública** (`transformation_publica_onnxq_cuantizador_olive_hasta_publicacion`): cualificación P026 real con la suite (246 casos), plan público con `quantization=INT8` (+ negativos 404/422), ejecución del runtime sellado, publicación CAS del modelo **byte-exacto al golden del runtime** con verificación QDQ independiente en el test (ops en orden, initializers, espejo de peso y escala, zero_point 0, variante con cuantización INT8). **2/2 corridas PASS** (315 s y 325 s) antes y después de retirar los diagnósticos temporales.

## Defectos encontrados en esta fase (todos corregidos)

6. **Colgue del subproceso del runtime**: la cadena de lanzadores del runtime sellado se bloquea si hereda el stdin RPC del worker (pipe abierto sin EOF) — el plan entero se colgaba; corregido con `stdin=subprocess.DEVNULL` en `_run_quantization`.
7. **Rutas extendidas `\\?\` en el scratch de ORT**: con el TEMP del worker (ruta gestionada con unicode), la inferencia de formas de ORT escribe el modelo preprocesado y `onnx.load` lo busca con `//?/` + barras normales → FileNotFoundError. Corregido dando al subproceso un scratch corto y ASCII (AppData/Local/Temp) vía USERPROFILE/APPDATA desde el perfil.
8. **Encoding no determinista del zero_point**: ORT serializa `w_zero_point` por `int32_data` (varints empaquetados) o por `raw_data` según el contexto; el parser del host solo entendía packed de 4 bytes fijos. Corregido el parser (varints no empaquetados, wire 0/5/1) y el check (agnóstico al campo y al encoding: todo-cero equivale a todos los bytes 0x00).
9. **Derivaciones de copia**: nombre de grafo `quant-fixture` vs real `larm-onnxq-fixture`, techo del output_schema 80 vs 64 MiB en el bundle, parameter_schema vacío en el describe esperado, expectativas de error `product_onnx_*` sin renombrar, negativo `target_format` con valor válido (ONNX en vez de GGUF), fase de progreso `ONNX_TENSOR` vs real `ONNXQ_TENSOR`, nombre del fichero de capabilities y convención de roots producto.
10. **Ventanas de interrupción**: el cancel/crash/eof de la suite llegaban tras el único evento de progreso (fin de la cuantización atómica) — reestructurados para aterrizar durante el subproceso (espera del job RUNNING en el ledger); el desenlace del EOF es legítimamente no determinista (CANCELLED por el server vs FAILED_RECOVERABLE por la recuperación) y el check acepta ambos con su semántica.

## Estado

- El ciclo completo plan→execute→verify del paquete funciona E2E (cuantiza el fixture determinista, 32.914 → 9.142 bytes).
- El oráculo focal pasa 2/2; los tests unitarios del oráculo Rust 2/2; clippy workspace -D warnings limpio.
- **Cualificación P026 real PASS** (246 casos), **aceptación pública 2/2 PASS**, sinopsis en `P093_runs/native-public-onnxq.json`.
- **Validación congelada PASS** (`P093_runs/public-onnxq-20261114/attempt2/`): 35 pasos ordenados (35ª comprobación congelada; attempt1 preservado FAIL por una calibración errónea del total de pruebas del auditor — la corrección se validó contra la evidencia preservada con el patrón attempt-sim), **518 pruebas / 0 fallos**, auditor independiente PASS con walk protobuf ONNXQ propio (re-deriva el peso y la escala publicados desde el input retenido, zero_point y estructura QDQ incluidos).
- Contrato vigente **3.14.0**.
