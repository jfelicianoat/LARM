# P093 — UI de transformaciones: panel "Transformar modelo" en Mis modelos

Fecha: 2026-11-12. Décima fase de la línea de conversores: la interfaz. El pipeline público de transformaciones (plan → confirmación → ejecución → publicación CAS, con seis formatos de cuantización GGUF y linaje estructural) no tenía presencia en la UI: la especificación (§106, pantalla P042 "Mis modelos") exige que la UI presente las variantes con su cuantización y permita derivar variantes. La lista ya muestra `quantization` (la fase del linaje la alimenta); falta el flujo de derivación.

## Diseño

Panel `ModelTransform` en el detalle del modelo (`model-transform.tsx`), mismo patrón que ModelImport/AdapterImport (preview → confirmación explícita → job):

1. **Plan**: seleccionar adaptador de transformación (instalaciones `family=transformation` en estado `VALIDATED` vía `GET /api/v1/adapters?family=transformation&limit=200`), destino (`GET /api/v1/storage-locations`, ONLINE) y parámetros (cuantización opcional: campo de texto libre — el daemon y el worker cualificado validan; la UI no inventa enums). `POST /api/v1/transform-plans` con `input_variant_id` del detalle → muestra el plan sellado (id, hash, pasos, cotas de almacenamiento por volumen, expiración) ANTES de poder confirmar.
2. **Confirmación explícita** (checkbox, como el import): `POST /api/v1/transformations` con `plan_id+plan_hash+approval_id:null` → 202 con receipt.
3. **Progreso**: `GET /api/v1/jobs/{id}` cada 1,5 s hasta COMPLETED/FAILED; muestra el estado del job y el progreso del worker si llega. Al completar: mensaje con el id de la variante derivada y `onCompleted()` refresca la lista de modelos (la variante nueva aparece con su cuantización estructural).

Errores con `explain()`: 404 (adaptador/variante desconocidos), 409 (plan caducado/replay/sin presupuesto), 422 (parámetro no soportado), offline. La UI nunca inventa el enum de cuantización ni el resultado: muestra lo que el plan y el job reportan.

## Prueba

- **Vitest** (`model-transform.test.tsx`): flujo completo plan→confirm→job COMPLETED (con las tres llamadas verificadas por path y cuerpo), confirmación bloqueada sin checkbox, plan rechazado (404) muestra el error y no envía confirmación, job FAILED muestra el error del daemon.
- **Typecheck + build** (`tsc -b`, `vite build`) en verde.
- La validación congelada Rust no cubre el frontend (el wrapper congela fuentes de crates/apps-daemon/adapters/tools): la evidencia de esta fase son los vitest y el build, documentados en el informe.
