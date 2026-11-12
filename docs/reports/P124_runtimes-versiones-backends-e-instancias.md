# P124 — Versiones, backends e instancias

Estado: `BLOCKED_REAL`.

La navegación `#/instancias` ya presenta las instancias registradas y permite consultar su detalle junto con el modelo y backend asociados. La ficha muestra estado, PID, hora local de inicio, tiempo activa, propiedad, perfil, huella de ejecución, hash del ejecutable y última actividad. La memoria procede del snapshot inmutable del plan de inicio y aparece como estimación junto al límite configurado; la pantalla aclara que la API aún no entrega consumo medido, endpoint ni ubicación de logs.

La parada requiere una confirmación explícita. Al confirmarla, la UI envía la razón `operator_requested` con una clave de idempotencia y consulta el trabajo durable para mostrar su estado. Las instancias externas quedan en solo lectura. Los errores de conexión, permiso, conflicto y recurso ausente permiten recuperar la vista sin presentar el trabajo como completado antes de recibir ese estado.

La pantalla de runtimes conserva visibles los canales de versión y los candidatos `BROKEN`; las pruebas verifican que no se ofrezca promover un candidato fallido ni borrar o adoptar de nuevo una instalación externa. `api-operations.json` sí define promoción (P117) y rollback (P032), pero sus handlers y controles de interfaz aún no están conectados. El flujo plan→instalación→validación→promoción y la reversión siguen pendientes de implementación.

## Comprobaciones

- Pruebas focales de instancias y runtimes: 24/24 PASS.
- Suite completa del escritorio: 9 archivos y 84/84 PASS.
- ESLint, TypeScript, build de producción y `cargo fmt --all -- --check`: PASS.
- API de instancias: 4/4 pruebas PASS para autorización, paginación firmada, validación y replay idempotente.
- Parada durable y propiedad: 4/4 pruebas PASS para scope, leases, recuperación, outbox y estado terminal.
- Evidencia formal: 8 PASS, 0 FAIL y 1 comprobación REAL `BLOCKED`; el recibo pasó `tools/validate_evidence.py`.

## Pendiente para cerrar P124

`RuntimeInstance` aún no expone endpoint, memoria medida ni ubicación de logs. Las operaciones de promoción y rollback existen en el contrato, pero no están conectadas a handlers de API ni a la UI. P032/P033/P037 tampoco han alcanzado `REAL_PASS`, y no hay una instalación de runtime cualificada para demostrar el ciclo sobre Windows. La UI no sustituye esa integración real.

La evidencia repetible está en [PromptEvidence](P124_runtimes-versiones-backends-e-instancias.evidence.json), con logs verificables bajo `P124_runs/`. El runner es `python tools/p124_evidence.py`. La pantalla de instancias y sus pruebas están en [instances.tsx](../../apps/desktop/src/instances.tsx) y [instances.test.tsx](../../apps/desktop/src/instances.test.tsx); las pruebas de inventario de runtimes están en [runtimes.test.tsx](../../apps/desktop/src/runtimes.test.tsx).
