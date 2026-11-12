# Adaptador de transformación GGUF (producto)

Conversor real safetensors → GGUF v3, sólo stdlib, mismo protocolo cerrado y
ledger privado que el worker de referencia (main/adapter/state/files + SDK
empaquetado). El núcleo (`gguf.py`) lee la cabecera safetensors validada y
emite el contenedor GGUF v3 (magic, kv tipados con `general.alignment` u32,
infos con dims en orden ggml, datos crudos copiados verbatim y alineados a 32
bytes) mediante una única ruta de emisión compartida por el preflight (hash
sin escritura) y la conversión (escritura durable con checkpoint por tensor).

Sub-incremento 1 de [la fase de conversores de producto](../../../docs/reports/P093_product_converters_design_2026-11-07.md):
paquete + oráculo focal independiente (`tests/test_gguf_conversion.py`, parser
GGUF propio del test). La suite de cualificación del host, el `Recipe::Gguf`
y la aceptación pública son los siguientes sub-incrementos.
