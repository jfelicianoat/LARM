# Mover modelos entre ubicaciones (P016)

Un artefacto se registra como `storage_location_id` + `relative_path` en `artifact_location`, nunca como una ruta absoluta. La ruta real se reconstruye en cada uso desde la ubicación y el volumen montado en ese momento.

## Resolver la ruta

| Ubicación | Raíz actual |
|---|---|
| Ruta de red (UNC) | La ruta canónica: su identidad es el propio recurso compartido |
| Volumen local con GUID y la misma letra | La ruta canónica |
| Volumen local con la letra cambiada | La ruta dentro del volumen (`metadata.volume_relative_path`, que se guarda al dar de alta la ubicación) bajo el punto de montaje actual |
| Volumen local sin letra | `\\?\Volume{GUID}\…` |
| Volumen desconectado | No se inventa ninguna ruta: `RESOURCE_UNAVAILABLE` `volume_offline` |

Una ruta relativa no puede ser absoluta, llevar unidad o `:` (flujos alternativos), contener `.`, `..` o componentes vacíos, acabar en punto o espacio, ni usar nombres reservados de Windows.

## Plan y ejecución

`POST /api/v1/models/{id}/move` (`storage:write`, `Idempotency-Key`). El `{id}` es el UUID de la **variante**, no el del grupo `CanonicalModel`.

1. **Plan** (`dry_run: true`): devuelve un `Plan` (201) y lo guarda como `PLANNED` hasta `expires_at` (900 s). Cada artefacto recibe una acción:

   | Paso | Cuándo |
   |---|---|
   | `RENAME` | Origen y destino en el mismo volumen: renombrado atómico |
   | `COPY` | Otro volumen: copia verificada |
   | `KEEP` | Se queda donde está. Motivos: `external_preserved`, `host_managed_preserved`, `shared_preserved` (propiedad `SHARED` o blob usado por otra variante) y `already_there` |

   El plan comprueba en disco que cada origen existe con su tamaño registrado y que el destino está libre. Incluye la estimación de espacio de P015 (solo las copias ocupan espacio nuevo) y los recursos que bloqueará. No expone rutas absolutas.

2. **Ejecución** (`dry_run: false` con el `plan_hash` del plan):
   - se recalcula el plan y tiene que dar el mismo hash; si no, `STALE_REVISION` `plan_changed` y el plan pasa a `CANCELLED`;
   - se toman los locks del guard (volúmenes compartidos; variante y artefactos exclusivos) junto con la fase `LOCKED`, en la misma transacción;
   - en esa misma transacción, una lease viva de una instancia de la variante devuelve **409 `ACTIVE_LEASE`**. El plan sigue `PLANNED`: el operador puede esperar, detener la instancia o cancelar;
   - se reserva el espacio de las copias;
   - la respuesta es 201 con el `Plan`. El job se sigue en `GET /api/v1/jobs/{plan_id}`, con `progress.phase`, `bytes_copied` y `bytes_total`.

   Un plan se usa una sola vez. Si llega un `approval_id`, se consume ligado al `plan_hash` y a los recursos del plan; el plan de movimiento no lo exige (`approval_required: false`), pero nunca se ignora.

## Fases del journal (`operation_journal`, `kind = artifact.move`)

| Fase | Qué ha pasado | Si el proceso muere aquí, al arrancar… |
|---|---|---|
| `LOCKED` | Locks y reserva tomados | se deshace: `FAILED` `job_interrupted` |
| `COPYING` | Renombrado hecho, o copia a `staging/<job>/<artifact>.partial` en la raíz de destino con flush a disco | se deshace el renombrado, se borra el staging y el origen sigue siendo el válido |
| `VERIFIED` | SHA-256 releído de la copia igual al del origen; copia colocada sin sobrescribir | se retira la copia colocada; el origen sigue siendo el válido |
| `DB_SWITCHED` | Registro cambiado con CAS por `row_version` y fencing, journal y evento `artifact.moved` en una transacción | se abre el destino: si responde, se borra el origen y se completa; si no, el registro vuelve al origen |
| `ACCESS_TESTED` | El destino se abrió resolviendo la ruta desde el registro nuevo | se termina de borrar el origen |
| `SOURCE_DELETED` | Origen borrado tras comprobar el fencing | se completa |
| `COMPLETED` / `FAILED` / `CANCELLED` | Terminal: locks y reservas liberados | — |

Reglas que no cambian:

- Antes del cambio de registro, el origen es siempre el válido. Si el destino desaparece antes de ese momento, el job falla con `destination_missing` y no se toca el registro.
- Después del cambio, se conservan las dos copias hasta que el destino se abre. Un destino que no se abre devuelve el registro al origen (`FAILED` `access_test_failed`); la copia que no se pudo abrir queda anotada en `leftovers` del journal.
- El origen solo se borra si el job sigue teniendo el lock exclusivo de la variante. Si lo perdió, se detiene sin escribir.
- `EXTERNAL`, `HOST_MANAGED` y los blobs compartidos nunca se mueven ni se borran.

## Lectura

`GET /api/v1/artifact-locations` (paginado, cursor firmado) y `GET /api/v1/artifact-locations/{id}` (`read`).
