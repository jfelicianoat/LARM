# Mantenimiento del almacenamiento (P018)

## Discos que desaparecen y vuelven (spec §29, §135)

La reconciliación se ejecuta al arrancar el daemon, cada 5 minutos y a mano con `larm-daemon storage-reconcile`. Lee el estado de cada ubicación, que viene del último sondeo (P012/P014).

| Ubicación | Sus `artifact_location` | Variante |
|---|---|---|
| `OFFLINE` / `ERROR` (disco desconectado) | `OFFLINE` | `availability` `OFFLINE`; guarda la anterior en `metadata.availability_before_offline` |
| `MISSING` (volumen presente, carpeta raíz ausente) | `MISSING` | `OFFLINE` |
| Vuelve a estar en línea | Se **releen los bytes** de cada fila que no estaba `ONLINE`: mismo sha256 → `ONLINE` con `verified_at`; otro → `CORRUPT`; sin fichero → `MISSING` | Recupera su disponibilidad anterior solo si todos sus artefactos requeridos están `ONLINE` y releídos. Un requerido `CORRUPT` la deja `CORRUPT` |

Nunca se borra un registro ni se cambia la etapa del ciclo de vida. `OFFLINE_STORAGE` de la spec es solo un alias de lectura de `OFFLINE`.

## Niveles de disco (H09)

`levels_by_volume` calcula NORMAL, WARNING o CRITICAL con una sola entrada por volumen físico, aunque tenga varias raíces. Usa la observación más reciente, en modo PERCENT o BYTES, y la igualdad no cruza el umbral. `level_transitions` devuelve los volúmenes que cambiaron de nivel, para alertar una vez por volumen. La presentación y las notificaciones son de P128/P140.

## Análisis (`GET /api/v1/storage-locations/{id}/analysis`, scope `read`)

| Categoría | Qué cuenta | Recuperable |
|---|---|---|
| `MODELS` | Ficheros registrados en `artifact_location`, vistas y manifiestos | Las copias de más del mismo sha256, solo si todas son `LARM_MANAGED`. Si no, `duplicate_not_larm_managed` |
| `PARTIALS` | `staging/<job>/*.partial` | Si su job ya terminó. Si sigue vivo, `job_active` |
| `TEMPORARIES` | Otros ficheros de `staging/` y `tmp/` | Igual que las parciales |
| `CACHES` | `cache/` | Sí, con plan aprobado |
| `RUNTIMES` | `runtimes/` | Desconocido: lo decide el registro de runtimes |
| `ORPHANS` | Blobs del CAS sin registro y ficheros sueltos | Solo los blobs. Lo demás es del usuario (`not_owned_by_larm`) y nunca se toca |

## Limpieza

1. `POST /storage-locations/{id}/cleanup-plan` (`storage:write`, `Probe` con `resource_id` y `profile_id` a `null`) devuelve un plan con todo lo recuperable. `approval_required` siempre es `true`.
2. `POST /approvals` (`admin`) con `operation_digest` igual al `plan_hash` y los `resource_ids` del plan. La aprueba el mismo actor que va a ejecutar.
3. `POST /storage-locations/{id}/cleanup` con `{plan_id, plan_hash, approval_id}` devuelve 202. Sin aprobación válida, `APPROVAL_REQUIRED`. El job vuelve a analizar y, antes de cada borrado, comprueba que el elemento sigue igual y sin registrar. Lo que cambió se salta y queda en el resultado.

## Borrar un modelo

1. `POST /models/{id}/delete-plan` (`models:write`, `Idempotency-Key`; `{id}` es la variante). Decide por fichero:

   | Paso | Cuándo |
   |---|---|
   | `DELETE_FILE` | `LARM_MANAGED` y ninguna otra referencia en esa ruta |
   | `KEEP_SHARED` | Blob compartido (`SHARED` u otra fila en la misma ruta): solo se retira esta referencia |
   | `DEREGISTER` | `EXTERNAL` o `HOST_MANAGED`: el fichero se queda donde está |

2. Aprobación del plan (como en la limpieza).
3. `DELETE /models/{id}?plan_hash=…&approval_id=…` con `If-Match` (la `row_version` de la variante) devuelve 204:
   - toma los locks exclusivos; una lease activa es `ACTIVE_LEASE`;
   - borra el registro en una transacción. Si otra cosa usa la variante (instancias, compatibilidad…), es `CONFLICT` `variant_in_use` y no se borra nada;
   - después borra los ficheros y su manifiesto. Un fichero que no se pueda borrar queda como huérfano recuperable.

   La aprobación se consume aunque la operación falle después, porque es de un solo uso.

## Reconstruir el registro (spec §134)

```
larm-daemon registry-rebuild --root <raíz> [--root …] --out staging.sqlite
larm-daemon --data-root <datos> db-restore --from staging.sqlite
```

La importación en modo COPY deja `root/manifests/<variant_id>.json` con:

- el modelo, la distribución y la revisión;
- la variante;
- cada artefacto con su rol, su sha256, su tamaño y su blob.

La reconstrucción:

- lee esos manifiestos y **relee los bytes** de cada blob;
- registra en una base de **staging nueva** las variantes completas;
- informa de las incompletas (blob ausente o corrupto), los manifiestos ilegibles y los blobs que nadie cita;
- recrea las fuentes citadas como marcadores deshabilitados, sin inventar su configuración ni su licencia.

No escribe en las raíces ni toca la base viva. La fusión es un `db-restore` verificado, que conserva la base anterior y también funciona en una instalación nueva sin carpeta de datos.
