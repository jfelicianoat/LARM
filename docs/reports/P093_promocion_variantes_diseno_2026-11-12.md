# P093 — flujo de promoción de variantes (P120): diseño

Fecha: 2026-11-12. Undécima fase de la línea de conversores: el ciclo de vida. La especificación §120 define el ciclo de promoción de una variante (`CANDIDATE → TECHNICALLY_APPROVED → EVALUATION_APPROVED → DEFAULT`) para que el AI Broker futuro use automáticamente sólo las categorías permitidas por su política. El dominio ya declara los 7 valores (`NONE, CANDIDATE, TECHNICALLY_APPROVED, EVALUATION_APPROVED, DEFAULT, PINNED, REJECTED`) y la DB guarda el campo — pero no existía flujo: las variantes derivadas por transformación nacen `NONE` y no había forma de avanzarlas.

## Máquina de estados (transiciones legales)

Ascendente estricta, una escalón por operación:
- `NONE → CANDIDATE`
- `CANDIDATE → TECHNICALLY_APPROVED`
- `TECHNICALLY_APPROVED → EVALUATION_APPROVED`
- `EVALUATION_APPROVED → DEFAULT` — **DEFAULT es exclusivo por (revision_id, format, quantization)**: al promover, la variante `DEFAULT` previa de la misma combinación retrocede a `EVALUATION_APPROVED` en la misma transacción (coherente con el dedup `variant_exists` que considera revision+format+quantization).

Laterales:
- `REJECTED` desde `NONE/CANDIDATE/TECHNICALLY_APPROVED/EVALUATION_APPROVED` (jamás desde `DEFAULT`; para quitar un DEFAULT se promueve otro).
- `PINNED` desde cualquier estado excepto `REJECTED` (ancla una variante para el broker).
- Desde `DEFAULT` sólo se puede pasar a `PINNED` o ceder el puesto promoviendo otra variante.

## Implementación

1. **Dominio** (`model.rs`): `PromotionTransition` — función pura `fn can_transition(from: &str, to: &str) -> bool` con la tabla anterior + tipos de request.
2. **DB** (`variants.rs`): `set_promotion(conn, id, to, expected_row_version, ctx)` — transacción: lee la variante actual, valida la transición con la máquina del dominio, si `to==DEFAULT` rebaja la DEFAULT previa de la misma (revision, format, quantization) a `EVALUATION_APPROVED`, actualiza `promotion` y `row_version`, escribe evento al outbox. Optimista por `row_version` (409 en conflicto).
3. **API** (`models.rs`): `PATCH /api/v1/variants/{id}/promotion` con `{promotion, expected_row_version}` (deny_unknown_fields); 200 con la variante actualizada; 409/422/404 según el fallo. Scope existente de escritura de modelos.
4. **UI** (`models.tsx`/`model-promote.tsx`): en el detalle del modelo, botones de promoción que ofrecen el siguiente estado legal (y REJECTED/PINNED donde proceda) con confirmación explícita y `expected_row_version` leído al abrir.
5. **Tests**: unitarios de dominio (tabla completa de transiciones), API (404/409/422/200 y rebajo de la DEFAULT previa), SQL (transacción y exclusividad), UI vitest (flujo con confirmación).

## Prueba

- `cargo test` de dominio/API/DB + clippy + fmt.
- Vitest de la UI + suite completa.
- La validación congelada Rust no cubre el frontend; la evidencia backend son las suites existentes ampliadas.
