# Revisión contractual 3.14.0 — promoción de variantes (P120)

Fecha: 2026-11-12. Predecesora:3.13.0. Publicación e implementación autorizadas expresamente por el operador al responder «Autorizar revisión e implementación» a la propuesta3.14.0. Contrato normativo; no acredita por sí mismo implementación ni REAL_PASS.

## Problema comprobado

La especificación §120 define el ciclo de promoción de variantes (`CANDIDATE → TECHNICALLY_APPROVED → EVALUATION_APPROVED → DEFAULT`) para que el AI Broker use automáticamente sólo las categorías permitidas por su política. El dominio declara los siete valores (`PROMOTION`), la tabla `model_variant` guarda el campo y su dedup `variant_exists` ya considera (revision, format, quantization) — pero el contrato de API no expone ninguna operación para cambiar la promoción: las variantes derivadas por transformación nacen `NONE` y no hay forma de avanzarlas.

Fuentes: `contracts/01_DOMAIN_STATES.md`, `PROMOTION`; `contracts/api-operations.json`; `schemas/larm.schema.json`, `model_variant`. El contrato00 exige resolver con el operador los cambios de alcance, conservando los requisitos.

## Operación y reglas normativas

Se añade una operación, reutilizando los DTOs existentes:

```json
{
  "id": "P120_patch__variants_id_promotion",
  "method": "PATCH",
  "path": "/api/v1/variants/{id}/promotion",
  "scope": "models:write",
  "request_schema": null,
  "response_schema": "larm.schema.json#/$defs/model_variant",
  "owner": "P120",
  "mode": "sync",
  "success_status": 200,
  "path_parameters": {
    "id": {
      "type": "string",
      "pattern": "^[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$"
    }
  }
}
```

Cuerpo cerrado `{promotion, expected_row_version}` (string del enumerado PROMOTION + row_version optimista). Reglas:

1. **Máquina de estados**: escalón ascendente estricto (`NONE→CANDIDATE→TECHNICALLY_APPROVED→EVALUATION_APPROVED→DEFAULT`); laterales `REJECTED` (jamás desde `DEFAULT`) y `PINNED` (desde cualquier estado no rechazado). Transición ilegal → `INVALID_STATE_TRANSITION` (422) sin cambio.
2. **Exclusividad de `DEFAULT`** por (revision_id, format, quantization): al promover, la variante `DEFAULT` previa de la misma combinación retrocede a `EVALUATION_APPROVED` en la MISMA transacción, con su evento `{action:"promotion", from:"DEFAULT", to:"EVALUATION_APPROVED", reason:"reemplazada"}`. Coherente con el dedup `variant_exists`.
3. **Optimista**: `expected_row_version` debe coincidir; conflicto → `STALE_REVISION` (409).
4. **Auditoría**: evento de outbox `{action:"promotion", from, to}` en cada cambio.
5. El `AI Broker` futuro consumirá `DEFAULT`/`PINNED` según su política (fuera de esta revisión).

## Evidencia de implementación (anexa, no contractual)

- Dominio: tabla completa de 12 transiciones legales/prohibidas.
- SQL: ascenso, salto prohibido falla cerrado, exclusividad real de DEFAULT con tres variantes, STALE_REVISION — 4/4.
- API: PATCH montado, suite `models` 6/6; OpenAPI publicado como implementado (`x-larm-implemented`) y catálogo actualizado con la operación.
- UI: panel de promoción con confirmación explícita; vitest 3/3; suite completa 157/157; typecheck y build limpios.
- Regresión: dominio 181, DB 77, fmt y clippy -D warnings limpios.
