# P018 — Storage analyzer, offline volumes y rebuild registry

**Fecha**: 2026-10-11 · **Host**: A9_MEGA (PC IA) · **Estado**: **BLOCKED_REAL**. Todas las pruebas ejecutables pasan, incluida la real (6 pasos más la regresión de P017). Queda una BLOCKED (desconectar un disco) y su dependencia P017 no tiene REAL_PASS. [evidence.json](P018_storage-analyzer-offline-volumes-y-rebuild-registry.evidence.json) · [operación](../operations/storage-maintenance.md)

## Qué se construyó

- **Reconciliación** (`larm-storage/src/reconcile.rs`):
  - ubicación `OFFLINE` → artefactos `OFFLINE`; ubicación `MISSING` → artefactos `MISSING`;
  - la variante pasa a `OFFLINE` guardando su disponibilidad anterior, sin borrar nada ni tocar la etapa;
  - al volver, se releen los bytes: `ONLINE`, `CORRUPT` o `MISSING`. La disponibilidad solo se restaura si todos los artefactos requeridos están `ONLINE` y releídos;
  - se ejecuta al arrancar, cada 5 minutos y con `storage-reconcile`.
- **Niveles H09 por volumen** (`levels_by_volume` y `level_transitions`): una entrada por volumen físico y alerta solo en cada transición.
- **Analizador** (`larm-storage/src/analyzer.rs`): las seis categorías del schema, con lo recuperable y sus bloqueos. Ni los ficheros del usuario, ni las copias `EXTERNAL`, ni los provisionales de jobs vivos son recuperables.
- **Limpieza y borrado** (`larm-storage/src/cleanup.rs`):
  - planes con aprobación obligatoria y de un solo uso;
  - el job de limpieza vuelve a comprobar cada elemento antes de borrarlo;
  - el borrado de una variante sigue este orden: locks y lease, registro en una transacción, ficheros y manifiesto.
- **Reconstrucción** (`larm-storage/src/rebuild.rs` + `larm-models/src/rebuild.rs`):
  - la copia al CAS escribe `manifests/<variant_id>.json`;
  - `registry-rebuild` lee los manifiestos, relee los blobs y registra lo completo en una base de staging nueva;
  - informa de lo incompleto, lo ilegible y los blobs que nadie cita;
  - la fusión es un `db-restore` verificado.
- **API**: `GET /storage-locations/{id}/analysis`, `POST …/cleanup-plan`, `POST …/cleanup` (202 y job), `POST /models/{id}/delete-plan` y `DELETE /models/{id}`. `openapi.json` queda con 47 operaciones implementadas.

## Defectos encontrados por la prueba real

1. **Manifiesto huérfano tras borrar.** Al borrar una variante, su manifiesto se quedaba en el CAS y la reconstrucción la resucitaba como «incompleta». Ahora el borrado lo retira. Lo comprueba la prueba de integración.
2. **`db-restore` en una instalación nueva** (defecto heredado de P004). Fallaba si la carpeta de datos no existía todavía, que es justo el caso de §134: la base se perdió. Ahora la crea. Tiene prueba de regresión en `larm-db`.

## Decisiones

- **Aprobación en la limpieza y el borrado**: siempre se exige. Ligada al `plan_hash` y a los recursos del plan, de un solo uso y del mismo actor que ejecuta. Si la operación falla después, se consume igualmente.
- **Fuentes en la reconstrucción**: las citadas por los manifiestos se recrean como marcadores deshabilitados, sin adaptador, credenciales ni licencia (no se inventa nada).
- **Huérfanos**: solo son recuperables los blobs del CAS sin registro. Los demás ficheros sueltos de una raíz se informan como `not_owned_by_larm` y nunca se tocan.
- **`RUNTIMES`**: recuperable desconocido (`null`) hasta que exista el registro de runtimes.

## Pruebas

| Prueba del prompt | Dónde |
|---|---|
| Volume offline/online | `volumen_offline_y_vuelta_con_reverificacion` y real (raíz que desaparece y vuelve) |
| Orphan detection | Unitaria del analizador, `huerfanos_parciales…` y real |
| Rebuild from fixture | `reconstruir_el_registro_desde_manifiestos` y real (staging + `db-restore` + daemon) |
| Partial download recognized | Unitaria, integración y real (parcial de un job vivo bloqueada; de uno terminado, recuperable) |
| Niveles H09 (contrato 11) | `niveles_por_volumen_sin_duplicar_raices` |

**Negativos**:

- sin aprobación, o aprobada para otros recursos;
- `If-Match` ausente (428) o viejo;
- `plan_hash` ausente;
- parámetros no previstos;
- `Probe` con otro recurso;
- lease activa (`ACTIVE_LEASE`);
- variante en uso (`variant_in_use`, sin borrar nada);
- plan ya usado;
- elemento que cambió tras el plan;
- blob ausente o corrupto en la reconstrucción.

**Mutaciones**: cada una se aplicó y se restauró el código. Las 10 hacen fallar al menos una prueba:

1. offline sin marcar;
2. reverificación sin hash;
3. parcial viva recuperable;
4. limpieza sin recomprobar;
5. borrado de compartidos;
6. borrado sin lock;
7. reconstrucción de incompletos;
8. manifiesto que no se retira;
9. restaurar la disponibilidad sin reverificar;
10. aprobación opcional.

Dos no se detectaban al principio:

- la del borrado de compartidos: se reforzó la prueba;
- la de la aprobación opcional: la mutación inicial era equivalente al código y se cambió por una que de verdad se salta la aprobación.

## Prueba real (`tools/p018_real.py`, 6 pasos PASS)

1. **Copia al CAS**: dos modelos con README compartido y sus dos manifiestos.
2. **Análisis**:
   - parciales: 800 B, de los que 300 son recuperables (la del job vivo está bloqueada);
   - el blob huérfano es recuperable y la foto del usuario no;
   - sin rutas absolutas.
3. **Limpieza**:
   - sin aprobación, 409;
   - con aprobación, 202 y job COMPLETED (308 B liberados);
   - la parcial viva y la foto, intactas.
4. **Borrado de Alfa**: DELETE 204. Su gguf se borra; el README compartido y Beta quedan intactos.
5. **Raíz que desaparece y vuelve**:
   - carpeta renombrada: MISSING y Beta `OFFLINE` sin tocar su etapa;
   - de vuelta: ficheros releídos `ONLINE` y Beta `AVAILABLE`.
6. **Reconstrucción y restore**:
   - `registry-rebuild` recupera a Beta y no queda nada incompleto;
   - `db-restore` en otra carpeta de datos, verificado;
   - el daemon sirve la variante recuperada.

## Bloqueado

- **Desconectar físicamente un disco**: el PC IA no tiene un segundo volumen ni un disco extraíble (bloqueo de P000). La desaparición de la raíz se probó en real; el estado `OFFLINE`, en integración.

## Pendiente

- **Interfaz del analizador y de los planes**: P128/P140.
- **Notificaciones de nivel de disco**: P128/P140. Las transiciones por volumen ya se calculan.
- **Importadores restantes y sidecars de procedencia**: P098.
