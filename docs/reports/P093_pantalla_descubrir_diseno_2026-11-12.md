# P093 — pantalla Descubrir: búsqueda federada de modelos en la interfaz

Fecha: 2026-11-12. Fase de la línea de conversores: la pantalla "Descubrir" del menú (§106, P044/P121) era un stub. El pipeline de búsqueda federada ya está montado de punta a punta en el daemon (`POST /api/v1/models/search` → `QualifiedSourceSearch` → `larm_models::search`, con `Page<SearchCandidate>` y warnings por fuente en metadata) — la pantalla lo consume **sin endpoints nuevos**.

## Diseño

1. **Formulario** (`discover.tsx`): texto libre (`q`), filtros por formato y cuantización (las familias que ya gestiona LARM), compatibilidad (`ALL`/`COMPATIBLE`/`INCOMPATIBLE`/`UNKNOWN` como el enumerado del dominio), y límite de página (50). El DTO `Search` del contrato es cerrado y con TODOS los campos required: la UI construye el objeto completo con arrays vacíos/nulls por omisión (según contrato, array vacío omite el filtro).
2. **Resultados**: lista de `SearchCandidate` — nombre del modelo (canonical), fuente (source_id + prioridad), formato/cuantización/arquitectura, tamaño estimado con su confianza, autor, licencias, technical_fit y trust_score cuando vengan. Paginación por `cursor` (siguiente página) y aviso de advertencias (`warnings`) por fuente si las hay.
3. **Navegación**: cada candidato con `variant_id` de una variante ya registrada enlaza a su detalle en Mis modelos (`#/mis-modelos?id=...`); si el candidato no está registrado aún, se muestra sin enlace (la adquisición desde descubrir es P044 y llega por su propia aceptación).
4. **Errores**: 403 (permisos), offline, y el mensaje del contrato para búsquedas inválidas.

## Prueba

Vitest (`discover.test.tsx`): búsqueda con el DTO completo verificado por cuerpo, resultados renderizados con sus campos, paginación con cursor, warnings mostrados, vacío y error de contrato. Typecheck y build.
