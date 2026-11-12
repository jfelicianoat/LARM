# P093 — aceptación dedicada H07: caída con bytes parciales

Fecha: 2026-11-07. Incremento posterior al [relé de progreso](P093_public_progress_relay_2026-11-06.md), siguiente prioridad del STATE. Contrato público sin cambios. Esta aceptación dedicada cubre el hueco que la ADR-0016 reservaba: «esta interrupción no demuestra caída durante bytes parciales de conversión» — ahora demostrada con oráculos propios.

## Lo que demuestra la aceptación (`transformation_publica_h07_caida_con_bytes_parciales_y_reinicio_hasta_publicacion`)

1. **Caída con bytes parciales reales**: tras la confirmación pública, el intento RUNNING convierte con el worker real cualificado P026; el relé de progreso observa `TRANSFORM_ENCODING` con ≥1,5 MiB convertidos y el ledger privado declarado acredita el staging del intento (`state/staging/{worker_job}-1/artifact.b64` con 3,4 MiB ya fsincados y el checkpoint del worker). El daemon se mata DENTRO de la conversión.
2. **Reconciliación automática**: el daemon reiniciado reconcilia el intento como `FAILED_RECOVERABLE` con causa `WORKER_INTERRUPTED`, cierra el intento (attempt_closed) con la revisión observada en la misma fila y libera exactamente sus recursos.
3. **Reinicio del mismo trabajo hasta publicación CAS** (contrato 11: la herramienta no reanudable NO retoma salida incompleta): el plan nuevo confirma y el worker ejecuta un job NUEVO cuyo namespace queda atado a su propio id; la procedencia publicada acredita ese job, distinto del interrumpido. Los bytes publicados se contrastan con la recodificación COMPLETA del original — el provisional parcial del intento 1 no se publica ni se reutiliza.
4. Recuentos finales [1,2,0,0], dos señales de despacho, eventos==outbox, original intacto, principals revocados.

## Ajustes de la prueba durante su desarrollo

- El oráculo provisional apuntó primero a `output/{id}-1` (inexistente: los bloques duraderos viven en el STAGING bajo `state/` hasta la verificación) y luego al staging correcto.
- La revisión del If-Match del reinicio se toma de la MISMA fila que el cierre observado (`attempt_closed` + row_version), evitando una lectura vieja del pool.
- La expectativa de namespace se corrigió de «reutilización `{id}-2`» a «job worker NUEVO `{nuevo_id}-1`» — la reutilización sólo aplica a reintentos worker-side del MISMO plan; un reinicio Core usa plan nuevo y el contrato 11 prohíbe retomar la salida incompleta.

## Validación completa

[attempt3](P093_runs/public-h07-20261107/attempt3/verification-synopsis.json) — **509 pruebas PASS / 0 FAIL / 0 ignoradas, 30/30 comprobaciones** (la 30ª nueva: public-h07), auditor independiente PASS (894 entradas, 30 logs/recibos). El auditor verifica el informe H07 (bytes parciales > 0, WORKER_INTERRUPTED, job del worker nuevo distinto del interrumpido, recuentos). Attempts 1–2 conservados: attempt1 FAIL ambiental (exe bloqueado por un daemon residual durante el rebuild) y attempt2 con el informe sin la clave `restarted_worker_job`, añadida en attempt3.

## Límites conservados

Quedan pendientes: la pérdida definitiva del testigo/reinicio del sistema, conversores GGUF/ONNX reales, aceptación de runtimes y UI. P093 sigue `BLOCKED_REAL`/preparación sin `CODE_PASS` integral ni `REAL_PASS`; contrato 3.13.0 intacto.
