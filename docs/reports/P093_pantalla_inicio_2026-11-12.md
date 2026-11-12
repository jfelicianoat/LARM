# P093 — pantalla de inicio: dashboard de la sesión

Fecha: 2026-11-12. Duodécima fase de la línea de conversores según el [diseño fijado](P093_pantalla_inicio_diseno_2026-11-12.md): la pantalla "Inicio" del menú (§106, primera posición) era un stub. Ahora compone el estado real de la sesión con los datos que las fases previas dejaron servidos por la API pública — **sin endpoints nuevos**.

## Implementación

- **`HomePage`** (`apps/desktop/src/home.tsx`), enganchada en App.tsx para la ruta `inicio`. Cinco tarjetas, cada una carga y falla por su lado (`Promise.allSettled`):
  1. **Estado del daemon**: snapshot de conexión con hora de la última comprobación y enlace al diagnóstico (P006).
  2. **Modelos** (`/api/v1/variants`): total ("Más de 200" si la primera página llena), desglose por estado de promoción P120 con etiquetas en español, y nº de variantes con cuantización registrada (la fase del linaje las alimenta); enlace a Mis modelos.
  3. **Adaptadores** (`/api/v1/adapters`): total, desglose por familia, nº cualificados (VALIDATED); enlace a Adaptadores.
  4. **Almacenamiento** (`/api/v1/storage-locations`): ubicaciones ONLINE sobre el total y presupuesto usable agregado (volume_budget, formato con `formatBytes`); enlace a Almacenamiento.
  5. **Trabajos en curso** (`/api/v1/transformations` + `/api/v1/downloads`): los QUEUED/RUNNING de ambos lados con tipo, estado y adaptador; enlace a Descargas.
- Un fallo de un recurso muestra el error en su tarjeta sin tumbar el resto; el estado del daemon lo gestiona el shell.

## Prueba

- **Vitest** (`home.test.tsx`, 3 pruebas): dashboard con datos completos (contadores de promoción/cuantización/familia/cualificados, presupuesto usable, trabajos en curso de ambos lados, enlaces a las cinco secciones), degradación parcial (una tarjeta con error, el resto con datos), vacíos ("Sin descargas ni transformaciones en curso.").
- **Suite completa: 18 archivos / 156 pruebas PASS** (153 previas + 3 nuevas); `App.test.tsx` actualizado: la pantalla inicio ya no es stub (aserta el heading "Estado del daemon" en lugar del mensaje "todavía no está disponible"); `tsc -b` limpio; `vite build` exitoso.

## Límites conservados

Descubrir y Benchmarks siguen como stubs del menú. Pendientes del plan general: cuantización ONNX (Olive/P065, bloqueada por runtime Python gestionado), aceptación de runtimes reales (P060/P099). P093 sigue `BLOCKED_REAL`/preparación sin `CODE_PASS` integral ni `REAL_PASS`; contrato 3.13.0 intacto.
