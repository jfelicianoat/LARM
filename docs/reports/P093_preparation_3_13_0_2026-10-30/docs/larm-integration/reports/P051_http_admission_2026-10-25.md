# P051/P122 — confirmación explícita y admisión HTTP de descargas

Estado: `BLOCKED_REAL`. Incremento verificado en Windows, host `A9_MEGA`, el 2026-10-25. Amplía la [prueba anterior de reinicio](P051_daemon_restart_2026-10-25.md) hasta las rutas HTTP de planificación y confirmación, y conecta la confirmación en “Mis modelos”. No constituye aceptación completa de las fases.

## Cambio funcional

El daemon utiliza `QualifiedDownloadExecutionGate` en lugar del guardián que rechazaba toda ejecución. La admisión y el consumidor comparten el mismo resolver: exige una instalación cualificada para ese ejecutable/contexto, protocolo de bloques, revisión y manifiesto vigentes y permisos del origen. Abre una sesión, pide el plan de la fuente, valida el snapshot con el mismo oráculo que el consumidor y cierra la sesión antes de crear el trabajo. Autenticación privada y distribuciones con licencia declarada siguen rechazadas mientras no esté conectado su flujo de autorización.

La validación física del conector y su RPC se realizan antes de refrescar el espacio. El primer intento HTTP detectó `free_snapshot_stale`: el sondeo anterior caducaba durante esa validación. Se corrigió el orden; la frescura máxima de cinco segundos y las comprobaciones transaccionales de reserva permanecen exigidas.

“Mis modelos” habilita «Confirmar y crear descarga» únicamente con un plan vigente para el destino elegible seleccionado. El clic envía `operator_confirmed:true` y el hash sellado. Preparar un plan no inicia transferencias. La interfaz bloquea el doble envío, muestra el recibo y enlaza a Descargas; ante una respuesta perdida conserva cuerpo y clave para «Comprobar solicitud» sin duplicar trabajos. Durante esa incertidumbre bloquea cambios del plan. Un plan caducado o un rechazo conocido no se presenta como descarga creada.

## Prueba con dos procesos reales

El catálogo inicial usa datos controlados en SQLite. La planificación, rechazo, cualificación y aceptación posteriores se ejecutan a través de la API autenticada del daemon. Una credencial efímera vive sólo en memoria y se revoca al terminar.

| Observación | Resultado |
| --- | --- |
| Preflight HTTP | 201 |
| Cualificación emitida por otro ejecutable | 409, `conformance_context_changed`; cero trabajos |
| Confirmación `false` | 400; cero trabajos y reservas |
| Confirmación explícita | 202; un trabajo |
| Repetición de clave y cuerpo | 202; recibo idéntico, sigue habiendo un trabajo |
| Checkpoint antes de terminar el primer daemon | 65.536 bytes |
| Petición tras reiniciar | `Range: bytes=65536-`, `If-Range` conserva ETag |
| Archivo final | 2.097.152 bytes; SHA-256 `5647f05ec18958947d32874eeb788fa396a05d0bab7c1b71f112ceb7e9b31eee` |
| Finalización | intento 2, `COMPLETED`, CAS y procedencia correctos |
| Journal y recuperación | `COMPLETED`, un evento de recuperación |
| Reservas / eventos sin outbox | 0 / 0 |
| Principal temporal | revocado |

Recibos: [result.json](P051_runs/daemon-restart/01a17550-e1e9-70fe-a308-21ce941c0d53/result.json), [peticiones reanudadas](P051_runs/daemon-restart/01a17550-e1e9-70fe-a308-21ce941c0d53/resumed-requests.json) y [comprobaciones y hashes de este incremento](P051_runs/daemon-restart/01a17550-e1e9-70fe-a308-21ce941c0d53/increment.checks.json). El recibo conserva el hash del ejecutable que realmente se probó; compilaciones posteriores no sustituyen esa identidad.

## Verificación

- Integración `source_suite_nativa_completa_descarga_recupera_y_confirma_core_sqlite`: 1/1 PASS, 282,46 s, con las aserciones HTTP anteriores y reinicio completo.
- `cargo test -p larm-download --lib -j1 -- --test-threads=1`: 42/42 PASS.
- `cargo test -p larm-api --test download_plans -j1 -- --test-threads=1`: 4/4 PASS.
- `cargo test -p larm-daemon --bin larm-daemon download_execution::tests -j1 -- --test-threads=1`: 3/3 PASS.
- Interfaz: módulo de modelos 18/18 y suite completa 118/118 en 16 archivos PASS. Incluye confirmación explícita/doble clic, replay tras pérdida de respuesta, caducidad y rechazo de acceso.
- ESLint y build de producción con TypeScript: PASS. Se repitieron después de corregir el contraste del nuevo enlace; la primera repetición de lint detectó dos símbolos sobrantes en el arnés temporal de revisión visual. Ese arnés se retiró y la repetición final pasó.
- `cargo clippy -p larm-download -p larm-daemon --all-targets -j1 -- -D warnings`: salida 0. El compilador avisó de que copiaba la caché incremental porque no pudo crear hard links; no hubo diagnósticos de Clippy sobre el código.
- `cargo fmt --all -- --check`: PASS. Sintaxis de los runners Python: PASS.
- Revisión visual de la pantalla real con transporte simulado en escritorio y anchura 375: confirmación y éxito visibles, sin desbordamiento horizontal del documento (360 px sobre viewport de 375). Se corrigió el enlace con el color de acento existente. El servidor y las dos entradas temporales se retiraron al terminar.

Capturas simuladas: [resultado escritorio](../../target/visual-checks/confirmation/desktop-result.png) y [pantalla estrecha](../../target/visual-checks/confirmation/mobile.png). Acreditan presentación; no sustituyen la interacción de la UI con el daemon real.

## Pendiente

P051 y P122 conservan `BLOCKED_REAL`. Faltan upstream real cualificado, credenciales/licencias autorizadas, inyección integrada de disco lleno, ETag/hash alterados, controles completos de pausa/reanudación/cancelación, medición H08 de dos transferencias y los `REAL_PASS` de sus dependencias. El recorrido GUI→daemon de extremo a extremo también está pendiente: aquí se probaron por separado API real e interfaz con transporte simulado.

No se repitieron el workspace completo, el runner formal P051/P122 ni su puerta global. Los recibos formales previos mantienen su fecha y sus conteos históricos. No se declara `REAL_PASS` ni cierre del objetivo global.
