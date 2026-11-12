# P093 — UI de transformaciones: panel "Transformar modelo" en Mis modelos

Fecha: 2026-11-12. Décima fase de la línea de conversores según el [diseño fijado](P093_ui_transformaciones_diseno_2026-11-12.md): la interfaz. El pipeline público de transformaciones (plan sellado → confirmación → ejecución → publicación CAS, con la familia completa de cuantizaciones GGUF y linaje estructural) no tenía presencia en la UI; la especificación (§106, pantalla P042 "Mis modelos") exige presentar las variantes con su cuantización y derivar variantes.

## Implementación

- **Panel `ModelTransform`** (`apps/desktop/src/model-transform.tsx`) en el detalle del modelo, mismo patrón que los demás paneles de acción (ModelImport/AdapterImport): **plan visible antes de confirmar** — POST /api/v1/transform-plans con adaptador (instalaciones `family=transformation` en estado `VALIDATED`), formato de salida, cuantización opcional (campo libre; la UI no inventa enums — el daemon y el worker cualificado validan) y destino ONLINE; el panel muestra el plan sellado (id, hash truncado, pasos, cotas de almacenamiento por volumen con margen, expiración) antes de habilitar la confirmación. **Confirmación explícita** (checkbox, POST /api/v1/transformations con plan_id+plan_hash+approval_id:null). **Progreso**: GET /api/v1/jobs/{id} con primer poll inmediato y sondeo a 1,5 s hasta COMPLETED/FAILED; muestra el estado, la transform_state del worker si llega y el error del daemon si falla; al completar anuncia la variante derivada (id del result si viene) y refresca la lista de modelos (la variante nueva aparece con su cuantización estructural de la fase del linaje).
- **Enganche**: botón "Transformar a otro formato" en `ModelDetailPanel` (models.tsx).
- **Errores** con mensajes de contrato: 404 (adaptador/variante no disponibles), 422 (parámetro no soportado), 409 (plan cambiado/caducado o sin espacio), 403 (permisos), offline. La confirmación nunca se envía sin el checkbox ni sin plan visible.

## Prueba

- **Vitest** (`model-transform.test.tsx`, 3 pruebas): flujo completo (plan → muestra sellado → sin checkbox no envía confirmación → confirm → job RUNNING→COMPLETED → variante derivada anunciada, verificando cuerpos de las tres llamadas), plan rechazado 404 (mensaje de contrato y CERO llamadas a /api/v1/transformations), job FAILED (error del daemon mostrado).
- **Suite completa de la UI: 18 archivos / 150 pruebas PASS** (las 147 previas sin regresión + 3 nuevas); `tsc -b` limpio; `vite build` exitoso (bundle 493.54 kB, gzip 138.51 kB).
- La validación congelada Rust no cubre el frontend (el wrapper congela crates/apps-daemon/adapters/tools): la evidencia de esta fase son los vitest, el typecheck y el build documentados aquí.

## Ampliación posterior (mismo día): cancelación desde la UI

El panel añade el cierre cooperativo: botón "Solicitar cancelación" visible sólo con el job QUEUED/RUNNING → POST /api/v1/jobs/{id}/cancel (endpoint de la fase de cancelación pública) → el estado muestra "· cancelación solicitada" hasta que el job pasa a Cancelada; el botón desaparece mientras coopera y reaparece el detalle del cierre. Vitest 4/4 del panel (nueva prueba: POST verificado, estado de cancelación visible, botón ausente mientras coopera); **suite completa 20 archivos / 157 pruebas PASS**; typecheck y build limpios.

## Límites conservados

La pantalla expone el flujo público completo; la cuantización es campo libre hasta que exista un endpoint público de describe por adaptador (el daemon lo valida). Pendientes del plan general: cuantización ONNX (Olive/P065, bloqueada por runtime Python gestionado), aceptación de runtimes reales (P060/P099). P093 sigue `BLOCKED_REAL`/preparación sin `CODE_PASS` integral ni `REAL_PASS`; contrato 3.13.0 intacto.
