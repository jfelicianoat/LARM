# P045 — Autenticación de fuentes y CredentialReference

**Estado: `BLOCKED_REAL`.** La administración de cuentas y licencias está conectada a dominio, SQLite, API, OpenAPI y daemon. El probe source ya acepta jobs durables e idempotentes y ejecuta `source.auth_probe` mediante la cualificación vigente para fuentes públicas `NONE`. El flujo no crea credenciales: sólo maneja referencias. La resolución de credenciales privadas y su cualificación real siguen pendientes.

## Implementado

- DTOs cerrados y validación de nombres, UUIDv7, hashes SHA-256 y referencias de credencial ligadas a una fuente.
- Alta, consulta, listado paginado y borrado de cuentas. La cuenta seleccionada no se puede borrar; referencias cruzadas entre fuentes se rechazan.
- Aceptación y revocación de licencia con evento outbox y recibo idempotente en la misma transacción SQLite. Se verifican replay, conflicto por cuerpo distinto y rollback cuando falla el outbox.
- Rutas de cuentas y licencia con scopes, cursor firmado, `Idempotency-Key`, respuestas sin secretos y operaciones marcadas como implementadas en OpenAPI.
- Composición del daemon conectada al repositorio SQLite.
- `POST /api/v1/sources/{id}/probe` con `202`, validación estricta, `Idempotency-Key` y journal SQLite con replay, aislamiento por actor, bloqueo de jobs simultáneos por fuente, snapshot de versión, outbox y fallos redactados.
- Supervisor durable con control de propiedad de jobs, ejecución bajo la cualificación P026 vigente, permisos limitados a los orígenes declarados o a la carpeta local y drenaje del worker antes del estado terminal.
- Para fuentes públicas `NONE`, el daemon invoca `source.auth_probe`; referencias privadas no llegan al worker y fallan cerradas hasta P141.

## Verificación

- Evidencia focal: 6 PASS, 0 FAIL y 2 BLOCKED. Incluye formato, DTO, integración SQLite, scopes/replay de API, publicación OpenAPI y composición del daemon.
- Verificación incremental del probe (2026-10-24): 11/11 pruebas focales pasan entre dominio (2), SQLite (2), API (2), política del daemon (3), supervisor (1) y llamada `source.auth_probe` desde un worker real de fixture (1); `cargo fmt --all -- --check` y Clippy `--all-targets -- -D warnings` pasan. El test del worker se volvió a comprobar con Clippy después de añadirlo.
- Suite focal SQLite de fuentes: 17/17; incluye persistencia de cuentas, licencia, referencias cruzadas y rollback transaccional. Evidencia validada con `tools/validate_evidence.py`.
- Gate oficial del checkpoint 2026-10-23: 14/14 PASS. Formato, Clippy, fixtures, suite serial del workspace, arquitectura, 57 copias del paquete, Tauri y lint/typecheck/tests/build del frontend pasan.
- El primer gate sin permisos revisados falló en 3 de las 7 pruebas `declarative` con `health_unreachable`; la repetición aislada con acceso local revisado pasó 7/7 y la corrida oficial completa pasó. El código de P045 no se modificó para corregirlo.

## Pendiente

- Actualización P141 (2026-10-24): el probe Basic del perfil HTTP P048 ya valida y resuelve la cuenta/referencia en el host y lee el secreto en Credential Manager dentro del worker, bajo el permiso exacto. OAuth, SSH, cookies, Git LFS y rotación/revocación siguen pendientes; véase el [informe P141](P141_autenticacion-privada-oauth-ssh-cookies-y-git-lfs.md).
- Revalidar contra una fuente privada y cuenta autorizadas, y obtener `REAL_PASS` de P003, P043 y P009; hasta entonces P045 conserva `BLOCKED_REAL`.

La evidencia formal enlazada y el [resultado de la puerta común](check_P045_real.json) son el checkpoint histórico del 2026-10-23; la verificación incremental del probe indicada arriba corresponde al 2026-10-24.
