# ADR-0008 — Versionado de API y del Adapter Protocol

**Estado**: aceptado · **Fecha**: 2026-10-10 · **Prompt**: P002

## Contexto
La API administrativa y el protocolo de adaptadores son contratos consumidos por la GUI y procesos externos (Broker, plugins).

## Decisión
- API HTTP bajo prefijo **`/api/v1`** (OpenAPI generado). Cambio incompatible ⇒ `/api/v2` en paralelo; nunca romper `/api/v1` publicada.
- Adapter Protocol **`protocol_version = "1.0"`** en el manifiesto y en `core.handshake` (contrato 03): un major incompatible se rechaza antes de cualquier IO; NDJSON JSON-RPC 2.0 por stdio.
- Eventos/SSE con `payload.schema_version = 1` en cada evento (contrato 07).

## Consecuencias
- Los consumidores anclan versión; las deprecaciones se anuncian en OpenAPI y changelog.
