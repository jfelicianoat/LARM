# P093 — Transformation Pipeline framework

Fecha: 2026-10-23  
Estado: BLOCKED_REAL

P093 añade el contrato de dominio para planes y trabajos de transformación, DTOs estrictos y rutas API para crear planes, confirmar ejecuciones con idempotencia y consultar jobs. SQLite persiste actor, correlación, request, plan sellado y hashes de entrada. Al arrancar, la recuperación convierte interrupciones RUNNING/PAUSING/VERIFYING en FAILED_RECOVERABLE junto con causa, journal, evento y outbox dentro de una transacción; no publica un output parcial. La migración 0008 habilita el estado y su índice de recuperación. OpenAPI publica las tres operaciones.

El grafo de crates publicado permanece intacto: DTOs e invariantes de transformation viven en `larm-domain`, su API usa el puerto de aplicación del dominio, y `larm-api`/`larm-db` no añaden dependencias internas nuevas. La revisión del código y las pruebas muestran un límite funcional pendiente: el repositorio todavía no conecta el worker al preflight de espacio pico, guards/reservas, staging ni verificación de artefactos. Por ello las escrituras devuelven UPSTREAM_UNAVAILABLE antes de crear filas. Tampoco está implementado el reinicio durable de FAILED_RECOVERABLE bajo un plan y preflight nuevos. El adaptador de referencia protegido sí ejecuta un proceso local, rechaza accesos fuera de permisos y verifica el bundle; esa prueba no sustituye el job público del daemon.

La evidencia focal contiene 9 pruebas PASS, 0 FAIL y 2 aceptaciones reales bloqueadas. Incluye tests del dominio, API, recuperación SQLite, migración, conformance del adaptador protegido, compilación del daemon, coherencia de OpenAPI y arquitectura. La puerta completa `tools/check.ps1 -Prompt P093 -Real` terminó **PASS (14/14 checks)**: fmt, Clippy, fixtures, workspace, arquitectura, copia contractual, Tauri y frontend; sus logs están enlazados en [check_P093_real.json](check_P093_real.json).

Pendientes para cerrar la fase:

- Conectar worker y preflight peak-disk por volumen con input inmutable, herramienta y parámetros fijados, guards/reservas y provenance.
- Implementar verificación de hashes y smoke/load antes de registrar outputs; conservar el original por defecto.
- Implementar reinicio desde FAILED_RECOVERABLE con input íntegro, nuevo preflight, ownership de temporales, attempt incrementado y trazabilidad anterior.
- Ejecutar en Windows los oráculos reales de disco insuficiente, crash, hash mismatch y recovery/restart a través de jobs del daemon.
- Obtener `REAL_PASS` de P016, P040 y P051, requerido por el manifiesto y el contrato 00.

Las pruebas, sus salidas y hashes de entrada constan en [la evidencia contractual](P093_transformation-pipeline-framework.evidence.json). La matriz de dependencias impide completar P093 hasta cumplir esas puertas.
