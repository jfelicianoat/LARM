# P093 — pantalla Descubrir: búsqueda federada de modelos en la interfaz

Fecha: 2026-11-12. Fase de la línea de conversores según el [diseño fijado](P093_pantalla_descubrir_diseno_2026-11-12.md): la pantalla "Descubrir" del menú (§106, P044/P121) era un stub. El pipeline de búsqueda federada ya estaba montado de punta a punta en el daemon (`POST /api/v1/models/search` → `QualifiedSourceSearch` → `larm_models::search`, respuesta `Page<SearchCandidate>` con warnings por fuente) — la pantalla lo consume **sin endpoints nuevos**.

## Implementación

- **`DiscoverPage`** (`apps/desktop/src/discover.tsx`), enganchada en App.tsx para la ruta `descubrir`:
  - **Formulario**: texto libre (`q`), formato, cuantización y compatibilidad (enumerado del dominio `ALL/COMPATIBLE/INCOMPATIBLE/UNKNOWN`). La UI no construye un DTO parcial: `buildSearchRequest` produce el objeto `Search` completo del contrato (todos los campos required, arrays vacíos omite el filtro por contrato, `q` recortado).
  - **Resultados**: lista de `SearchCandidate` — nombre canónico, fuente con prioridad, autor, formato/cuantización/arquitectura, tamaño con `formatBytes`, technical_fit y trust_score en %; paginación por `cursor` acumulando resultados (botón desaparece al agotarse); advertencias (`warnings`) por fuente como estados informativos.
  - **Navegación**: candidatos con `variant_id` de variante registrada enlazan a su detalle en Mis modelos; los no registrados se muestran sin enlace (la adquisición desde descubrir llega con P044).
  - **Errores de contrato**: 403/offline/mensajes diferenciados; el DTO inválido muestra el error genérico sin exponer detalles internos.
- Regla de alcance: la pantalla busca y navega; la adquisición directa desde un candidato no registrado no se afirma (P044 la trae con su propia aceptación).

## Prueba

- **Vitest** (`discover.test.tsx`, 4 pruebas): DTO completo verificado por cuerpo con filtros y advertencia de fuente renderizada, paginación con cursor acumulando y sin botón al agotarse, error de contrato 422, y la normalización de `buildSearchRequest` (recorte de `q`, defaults del contrato).
- **Suite completa: 21 archivos / 161 pruebas PASS** (157 previas + 4 nuevas); `tsc -b` limpio; `vite build` exitoso.
- La validación congelada Rust no cubre el frontend; la evidencia son los vitest, el typecheck y el build.

## Límites conservados

La búsqueda federada contra fuentes upstream reales es la aceptación P044 que sigue pendiente (los resultados contra fuentes reales dependen de las fuentes cualificadas del entorno). Benchmarks sigue como stub del menú. P093 sigue `BLOCKED_REAL`/preparación sin `CODE_PASS` integral ni `REAL_PASS`; contrato vigente **3.14.0**.
