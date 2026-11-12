# ADR-0007 — Identificadores UUID v7 y timestamps UTC ISO-8601

**Estado**: aceptado · **Fecha**: 2026-10-10 · **Prompt**: P002

## Contexto
El dominio necesita IDs únicos, ordenables por tiempo y serialización de tiempo sin ambigüedades.

## Decisión
- **UUID v7** para todas las entidades del dominio (elegido sobre ULID por soporte nativo del crate `uuid` y SQLx). Una sola convención en todo el dominio.
- **Timestamps en UTC**, RFC 3339 con `Z` y **milisegundos** exactos (`2026-10-11T04:07:43.192Z`, contrato 00). `chrono::DateTime<Utc>` en memoria; serialización siempre a ese formato. Prohibido mezclar formatos, precisiones o zonas locales en DB o API.
- Reglas comunes del contrato 00 que acompañan a la identidad: SHA256 hexadecimal en minúsculas; JSON canónico RFC 8785 antes de hashear; UTF-8; tamaños `u64` limitados a 9223372036854775807 para SQLite y representados como string decimal en JSON (nunca `number` JS); cero no representa unknown (unknown es `null` con `reason` y `evidence`).

## Consecuencias
- El orden por id refleja creación; las comparaciones de tiempo no requieren conversión de zona.
