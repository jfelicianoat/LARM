# P019 — Guards de artefactos, reservas y journal transaccional

**Fecha**: 2026-10-11 · **Host**: A9_MEGA (PC IA) · **Estado**: **BLOCKED_REAL**: 15 PASS (3 reales) y 2 BLOCKED; además, su dependencia P004 no tiene REAL_PASS. [evidence.json](P019_guards-de-artefactos-reservas-y-journal-transaccional.evidence.json) · [operación](../operations/guard.md)

Ampliación posterior del 2026-10-27: [caudal del Core y CAS concurrente](P051_h08_core_cas_2026-10-27.md). Dos consumidores transfieren bytes TCP reales con contraste sin límite y respetan presupuesto agregado/ráfaga, cambio persistido, pausa y reapertura SQLite. Ocho pares adicionales completan 16 jobs. Se reproduce y corrige la carrera Windows al publicar el mismo blob; exige hash/tamaño antes de reutilizarlo y rechaza corrupción sin retirar el parcial. Pasan motor 50/50, storage 32/32 y daemon/P048 original 1/1 en 192,81 s, con cualificación 39/16 y regresión de integridad/pausa/reinicios/CAS. Clippy/fmt y 74 copias pasan; 40 inputs/binarios contrastados. La fuente y los planes del ensayo Core son de test: falta reunir dos transferencias cualificadas, PATCH y kill/reinicio en un mismo H08 del daemon. Frontend, workspace y runners completos no se repiten; los informes anteriores conservan su alcance histórico.

## Qué se construyó

- **Dominio** (`larm-domain/src/guard.rs`): recursos (volumen, variante, artefacto, pool), modos SHARED/EXCLUSIVE y normalización al orden global (sin duplicados; el exclusivo manda). Los puertos son `ResourceGuard` y `PoolReservations`.
- **Resource Guard SQLite** (`larm-db/src/guard.rs`):
  - adquisición todo o nada, con el bloqueo de escritura tomado antes de leer;
  - rechazo limpio y reintentable: `ACTIVE_LEASE` si el titular es una lease viva, `CONFLICT` `resource_locked` si es otro trabajo;
  - fencing por generación;
  - liberación solo de ese titular y generación;
  - el paso del journal se escribe en la misma transacción que los locks;
  - reconciliación que libera solo con el journal o la lease terminados, sin timeout;
  - `delete_blockers` para el CAS compartido.
- **Reservas**:
  - de pool de memoria, atómicas y con suma comprobada contra el presupuesto;
  - por volumen (P015), que ahora funden en una sola las demandas del mismo volumen (dos raíces en el mismo disco reservan una vez).

## Defecto encontrado en las propias pruebas

La primera versión de la carrera **no detectaba** la ausencia del bloqueo de escritura. En modo WAL, SQLite ya impide la doble escritura devolviendo un error interno de instantánea, y la prueba solo miraba si hubo error. Ahora exige que el perdedor reciba el rechazo limpio del guard (`CONFLICT`, reintentable). Con la mutación (sin bloqueo), dos pruebas fallan; con el código real, pasan.

## Oráculos del prompt

| Oráculo | Prueba |
|---|---|
| Carrera entre 10 admisiones y move/delete | 20 rondas: nunca un escritor con lectores; los perdedores reciben CONFLICT |
| Orden de locks sin interbloqueo | 30 rondas de A,B contra B,A: exactamente uno gana |
| Dos raíces del mismo volumen reservan una vez | Una fila con la suma |
| Caída en la adquisición, el commit o la liberación | Adquisición fallida no deja nada a medias (ni journal); un titular caído con journal vivo conserva el lock hasta que el journal termina |
| CAS compartido referenciado no borrable | `delete_blockers` lista las variantes y los locks |

## Prueba real en el PC IA (`tools/p019_real.py`, 6 pasos)

Un fichero de 8 MiB alterna entre una carpeta en C: y otra en `\\localhost\Temp`. Hay 30 lectores (lock compartido) y 6 movedores (exclusivo, con copia verificada y fencing antes de borrar el origen), todos procesos distintos sobre la misma base SQLite.

- No hay ningún solape lector/movedor ni entre movedores. La contención fue real: 192 reintentos.
- Cada lector vio el fichero entero en un solo sitio y con el hash original, que se conserva al final.
- Un proceso que muere tras adquirir deja el journal en LOCKED: la reconciliación conserva su lock y, al pasar el journal a FAILED, lo libera.
- No queda ningún lock al terminar.

## Bloqueado

- **Dos volúmenes físicos**: C: y `\\localhost\Temp` son dos `volume_key`, pero el mismo disco; el segundo volumen sigue pendiente (bloqueo de P000).
- **Leases reales**: el prompt pide repetir la prueba con leases de P100 cuando existan.
