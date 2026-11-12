# P093 — conversores de producto reales (GGUF/ONNX): diseño y punto de continuación

Fecha: 2026-11-07. Fase siguiente explorada a fondo; NO iniciada su implementación para no dejar fases sin validación congelada (disciplina del proyecto). Este documento fija el diseño y los puntos de integración exactos para ejecutarla en la siguiente sesión.

## Estado de partida (verificado)

- La composición pública sólo registra la receta Base64 de referencia: `Backend::load` exige `Recipe::Transformation(reference)` (`apps/daemon/src/transformations/backend.rs:73`, error `transformation_planner_not_registered`) y el paso sellado del plan fija `"planner":"reference.base64.v1"` (`apps/daemon/src/transformations/planning.rs:151`).
- Las recetas son variantes serde de `Recipe` (`crates/larm-adapter-host/src/conformance/profiles.rs:10`, tag `reference.transformation.base64.v1`) y `LocalProfile::target` mapea variante→suite compilada (`profiles.rs:78` → `ReferenceTransformationSuite`).
- La suite de referencia (`crates/larm-adapter-host/src/conformance/transformation.rs`, 1143 líneas) está acoplada a Base64: CAP `vendor.reference.base64`, oráculos Base64, planner id, wire cases.
- El paquete worker de referencia (`adapters/transformation/reference/`, 719 líneas Python stdlib) acopla Base64 en ~6 puntos: formats_output, nombre de fase del plan, `encode()` (files.py), provenance logs, describe. La estructura (main/adapter/state/files/package + ledger privado + protocolo cerrado) es reutilizable tal cual.
- La selección pública ya es por `adapter_id` del request (`Backend::select`): una instalación GGUF cualificada se selecciona sin cambios de composición en la SELECCIÓN — sólo hay que aceptar la receta nueva en `Backend::load` y sellar el planner id correcto.
- El registro de perfil del operador (`probe_profiles::register`) valida el paquete bajo `data_root/adapters/{adapter_id}/{version}` y la receta del fichero de perfil — el flujo del operador sirve tal cual para la receta GGUF.
- Herramientas: `.venv-tools` NO tiene gguf/safetensors/onnx/torch; el worker usa Python stdlib empaquetado — el conversor GGUF debe escribirse en stdlib puro (GGUF v3 y safetensors son formatos binarios simples: cabecera JSON + tensor crudo).

## Plan de la fase (por sub-incrementos, cada uno con su validación)

1. **✅ COMPLETADO (2026-11-07) — Paquete adaptador GGUF real** (`adapters/transformation/gguf/`, 1.226 líneas): núcleo `gguf.py` (lector safetensors validado + escritor GGUF v3 stdlib con kv u32 según especificación, dims en orden ggml, datos verbatim alineados a 32B, UNA única ruta de emisión compartida por preflight y conversión), worker derivado del reference con los seis puntos Base64 sustituidos, `package.py` con fixture safetensors determinista (F32/F16/I64) cuyo fixture de salida se calcula con el núcleo real, y oráculo focal independiente `tests/test_gguf_conversion.py` (parser GGUF propio del test + negativo de dtype no copiable). Ambos tests en verde; arquitectura y 79 copias PASS.
2. **Suite de cualificación del host** (`GgufTransformationSuite`, NativeSuite): espejo de la de referencia con oráculos GGUF reales (parse GGUF v3 en Rust, mapeo de dtype, hashes de tensor) + los casos de protocolo/wire equivalentes. Añadir `Recipe::Gguf` (tag `product.transformation.gguf.v1`) + wiring en `profiles.rs`.
3. **Composición pública**: `Backend::load` acepta `Recipe::Gguf`; el paso sellado usa el planner id del conversor (`planner` del recipe). La publicación/verificación ya es genérica por artefactos.
4. **Aceptación pública**: fixture del paquete GGUF en los tests, instalación + sonda P026 real, plan/confirm/ejecución pública con `adapter_id` GGUF y publicación del `.gguf` verificado por parse independiente.
5. **Validación congelada**: wrapper (32ª comprobación) + auditor + recuentos.

## Riesgos señalados

- La suite GGUF del host es la pieza mayor; valorar factorizar los casos de protocolo de la suite de referencia para compartirlos.
- El estimador de tamaño del plan: `predicted` del GGUF (suma de tensores + cabecera + alineación) debe ser EXACTO (el admission compara cotas).
- El wire case de progreso/cancelación del conversor real (bloques GGUF duraderos por tensor).
- ONNX (Olive, P065) queda para después del GGUF: su worker privado existe pero depende de runtime Python gestionado.
