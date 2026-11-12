# P093 — linaje de cuantización estructural en la variante publicada: diseño

Fecha: 2026-11-11. Novena fase de conversores — de composición, sin cambio de los conversores. La tabla `model_variant` ya tiene columna `quantization` (el dedup por revisión la usa), el dominio `ModelVariant` ya tiene el campo y la especificación exige que la UI distinga variantes por cuantización — pero la publicación de transformaciones la construía SIEMPRE con `quantization: None`: el parámetro de cuantización sólo vivía en el linaje JSON (`lineage.parameters`) y en la provenance del worker. Esta fase lo hace estructural.

## Cambio único y sus invariantes

En `crates/larm-db/src/transformations/lifecycle/publication.rs` (`build()`): la variante derivada lleva `quantization` extraído del request sellado del plan (`r.job.parameters`). Regla genérica (no acoplada a la lista de recetas): si `parameters.quantization` es un string no vacío → `Some(ese string)`; ausente o no-string → `None`. La validez del valor ya está garantizada aguas arriba: el worker cualificado valida el enum (falla cerrado), el plan sella el request y `check_claim` compara el request del job contra el plan. La publicación no re-valida — transporta.

Invariante clave: dos conversiones de la MISMA revisión con distinta cuantización producen variantes distintas (ids planificados distintos), y el dedup `variant_exists` de la DB ya considera `quantization` en su comparación — no hay colisión posible entre Q8_0 y verbatim.

## Verificación

- **Tests públicos**: `public_gguf_quant.rs` aserta `variant["quantization"]` en los SEIS registros (Q8_0/Q4_0/Q5_0/Q4_K/Q5_K/Q6_K); `public_gguf.rs` y `public_onnx.rs` asertan `null` (sin parámetro).
- **Auditor congelado**: verifica el campo en el record retenido del caso cuantizado (6 valores) y en los casos verbatim/ONNX (null).
- Los tests de la suite del host no cambian: su ledger es privado, sin DB de Core.

## Validación congelada

La 34ª comprobación existente (public-gguf-quant) cubre el cambio; los asertos nuevos van en el test y el auditor, sin comprobaciones nuevas. Wrapper y recuentos intactos.
