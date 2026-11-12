# Resource Guard, reservas y journal (P019)

El guard es el **único** sitio donde un consumidor decide si puede usar, mover, borrar o transformar un recurso. Nadie consulta `lease_count` ni actúa fuera de él (contrato 02). Lo consumirán el movimiento de modelos (P016), el CAS (P017), las transformaciones y la admisión de leases (P100).

## Locks (`resource_lock`)

| Modo | Para | Compatible con |
|---|---|---|
| `SHARED` | Usar un artefacto: una lease que lo carga, una lectura | Otros `SHARED` |
| `EXCLUSIVE` | Mover, borrar o transformar | Nada |

- Los recursos son `volume:…`, `variant:…`, `artifact:…` y `pool:…`. Se toman siempre en **orden global**: volumen, variante, artefacto, pool, y dentro de cada tipo por id. Da igual el orden en que se pidan, así que dos operaciones no se esperan en ciclo. Si un recurso se pide en los dos modos, manda el exclusivo.
- Se toman **todos o ninguno**, en una transacción que primero toma el bloqueo de escritura. El perdedor recibe un rechazo limpio y reintentable:
  - `ACTIVE_LEASE` (409) si quien lo tiene es una lease viva (spec §38: un modelo con una lease activa no se mueve ni se borra);
  - `CONFLICT` `resource_locked` si es otro trabajo.
- **Lease bajo el mismo lock** (P016): pedir en exclusiva una variante con una lease viva de una de sus instancias devuelve `ACTIVE_LEASE`, aunque esa lease no tenga locks propios. Se comprueba dentro de la transacción que toma los locks.
- **Fencing**: la clave es `(recurso, titular, generación)`. Antes de cada escritura irreversible, como borrar el origen tras copiar, el titular comprueba `holds(titular, generación, recurso, EXCLUSIVE)`. Una generación ya liberada o reconciliada no escribe.
- `release` solo borra las filas de ese titular y esa generación.
- **Paso del journal desde fases concretas** (P016): con `from_phases`, el paso solo se escribe si el journal está en una de esas fases; si no, toda la transacción (locks incluidos) se rechaza con `CONFLICT` `journal_phase_conflict`. Así un plan `PLANNED` solo se ejecuta una vez.
- **Journal en la misma transacción**: `acquire` y `release` pueden escribir el paso de `operation_journal` (`LOCKED`, `COPYING`, …, `COMPLETED`) atómicamente con los locks.

## Reconciliación tras una caída

Un lock no se retira nunca por tiempo. `reconcile` mira a su titular:

| Titular | Resultado |
|---|---|
| Journal en `COMPLETED`, `FAILED`, `CANCELLED`, `FAILED_RECOVERABLE` o `CONSUMED`, o lease en `EXPIRED`, `RELEASED` o `FAILED` | Se libera |
| Journal o lease todavía vivos | Se conserva: un movimiento a medias sigue protegido aunque su proceso haya muerto, hasta que la recuperación del journal lo cierre |
| Sin journal ni lease | Se informa como `unverified` y no se libera solo |

## Borrar artefactos

`delete_blockers(artefacto)` devuelve las variantes que lo referencian (`referenced_by_variant:…`; un blob CAS compartido no se borra mientras alguien lo use) y los locks vigentes (`locked_by:…`). Vacío significa que se puede borrar.

## Reservas

- **Por volumen físico** (`volume_reservation`, P015): una sola reserva por volumen aunque la operación toque dos raíces del mismo disco; las demandas se suman.
- **Por pool de memoria** (`memory_reservation`): `reserve(titular, pool, bytes, presupuesto)` es atómico. La suma comprobada no puede superar el presupuesto (`pool_budget_exceeded`, reintentable) y repetir sustituye en vez de sumar. El presupuesto lo calculará la admisión de P100.
