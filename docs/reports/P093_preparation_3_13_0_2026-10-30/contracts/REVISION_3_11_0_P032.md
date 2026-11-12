# Revisión de contrato LARM 3.11.0 — actualización y pinning de runtime P032

Fecha: 2026-10-25. Predecesora: 3.10.2. Ámbito: instalación de candidatos side-by-side, ciclo de vida de versiones runtime, promoción gobernada por comparación P117, rollback, pins por variante/perfil y retención segura.

## Identidad y ciclo de vida

- `RuntimeVersion.lifecycle` es un campo tipado e independiente de `promotion`. En observaciones de adapters legacy puede omitirse; el host lo inicializa como `CANDIDATE` y nunca acepta del adapter una transición de lifecycle. En respuestas de LARM siempre se serializa: `CANDIDATE`, `INSTALLING`, `VALIDATING`, `VALIDATED`, `DEFAULT`, `RETAINED`, `BROKEN` o `REJECTED`. `DEFAULT` coincide con `runtime.default_version_id`; cambiar el default actualiza ambos en una transacción.
- Una actualización nunca modifica bytes, instalación o alias del default en sitio. Un candidato se instala en una ubicación side-by-side mediante el admission durable de P031. Un fallo conserva default, instalación y evidencia anteriores; no asciende el candidato por estado `READY`/health solamente.
- `VALIDATED` exige instalación completa, load, health, smoke del adapter y cualificación del protocolo vigentes para esa instalación. La promoción requiere además una comparación P117 `PASS`, licencia/trust aprobados, referencias íntegras y ausencia de gates desconocidos. `FAIL`, `INCONCLUSIVE`, `INCOMPARABLE`, inexistente, obsoleto o de otro candidato bloquea el cambio de default.

## Solicitudes API

- `POST /api/v1/runtime-versions/{id}/update` acepta el objeto cerrado `RuntimeUpdateRequest`: `storage_location_id`, `strategy`, `profile_lock_hash` y `external_host_id` nullable. El path identifica una `RuntimeVersion` catalogada; el host resuelve de SQLite su `runtime_id` y `version_string`, valida que el canal esté permitido y crea una instalación side-by-side con el servicio/job de P031. El body no puede cambiar el runtime, la versión o el artefacto resuelto por el path. Respuesta `JobReceipt`, HTTP 202 e `Idempotency-Key`.
- `POST /api/v1/runtime-versions/{id}/rollback` acepta `RuntimeRollbackRequest` con `expected_default_version_id` UUIDv7 nullable. El path identifica el destino `VALIDATED` o `RETAINED`; el destino y el default observado deben pertenecer al mismo runtime. Una comparación concurrente devuelve 409. Cambia el default atómicamente, conserva el candidato fallido y deja finalizar las leases existentes; las nuevas resoluciones usan el destino.
- `POST /api/v1/runtime-versions/{id}/pin` acepta `RuntimePinRequest` cerrado con `variant_id` y `launch_profile_id`. Sólo fija una versión `VALIDATED`, `DEFAULT` o `RETAINED` compatible con ambos IDs. La clave única es `(variant_id, launch_profile_id)`. Nuevas leases de esa pareja resuelven la versión fijada incluso tras cambiar el default. Un pin inválido, incompatible o roto falla cerrado; no hay fallback silencioso al default.
- `DELETE /api/v1/runtime-pins/{variant_id}/{launch_profile_id}` retira la fijación para las nuevas resoluciones y no interrumpe leases existentes. Todos los comandos mutables son idempotentes, autenticados por `runtimes:write` y escriben journal, evento y outbox junto con el estado.
- `RuntimeUpdateRequest`, `RuntimeRollbackRequest` y `RuntimePinRequest` son `additionalProperties:false`. Las rutas de update, rollback y pin conservan sus códigos de éxito y recibos actuales; promoción sigue siendo operación P117.

## Comparación P117 consumible

- `POST /api/v1/comparisons` conserva su `Compare` y `CommandReceipt`. El `resource_id` identifica una comparación durable; `GET /api/v1/comparisons/{id}` devuelve `ComparisonResult` cerrado con los benchmark IDs, eje, fingerprints, candidato/baseline cuando el eje es `runtime_version`, `outcome` (`PASS`, `FAIL`, `INCONCLUSIVE`, `INCOMPARABLE`), `differing_components`, `limitations` y referencias de evidencia.
- La comparación es inmutable. P032 sólo admite una comparación cuyo eje, candidato, baseline y runtime coincidan con la solicitud de promoción y cuyo outcome sea `PASS`; no recalcula ni infiere evidencia.

## Pins y persistencia

- `runtime_version_pin` almacena `variant_id`, `launch_profile_id`, `runtime_version_id`, timestamps, `row_version` y metadatos; `variant_id` + `launch_profile_id` forman la clave primaria y las tres relaciones tienen claves foráneas restrictivas.
- El resolver de leases comprueba primero el pin exacto variante/perfil y luego el default. Si la pareja tiene pin, la elección es estable; si ese destino no está cualificado, la lease se rechaza explícitamente.
- `benchmark_comparison` almacena `ComparisonResult` y vincula benchmarks, versiones y referencias por claves foráneas. Sólo el servicio P117 crea resultados.

## Retención

- `Runtime.retention_policy.keep_last_validated` mantiene como mínimo 2 y como máximo 50 versiones validadas; el default es 3. La política se puede cambiar mediante `PATCH /api/v1/runtimes/{id}/retention-policy`, con `If-Match` y `runtimes:write`.
- Después de una actualización validada/promoción, el worker conserva las N versiones validadas más recientes y, adicionalmente, toda versión default, fijada, ligada a benchmark/comparación/certificación, o con lease activo. No elimina instalaciones `ADOPTED` o `EXTERNAL`.
- La retirada y limpieza de instalaciones administradas es durable, idempotente y recuperable. Antes del borrado físico revalida referencias, ownership, identidad de ubicación y ausencia de leases; si la comprobación falla o la IO falla, conserva la versión y registra la causa. Nunca se borra una versión que cumpla cualquiera de las protecciones anteriores.

## Migración y compatibilidad

- La migración local `0013_p032_runtime_version_pinning_and_retention.sql` añade ciclo de vida y política de retención, crea pins y comparaciones con claves foráneas restrictivas e inicializa filas existentes sin alterar `runtime.default_version_id`.
- DTOs de runtime incluyen `lifecycle` y `retention_policy`; consumidores estrictos deben actualizar sus schemas juntos. La nueva lectura de comparación no expone payloads upstream ni secretos.

## Oráculos mínimos

- Una actualización que falla antes o después de instalar no cambia default ni elimina la última instalación válida.
- Una pin de variante/perfil sobrevive a promoción, rollback y cambio del default, y sólo se retira mediante el comando explícito.
- Rollback atómico permite nuevas leases en la versión anterior y mantiene vivas las leases anteriores hasta su cierre.
- La retención nunca elimina default, pin, benchmark/comparación/certificación, lease activo ni instalación externa/adoptada; un fallo/crash de cleanup se recupera sin borrar referencias.
- P117 `FAIL`, `INCONCLUSIVE`, `INCOMPARABLE`, evidencia ausente y fingerprints incompatibles nunca cambian el default.
