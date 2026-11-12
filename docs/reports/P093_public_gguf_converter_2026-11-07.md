# P093 — conversor de producto GGUF: paquete, suite del host y aceptación pública

Fecha: 2026-11-07 (noche). Primera fase de conversores de producto real según el [diseño fijado](P093_product_converters_design_2026-11-07.md): el pipeline público de transformaciones ya no queda limitado a la receta Base64 de referencia. Contrato público sin cambios de superficie.

## Implementación

- **Paquete adaptador GGUF** (`adapters/transformation/gguf/`, sub-incremento 1): conversor safetensors→GGUF v3 real en stdlib (núcleo `gguf.py` con una única ruta de emisión compartida por preflight y conversión; ledger privado y protocolo cerrado derivados del worker de referencia con los seis puntos Base64 sustituidos; progreso `GGUF_TENSOR` por bloque con checkpoint duradero ANTES del progreso) y oráculo focal independiente (parser GGUF propio del test + negativo de dtype no copiable).
- **Suite de cualificación del host** (`GgufTransformationSuite` + `Recipe::Gguf` en `profiles.rs`, sub-incremento 2): espejo completo de la suite de referencia con oráculos GGUF en Rust — parseo safetensors, emisión esperada byte a byte validada contra un golden generado por el escritor Python (conformidad cruzada embebida como `gguf_golden.rs`), y parseo estructural del contenedor publicado (versión, alineación, kv tipados, dims en orden ggml, dtype ggml, bytes de tensor idénticos al safetensors fuente). 51 pruebas del crate, 4 nuevas del módulo GGUF.
- **Composición** (sub-incremento 3): `Backend::load` acepta `Recipe::Gguf`; el `planner_id` pasa a ser por receta (`reference.base64.v1` / `product.gguf.v1`) sellado en el paso de publicación y verificado por `check_destination`; el estimado del plan aplica el invariante por receta (Base64: expansión 4/3 exacta; GGUF: tamaño declarado por el worker cualificado con consistencia upper=final+control y ancla en su re-preflight fail-closed de execute).
- **Aceptación pública** (sub-incremento 4): `transformation_publica_gguf_conversor_cualificado_hasta_publicacion` — sonda P026 real con la suite GGUF del host, plan/confirmación/ejecución públicas con `adapter_id` product.transformation.gguf, 337 observaciones de progreso `GGUF_TENSOR` por `GET /jobs/{id}`, publicación CAS del contenedor verificado por el oráculo Rust contra el safetensors fuente (3 tensores, dtype/dims/bytes exactos).

## Defectos encontrados y corregidos durante la fase

1. **data_begin del oráculo GGUF** (Python y Rust): contaba el prefijo de 24 bytes dos veces al alinear; el fixture pequeño redondeaba igual por casualidad y el de 12 MB lo destapó.
2. **Orden de tensores**: serde_json ordena claves (BTreeMap); la emisión se ordena por offset de datos del safetensors, no por nombre.
3. **Exclusión de la cualificación durante el preview del plan GGUF**: el worker de preview retiene la exclusión de la instalación y `store_plan` la readquiría (409); el preview cierra y suelta antes de persistir (ya corregido en la fase del relé de progreso; esta fase lo re-ejercitó).
4. **Carrera checkpoint/progreso**: el sink emitía el progreso antes de confirmar el checkpoint en el ledger; el host veía el evento y consultaba un offset aún inexistente (fallo `gguf_completed_before_tensor_checkpoint` intermitente). Ahora checkpoint→progreso, misma disciplina del worker de referencia.

## Validación congelada

[attempt2](P093_runs/public-gguf-20261107/attempt2/verification-synopsis.json) — **514 pruebas PASS / 0 FAIL / 0 ignoradas, 32/32 comprobaciones** (la 32ª nueva: public-gguf), auditor independiente PASS (906 entradas, 32 logs/recibos). Attempts 1 conservado: verify 32/32 PASS y audit con base de recuento errónea (446 vs 447), corregida en attempt2.

## Límites conservados

El conversor copia tensores sin recuantizar (F32/F16/BF16/I64/I32/I16/I8): la cuantización real y los conversores ONNX (Olive, P065) siguen pendientes, así como la aceptación de runtimes y la UI. P093 sigue `BLOCKED_REAL`/preparación sin `CODE_PASS` integral ni `REAL_PASS`; contrato 3.13.0 intacto.
