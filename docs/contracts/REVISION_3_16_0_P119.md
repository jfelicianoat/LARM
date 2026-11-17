# Revisión contractual 3.16.0 — admisión autorizada de descargas automáticas (P119)

Fecha: 2026-11-16. Predecesora:3.15.0. Publicación e implementación autorizadas expresamente por el operador al responder «Autorizar revisión e implementación» a la propuesta3.16.0. Contrato normativo; no acredita por sí mismo implementación ni REAL_PASS.

## Problema comprobado

La especificación §121/P119 exige descargas automáticas autorizadas: política opt-in del operador (allowlist fuente/capability/license/trust, cuota diaria de bytes, destino de almacenamiento, aprobación versionada), watcher que por defecto sólo notifica, auto-job con prioridad BACKGROUND que revalida contra la política vigente al activarse (suspende si cambia), historial y revocación inmediata de futuras admisiones. El watcher actual rechaza `policy.automatic_download=true` con `watch_auto_download_not_implemented`: no existe el modelo de política ni su historial ni las operaciones API, y las descargas automáticas no pueden entrar como BACKGROUND dentro del límite compartido de Settings sin evade los presupuestos.

Fuentes: `LARM_SPEC_V1.md` §121 y H08; `contracts/02_STORAGE_DOWNLOAD.md`; `contracts/07_API_EVENTS_MCP.md`; `contracts/api-operations.json`; `schemas/domain.schema.json`. El contrato00 exige resolver con el operador los cambios de alcance, conservando los requisitos.

## DTOs nuevos y reglas normativas

```json
{
  "download_admission_policy": {
    "enabled": false,
    "source_allowlist": ["source-uuid"],
    "capability_allowlist": ["capability-id"],
    "license_allowlist": ["license-id"],
    "min_trust": "CANDIDATE",
    "daily_quota_bytes": "1073741824",
    "storage_location_id": "uuid|null"
  }
}
```

- `min_trust` enumera los estados de `Trust` (CANDIDATE/TECHNICALLY_APPROVED/EVALUATION_APPROVED); la confianza de la variante debe alcanzarlo o superarlo.
- `download_admission`: registro inmutable del resultado con `{id, created_at, updated_at, row_version, metadata, variant_id, source_id, estimated_bytes, decision: ADMITTED|BLOCKED|REVOKED, reason, policy_row_version, day_key, job_id}`. `day_key` es la fecha UTC del intento (cuota diaria). Un registro REVOKED anula admisiones futuras del mismo `variant_id` mientras siga vigente.

Reglas:

1. **Opt-in**: sin política `enabled=true` ninguna detección produce descarga; el watcher sólo notifica.
2. **Evaluación determinista antes de planear**: source en allowlist, capability en allowlist, license en allowlist (o variant sin license si la allowlist está vacía), trust ≥ min_trust y cuota diaria no agotada; cualquier fallo produce admisión BLOCKED con motivo estable y SIN job.
3. **Auto-job BACKGROUND**: la descarga admitida entra como BACKGROUND (nunca evita presupuestos/allowlists eligiendo prioridad) dentro del límite compartido de Settings; reserva y revalidación de tamaño/trust/license/revisión se aplican igual que una descarga manual.
4. **Clausura de dependencias**: `REAL_PASS` exige que las dependencias del manifiesto estén en `REAL_PASS`; violación → `closure_incomplete::<id>`.
5. **Evidencia huérfana**: cualquier fichero `*.evidence.json` cuyo prompt no esté en `required_prompt_ids` falla (`evidence_orphan`).
6. **Evidencia ilegible**: JSON inválido → `evidence_unreadable`.

## Evidencia de implementación (anexa, no contractual)

- Dominio: evaluador puro con las seis reglas y unit tests por regla.
- DB: migración 0016 con tabla singleton de política versionada, historial inmutable con day_key, triggers de revocación y outbox; repositorio con journal/evento/outbox en una transacción.
- API: PUT/GET de política y GET/POST de admisiones con scopes contractuales.
- Watcher: detección → evaluación → BACKGROUND dentro de cuota; sin política sólo notifica.
- Real en PC IA: fuente controlada con revisión compatible → notificación sin descarga; cuota pequeña autorizada → admisión y bloqueo por cuota.
