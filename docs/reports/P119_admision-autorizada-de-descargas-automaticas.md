# P119 — Admisión autorizada de descargas automáticas

Fecha: 2026-11-16.  
Estado: **BLOCKED_REAL** — la política opt-in, el evaluador determinista, la persistencia con cuota diaria, la API contratada y el flujo REAL sobre SQLite migrado están completos y PASS; el auto-job BACKGROUND end-to-end desde el watcher exige el plan preview real de la fuente (P044 en BLOCKED_REAL) y locks cualificados (P099/P031).

## Revisión contractual 3.16.0 (autorizada por el operador)

Los DTOs de la política opt-in y el historial no existían en el esquema sellado: documento `contracts/REVISION_3_16_0_P119.md` con los DTOs `download_admission_policy` (enabled, allowlists de source/capability/license, min_trust, cuota diaria bytes, storage target) y `download_admission` (historial inmutable con day_key, policy_row_version y job_id), las **4 operaciones** (`P119_put/get__download_admission_policy`, `P119_get__download_admissions`, `P119_post__download_admissions_id_revoke`) y las seis reglas normativas (opt-in, evaluación determinista antes de planear, auto-job BACKGROUND sin evadir presupuestos, revalidación al activarse, historial inmutable con day_key, revocación inmediata). `package_copy.py` → **82 copias idénticas al paquete 3.16.0**.

## Implementación

- **Evaluador puro** (`crates/larm-domain/src/download_admission.rs`): `evaluate_admission` con las seis reglas — opt-in apagado (`admission_policy_disabled`), allowlists de source/capability separadas, license (presente fuera de allowlist → `admission_license_not_allowlisted`; ausente con allowlist → `admission_license_missing`), trust por escalafón (`admission_trust_below_minimum`; desconocido es error), cuota diaria agregada (`admission_quota_exhausted`) — y validación cerrada de la política (min_trust del escalafón, cuota decimal positiva). Las admitidas entran siempre **BACKGROUND**.
- **Persistencia** (`migrations/0016_p119_download_admission.sql`, `crates/larm-db/src/download_admissions.rs`): tabla singleton de política versionada (`row_version+1` por PUT = aprobación versionada), historial inmutable con triggers (UPDATE sólo hacia REVOKED; DELETE prohibido), índice por day_key; repositorio con `put_policy` (journal `download.admission_policy` + evento + outbox), `admit` (evaluación con cuota agregada por day_key dentro de la transacción + journal/evento/outbox), `revoke` (REVOKED inmediato con journal/evento/outbox), `list` y `is_revoked` para la revalidación.
- **API contratada** (`crates/larm-api/src/download_admissions.rs`, montada en `serve.rs`): PUT/GET de política (If-Match → `admission_policy_stale`), GET de admisiones paginado, POST revoke; gates de cobertura del catálogo **155→159**.
- **Watcher**: `automatic_download=true` ahora es válido en `SourceWatchCreate.policy` (el rechazo `watch_auto_download_not_implemented` se retira); el destino de la política es obligatorio para watchers automáticos (`watch_auto_download_target_missing`). La evaluación de detección→admisión usa el evaluador del dominio; sin política habilitada sólo notifica (comportamiento por defecto conservado).

## Evidencia real (PC IA, 2026-11-16)

`apps/daemon/tests/download_admission.rs::flujo_real_notificacion_cuota_pequena_admision_y_bloqueo` **PASS** sobre SQLite migrado (0016): sin política → `policy_disabled` (sólo notifica); política autorizada con cuota de **1000 bytes** → primera y segunda descargas de 400 **ADMITTED** (BACKGROUND), tercera → **BLOCKED `admission_quota_exhausted`** sin job; revocación inmediata → REVOKED con `is_revoked=true`; la política y el historial **persisten tras re-open** de la base. Evidence formal: **BLOCKED_REAL (4 PASS, 0 FAIL, 1 BLOCKED)** — el bloqueo restante es el auto-job end-to-end desde el watcher (plan preview real P044 + locks cualificados).

## Defectos corregidos durante la fase

1. Placeholders del INSERT de política desalineados (10 columnas vs 9/11 valores) — alineados a `singleton` literal + 9 binds con `row_version` literal y `updated_at` bind.
2. CHECK de `daily_quota_bytes` con `GLOB '*[^0-9]*' IS NULL` (siempre falso sobre TEXT) — sustituido por límite de longitud; la regla de dígitos la aplica el dominio.
3. `used_bytes_on` decodificaba el INTEGER de `sum()` como `Option<String>` — corregido a `i64` con clamping a u64.
4. Asertos de cuota mal calculados (la cuota 1000 admite 2×400, no 3×400) y marcadores de log con/sin acento en el evidence runner.

## Nota sobre el gate del workspace

`tools/check.ps1 -Prompt P119 -Real` reporta 3 fallos **no atribuibles a P119**, confirmados así: `transformation_suite_nativa` falla con `adapter_probe_failed/probe_stdout_closed` **también en HEAD sin cambios** (git stash verificado: mismo fallo en el worker de transformación de referencia — fallo ambiental del host tras las corridas prolongadas); `python_tests` falla por el mismo subyacente; `package_copy` reportaba copias desincronizadas durante la corrida con el stash de trabajo intermedio (la copia restaurada al paquete 3.16.0 pasa: **PASS 82 copias**). Las puertas propias de P119 (evaluador 8/8, admisiones SQL 4/4, API 38/38, flujo REAL) están PASS.

## Verificación

- Dominio 8/8; SQL 4/4 (más DB completa 378/0); API compila y rutas montadas en `serve.rs`; REAL PASS con salidas impresas; clippy workspace -D warnings, fmt, **82 copias 3.16.0**, gates de catálogo **159** operaciones.
