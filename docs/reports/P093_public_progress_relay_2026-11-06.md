# P093 — relé de progreso del worker hacia GET /jobs/{id}

Fecha: 2026-11-06 (noche). Incremento posterior a la [cancelación pública](P093_public_cancellation_2026-11-06.md), siguiente prioridad del STATE. Contrato público sin cambios de superficie: `GET /api/v1/jobs/{id}` ya declarado; su mapa `progress` incorpora la clave `worker` con la última observación del worker durante el intento activo.

## Comportamiento implementado

- **Protocolo**: nueva acción firmada `Action::Progress` del helper nativo — el daemon retira las notificaciones `core.progress` acumuladas del worker; el helper drena un anillo acotado (64) alimentado por la suscripción de broadcast del worker. Observación pura: firma y nonce exigidos, sin IO ni autoridad. El trait `GuardianWorker` expone `drain_progress` (por defecto vacío) y el cliente cualificado añade `progress()`.
- **Consumidor**: durante execute/verify, una tarea propia con cliente independiente sondea cada 400 ms, filtra por los request-id de ESTE intento y persiste la última observación. La tarea se aborta al salir del tramo execute/verify.
- **Persistencia honesta**: `SqlTransformationJobs::observe_progress(job, attempt, observación)` — UPDATE de metadata con guardia `state IN ('RUNNING','VERIFYING') AND attempt=?`; no muta row_version, journal ni eventos; un intento terminado o revisión antigua se rechaza sin escribir. `job()` expone `progress.worker` sólo con el intento activo; los terminales responden con su resultado/publicación. El payload conserva exactamente lo observado (fase, completed_bytes, total_bytes, message_code, observed_at), sin agregación.
- **Defecto descubierto y corregido (namespace worker-side)**: la observación de progreso obliga al worker a comprometer bloques antes de la ventana de caída; el reinicio tras conversión parcial hace que el worker REUTILICE su job (`{id}-{intento+1}`), y el daemon fijaba `bundle_prefix` duro en `-1` → `INTEGRITY_FAILED/transformation_output_outside_attempt`. Ahora el daemon deriva el namespace del manifiesto verificado: un único primer componente `{worker_job_id}-{n}` (n decimal sin ceros) compartido por todos los artefactos — la atadura al recibo del worker se conserva. La auditoría independiente verifica la misma regla con uniformidad.
- Fases observadas del receta de referencia: `TRANSFORM_PREFLIGHT` (input observado, sin bytes) y `TRANSFORM_ENCODING` (bloques duraderos); el test observa la segunda con bytes avanzados.

## Pruebas

- **SQL** (`transformation_plans` 74/74, tres casos nuevos): observación del intento activo expuesta sin efectos colaterales (row_version/journal/eventos intactos, última observación sustituye), intento viejo/terminado/desconocido o payload no-objeto rechazado sin fuga de existencia, y ocultación al actor ajeno.
- **Protocolo** (`guardian-protocol` 20/20): codec firmado de `Action::Progress` y decodificación del resultado.
- **Aceptación nativa**: el flujo público COMPLETED observa ≥2 eventos `TRANSFORM_ENCODING` por `GET /jobs/{id}` (p. ej. 4.980.736/16.777.224 y 7.471.104/16.777.224), y el reinicio tras conversión parcial publica correctamente con el namespace observado.
- **Validación completa**: [attempt5](P093_runs/public-progress-20261106/attempt5/verification-synopsis.json) — **508 pruebas PASS / 0 FAIL / 0 ignoradas, 29/29 comprobaciones**, auditor independiente PASS (893 entradas, 29 logs/recibos). Intentos 1–4 conservados: attempt1 FAIL ambiental (exe bloqueado por un daemon residual durante el rebuild), attempt2/3 con bases de recuento erróneas del auditor (440 vs 441) y attempt3/4 con la aserción antigua `-1`/inicialización del conjunto, corregidos en attempt5.

## Límites conservados

El H07 integral con caída en bytes parciales sigue pendiente como aceptación dedicada (la composición ya sobrevive al caso que este incremento destapó, con namespace observado y publicación CAS correcta). Quedan pendientes la pérdida definitiva del testigo/reinicio del sistema, conversores GGUF/ONNX reales, aceptación de runtimes y UI. P093 sigue `BLOCKED_REAL`/preparación sin `CODE_PASS` integral ni `REAL_PASS`; contrato 3.13.0 intacto.
