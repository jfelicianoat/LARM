# P051 — diagnóstico del reinicio y del journal

Fecha: 2026-10-30. Regresión común detectada durante el gate Rust de P092; no cambia contratos ni código de producción.

El primer gate general falla en `proceso_real_reinicia_api_y_recupera_descarga_running_con_checkpoint` (3/4 casos de lifecycle pasan). La prueba esperaba FAILED, pero sembraba una descarga RUNNING sin la fila `operation_journal` asociada. El cierre actual exige actualizar job, journal, reserva y outbox juntos. El fixture había quedado desajustado respecto de esa condición, ya cubierta por las pruebas SQLite de cierre y rollback.

## Diagnóstico conservado

- [Gate original](P092_runs/candidate-composer-20261030/official-rust-gate.command.json): exit1, inputs sin cambios; cargo_test exit101.
- [Repetición restringida](P092_runs/candidate-composer-20261030/lifecycle-before.log): no conecta al servidor local. [Diagnóstico independiente](P092_runs/candidate-composer-20261030/startup-diagnostic/receipt.json): WinError10013, daemon vivo y escuchando; proceso propio recogido al terminar. No se atribuye este resultado al producto.
- [Repetición con conexión local](P092_runs/candidate-composer-20261030/lifecycle-local-loopback.log): reproduce el fallo de recuperación del gate.
- [Registros ampliados](P092_runs/candidate-composer-20261030/lifecycle-diagnostic.log): checkpoint recuperado, attempt2, estado RUNNING, error `download_failure_journal_conflict`. Por tanto, sí se recuperaba la descarga; fallaba el cierre atómico al faltar su journal.

## Corrección del fixture y oráculos

`apps/daemon/tests/lifecycle.rs` mantiene dos arranques de procesos reales y comprueba el checkpoint durable de4 bytes y un único evento de recuperación emparejado con outbox. Ahora diferencia dos escenarios:

1. Journal asociado existente, con snapshot de plan ausente: el worker publica `download_plan_invalid`, job/journal/execution quedan FAILED y se libera la reserva. El fixture conserva la inyección de un plan no válido; no convierte ese plan en ejecutable.
2. Journal completamente ausente, escenario original conservado como prueba de corrupción: la recuperación y el claim conservan el checkpoint, el supervisor registra el conflicto y no se publica un cierre parcial ni se libera la reserva. No se afirma que exista reparación automática de un journal perdido; esa intervención sigue pendiente.

Los límites de espera no se amplían. Se captura el log de arranque reiniciado y el último estado observado para que un fallo futuro tenga diagnóstico útil.

[Lifecycle completo](P092_runs/candidate-composer-20261030/lifecycle-journal-green.log): **5/5 PASS**, exit0, 2,89s; incluye configuración inválida, ayuda y ambos escenarios de reinicio. [Clippy focal](P092_runs/candidate-composer-20261030/review-clippy.log): PASS. Los avisos de copia del caché incremental son del sistema de archivos; no se suprime ningún diagnóstico de código.

La nueva ejecución general termina PASS7/7 en [reviewed-composer-20261030](P092_runs/reviewed-composer-20261030/official-rust-gate.command.json): workspace1443PASS/0FAIL y6 aceptaciones optativas ignoradas, inputs20/20 intactos y transcripciones contrastadas. [Resumen y exclusiones](P092_runs/reviewed-composer-20261030/verification-synopsis.json). Estos casos locales no cualifican una fuente upstream ni sustituyen la aceptación integral P051/P092.
