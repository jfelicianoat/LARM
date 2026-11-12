# P093 — flujo de promoción de variantes (P120)

Fecha: 2026-11-12. Undécima fase de la línea de conversores según el [diseño fijado](P093_promocion_variantes_diseno_2026-11-12.md): el ciclo de vida. La especificación §120 define `CANDIDATE → TECHNICALLY_APPROVED → EVALUATION_APPROVED → DEFAULT` para que el AI Broker futuro use automáticamente sólo las categorías permitidas por su política. El dominio ya declaraba los 7 valores y la DB guardaba el campo — pero no existía flujo: las variantes derivadas por transformación nacían `NONE` sin forma de avanzarlas.

## Implementación

- **Dominio** (`model.rs`): `can_transition_promotion` (escalón ascendente estricto + laterales `REJECTED` —jamás desde `DEFAULT`— y `PINNED` desde cualquier estado no rechazado) y `next_promotion` para la interfaz.
- **DB** (`variants.rs set_promotion`): transacción con lock explícito, validación de la transición contra la máquina del dominio, **exclusividad de `DEFAULT` por (revision_id, format, quantization)** — la `DEFAULT` previa de la misma combinación retrocede a `EVALUATION_APPROVED` en la misma transacción con su evento (coherente con el dedup `variant_exists`) —, evento de outbox con `{from,to}` y `row_version` optimista (STALE_REVISION al conflicto).
- **API**: `PATCH /api/v1/variants/{id}/promotion` con `{promotion, expected_row_version}` (deny_unknown_fields); 200 con la variante actualizada y nueva row_version; 404/409/422 según el fallo.
- **UI** (`model-promote.tsx` en el detalle del modelo): muestra el estado con etiquetas en español, ofrece el siguiente escalón legal con aviso explícito de que la `DEFAULT` previa retrocede, laterales (`REJECTED`/`PINNED`) con confirmación de dos pasos, `expected_row_version` capturado al abrir y actualizado tras cada cambio; errores de contrato diferenciados (403/409/422/offline).

## Prueba

- **Dominio**: tabla completa de transiciones (12 pares legales/prohibidos) en `variant_promotion.rs`.
- **SQL** (4 pruebas): ascenso de un escalón + salto prohibido falla cerrado sin cambiar nada; **exclusividad real de DEFAULT** (dos variantes misma combinación: al promover la segunda, la primera retrocede; la de otra combinación no se toca); STALE_REVISION con row_version vieja.
- **API**: suite `models` 6/6 con el mock del trait ampliado.
- **UI**: vitest 3/3 (ascenso con row_version correcta, aviso del rebajo de la DEFAULT previa sin llamadas, lateral REJECTED con confirmación y error 409 mostrado); **suite completa 18 archivos / 153 pruebas PASS** (150 previas + 3 nuevas); `tsc -b` limpio; `vite build` exitoso.
- **Regresión Rust**: dominio 181, DB lib 77, fmt y clippy (-D warnings, dominio/db/api/daemon) limpios.

## Límites conservados

El AI Broker futuro consumirá `DEFAULT`/`PINNED` según su política (fuera de esta fase). Pendientes: cuantización ONNX (Olive/P065, bloqueada por runtime Python gestionado), aceptación de runtimes reales (P060/P099). P093 sigue `BLOCKED_REAL`/preparación sin `CODE_PASS` integral ni `REAL_PASS`; contrato 3.13.0 intacto.
