# P093 — pantalla de inicio (dashboard de la sesión): diseño

Fecha: 2026-11-12. Duodécima fase de la línea de conversores: la pantalla "Inicio" del menú (§106, primera posición) era un stub. Ahora compone el estado real de la sesión con los datos que las fases previas dejaron servidos por endpoints ya probados — sin endpoints nuevos.

## Tarjetas

1. **Estado del daemon**: snapshot de conexión (En línea/Degradado/Sin conexión) con la hora de la última comprobación y enlace al diagnóstico (P006).
2. **Modelos** (`GET /api/v1/variants?limit=200`): total, desglose por estado de promoción P120 (Predeterminada/Candidata/…) y nº de variantes con cuantización registrada; enlace a Mis modelos.
3. **Adaptadores** (`GET /api/v1/adapters?limit=200`): instalados por familia, cuántos cualificados (`VALIDATED`); enlace a Adaptadores.
4. **Almacenamiento** (`GET /api/v1/storage-locations?limit=200`): ubicaciones ONLINE y presupuesto usable total (volume_budget); enlace a Almacenamiento.
5. **Trabajos en curso** (`GET /api/v1/transformations?limit=200` + `GET /api/v1/downloads?limit=200`): los QUEUED/RUNNING de ambos lados; enlace a Descargas.

Cada tarjeta falla sola (`Promise.allSettled`): un fallo de un recurso no tumba el resto; el estado del daemon viene del shell.

## Prueba

Vitest (`home.test.tsx`): dashboard con datos completos (contadores correctos, enlaces a secciones, jobs en curso listados), tarjeta con fallo parcial (el resto se muestra), sin datos (mensajes "sin …"). Typecheck y build.
