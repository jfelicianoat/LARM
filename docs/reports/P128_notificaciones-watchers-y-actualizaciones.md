# P128 — Notificaciones, watchers y actualizaciones

Estado: `BLOCKED_REAL`.

P128 conecta la configuración de seguimientos con su API, persistencia y experiencia de escritorio. Los técnicos pueden crear, editar, pausar y eliminar watchers por fuente, consulta, capacidad, compatibilidad y tamaño; las reglas distinguen tamaño exacto de estimado o desconocido. Las rutas de notificaciones y watchers quedan publicadas como implementadas en OpenAPI. La API valida el alcance del actor, usa DTOs cerrados, idempotencia y `If-Match` para cambios concurrentes.

El centro de notificaciones presenta avisos persistidos, permite marcar como leídos o descartarlos y resuelve enlaces locales a modelos, runtimes y almacenamiento. La tarjeta de actualizaciones muestra candidatos como propuestas que el técnico debe revisar; no promueve runtimes automáticamente.

H09 aplica umbrales editables `PERCENT` o `BYTES` por volumen físico. La migración `0012_p128_low_disk_transition.sql` conserva el último nivel y la revisión de Settings por `volume_key`. El refresco compara límites inclusivos, evita repetir avisos mientras el nivel no cambie y escribe el estado de transición, la notificación y el outbox en la misma transacción. Una observación offline o desconocida no crea un estado ni una alerta.

La evidencia de P128 registra **12 PASS, 0 FAIL y 1 BLOCKED**; `validate_evidence.py` la valida. El runner comprueba schemas de las 18 capacidades (36 schemas), los descriptores derivados, la validación y selección de consultas, CRUD de watchers, scopes y CAS, rutas OpenAPI, API y persistencia de notificaciones, los umbrales y la revisión H09, los enlaces y comportamientos del escritorio, y 71 copias idénticas del paquete 3.10.2. La puerta oficial final pasa **14/14**: formato, Clippy, fixtures, workspace Rust (**1.236 tests en 123 binaries**), arquitectura, paquete, Tauri y frontend. La interfaz pasó lint, typecheck, build y **111/111 tests en 16 archivos**.

La suite completa también detectó expectativas antiguas del catálogo de migraciones y del número de tablas del DDL tras P127/P128; se actualizaron y sus suites `larm-db` pasan 18/18 y 9/9. El test del worker confirma la liberación del ID después de vaciar la respuesta, con cinco ejecuciones consecutivas. Las pruebas locales no sustituyen la aceptación integrada en Windows/PC IA.

La aceptación obligatoria sobre datos y ubicaciones de prueba en Windows/PC IA permanece sin ejecutar. P050 está `BLOCKED_REAL`; P113, P032, P097 y P119 siguen `NOT_STARTED`. Conforme al contrato 00, P128 debe permanecer `BLOCKED_REAL` hasta que esas dependencias alcancen `REAL_PASS` y se registre la prueba integrada. No se afirma `REAL_PASS`.

La [puerta oficial final](check_P128_real.json), la [evidencia PromptEvidence](P128_notificaciones-watchers-y-actualizaciones.evidence.json) y sus transcripciones están disponibles en este directorio.
