# P093 — paquete del cuantizador ONNX→ONNX-INT8 vía Olive (P065): diseño

Fecha: 2026-11-13. Con el runtime gestionado `python-olive` instalado y sellado ([adquisición](P093_adquisiciones_runtimes_2026-11-13.md)), esta fase implementa el **paquete del cuantizador de producto**: adaptador de transformación `product.transformation.onnx-quantize` que cuantiza un modelo ONNX de entrada a ONNX con pesos int8 (cuantización **dinámica** weights-only vía el pass `OnnxDynamicQuantization` de Olive — sin calibración de activaciones, aplicable a un LLM sin dataset).

## Modelo de empaquetado (decisión del operador: runtime por ruta)

- El paquete sigue siendo **self-contained en su lógica**: lleva worker, ledger, contratos y SDK como los demás.
- El **runtime** (1,2 GB, 26.872 archivos, sellado con 26.872 sha256) NO se empaqueta: vive en la ruta gestionada `C:/Desarrollo/runtimes/python-olive-0.13.0` y el paquete lo declara como dependencia (lock de 67 versiones).
- **Cable**: el manifest del cuantizador declara un root nominal de lectura `product-olive-runtime`; la cualificación mapea ese root a la ruta sellada real (mismo mecanismo que `gguf_roots`); el WorkSpec pasa la variable `OLIVE_RUNTIME` (intérprete del runtime) al worker.
- El worker ejecuta la cuantización como **subproceso del runtime sellado**: `{OLIVE_RUNTIME}/Scripts/python.exe {package}/transformation/quantize.py entrada.onnx salida.onnx` — el paquete aporta la LÓGICA (script de cuantización determinista con el pass Olive), el runtime aporta las LIBRERÍAS (torch/olive/onnx sellados).

## Flujo del worker (derivado del patrón GGUF/ONNX)

- **Entrada**: variante ONNX (fixture `model.onnx`), parámetro `quantization: "INT8"` (enum único del cuantizador; absent → rechazo `product_onnxq_options_not_supported`).
- **describe**: formatos `["ONNX"]` → `["ONNX"]`, `parameter_schema` con `quantization: enum ["INT8"]`, tool_version 1.0.0.
- **Preflight** (misma ruta de emisión, sin publicar): valida el input (parseo protobuf de estructura ONNX), ejecuta la cuantización al staging efímero del work y fija hash/tamaño EXACTOS del output.
- **Execute**: ejecuta la cuantización al staging definitivo con `checkpoint` del ledger por modelo terminado y progreso por fases (`ONNXQ_TENSOR`); verifica el output (parseo estructural: nodos `DequantizeLinear` con initializers int8, scales f32) y el input intacto.
- **Salida**: `model.onnx` cuantizado + `provenance.json`; el linaje queda en el record (tool_id `product.transformation.onnx-quantize`, parameters con `INT8`).

## Reglas de cuantización (contrato del cuantizador)

- El pass Olive `OnnxDynamicQuantization` con `weight_type=QInt8` (predeterminado del pass): los pesos MatMul/MatMulInteger se cuantizan a int8 por bloque con escala por tensor; activaciones quedan F32.
- El worker **no** interpreta resultados: el subproceso Olive es la única escritura del modelo; el worker verifica estructura y hash.

## Prueba

- Oráculo focal independiente (parser protobuf del test) sobre el modelo cuantizado: nodos DequantizeLinear, initializers int8+scales, dequantización numérica verificable contra el F32 de origen.
- Suite del host con oráculo Rust (parseo QDQ del modelo publicado) y aceptación pública completa; validación congelada con el runtime por ruta mapeado en el fixture.
