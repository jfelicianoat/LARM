# P093 — linaje de cuantización estructural en la variante publicada

Fecha: 2026-11-11. Novena fase de conversores según el [diseño fijado](P093_linaje_cuantizacion_diseno_2026-11-11.md) — de composición, sin cambio de los conversores. La tabla `model_variant` ya tenía columna `quantization` (el dedup por revisión la usa), el dominio `ModelVariant` ya tenía el campo y la especificación exige que la UI distinga variantes por cuantización — pero la publicación de transformaciones construía SIEMPRE la variante derivada con `quantization: None`: el parámetro sólo vivía en el linaje JSON y en la provenance del worker. Ahora es estructural.

## Cambio

En el constructor de la variante derivada (`publication.rs build()`): `quantization` se extrae del request sellado del plan (`r.job.parameters`). Regla genérica no acoplada a la lista de recetas: si `parameters.quantization` es un string no vacío → `Some(ese string)`; ausente o no-string → `None`. La validez del enum no se re-valida en la publicación — transporta: el worker cualificado valida el enum (falla cerrado), el plan sella el request y `check_claim` compara el request del job contra el plan. Invariante de identidad: dos conversiones de la misma revisión con distinta cuantización producen variantes distintas, y el dedup `variant_exists` de la DB ya considera `quantization` — sin colisión posible entre cuantizadas y verbatim.

## Verificación

- **Tests públicos**: `public_gguf_quant.rs` aserta `variant["quantization"]` en los SEIS registros (Q8_0/Q4_0/Q5_0/Q4_K/Q5_K/Q6_K); `public_gguf.rs` y `public_onnx.rs` asertan `null` (sin parámetro).
- **Auditor congelado**: verifica el campo en el record retenido del caso cuantizado (debe estar en el conjunto de los seis) y `null` en los casos verbatim Base64 (public_audit) y ONNX.
- Los tests de la suite del host no cambian: su ledger es privado, sin DB de Core.

## Validación congelada

[attempt17](P093_runs/public-gguf-quant-20261108/attempt17/verification-synopsis.json) — **516 pruebas PASS / 0 FAIL / 0 ignoradas, 34/34 comprobaciones**, auditor independiente PASS con las verificaciones nuevas: el record retenido del caso cuantizado lleva `variant["quantization"]` dentro de {Q8_0,Q4_0,Q5_0,Q4_K,Q5_K,Q6_K} (el auditor lo comprueba al abrir el parseo de cada contenedor), y los casos verbatim Base64 y ONNX llevan `null`. **516 pruebas** — el recuento no cambia (los asserts nuevos viven dentro de los 6 tests públicos existentes y del auditor). La fase pasó a la primera en la validación congelada: sin attempts fallidos conservados.

## Límites conservados

La cuantización ONNX real (Olive/P065) sigue bloqueada por el runtime Python gestionado; la aceptación de runtimes reales (P060/P099) y la UI siguen pendientes. P093 sigue `BLOCKED_REAL`/preparación sin `CODE_PASS` integral ni `REAL_PASS`; contrato 3.13.0 intacto.
