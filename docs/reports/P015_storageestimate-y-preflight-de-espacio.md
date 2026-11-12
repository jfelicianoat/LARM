# P015 — StorageEstimate y preflight de espacio

**Fecha**: 2026-10-11 · **Host**: A9_MEGA (PC IA) · **Estado**: **BLOCKED_REAL**: 17/17 PASS (3 reales); solo bloqueado porque su dependencia P014 no tiene REAL_PASS. [evidence.json](P015_storageestimate-y-preflight-de-espacio.evidence.json) · [operación](../operations/storage.md#preflight-de-espacio-y-reservas-p015)

## Qué se construyó

- **`StorageEstimate`** (`larm-domain/src/estimate.rs`), con la forma exacta del schema. La operación se describe en fases con ficheros nuevos o existentes, su papel y su tamaño con confianza y cota superior.
  - Pico nuevo por volumen, margen `max(1 GiB, ceil 5 %)` y requerido.
  - Utilizable = libre − mínimo de política − reservas de otros, nunca negativo.
  - Desglose de descarga, final, instalado, caché, pico temporal y existentes.
  - Aritmética comprobada hasta i64::MAX.
  - Se reserva la cota superior; un tamaño UNKNOWN sin cota bloquea salvo con presupuesto duro explícito.
- **Planificador** (`larm-storage/src/estimate.rs`): evalúa todas las ubicaciones y explica cada una (`fits`, `insufficient_space: requiere X y hay Y`, `state_*`, `volume_identity_unknown`, `fat32_file_too_large`, `size_unknown`). El mínimo de política del volumen es la mayor reserva de sus raíces o el mínimo global, no la suma.
- **Admisión** (`larm-db/src/reservations.rs`): una transacción que toma el bloqueo de escritura antes de leer.
  - Exige una instantánea de espacio de menos de 5 s.
  - Reserva por volumen físico, todo o nada.
  - Si algo no cabe, `STORAGE_INSUFFICIENT`.
  - `consume` deja `remaining = total − creados`, sin contar dos veces lo que el SO ya refleja.
  - La readmisión sustituye la reserva anterior y la liberación es idempotente.
- **`larm-daemon storage-preflight --file op.json [--location --job]`**: el mismo cálculo contra las ubicaciones y el espacio reales, para el operador y la prueba real. Los errores de la CLI incluyen ahora el `motivo`.

## Prueba real en el PC IA (9 pasos)

- **EXACT**: utilizable igual a libre (ctypes) − 10 GiB, la mayor reserva de las dos raíces; requerido = 1 GiB + 1 GiB de margen. Las dos raíces ven el mismo utilizable.
- **Insuficiente**: una descarga del tamaño del libre no cabe en ninguna, con el motivo y las cifras.
- **ESTIMATED 1..3 GiB**: se reservan 3 GiB.
- **UNKNOWN**: bloquea; con presupuesto duro de 2 GiB, cabe.
- **Ejemplo del contrato a escala**: 30/18/9 MB → nuevo 27, existente 30.
- **Desbordamiento**: 9e18 + 9e18 da `size_overflow`.
- **Admisión real**: queda la fila en `volume_reservation`; un segundo trabajo en otra raíz del mismo volumen no cabe.
- **Concurrencia**: cuatro procesos admiten a la vez el 40 % cada uno; entran dos y nunca se reserva más que el utilizable.

## Pendiente

- No hay operación de API propia: el estimate lo consumen los planes de descarga y transformación (`POST /download-plans`, `/transform-plans`, P052/P093).
- El diálogo de destino de la spec §30 llega con P052/P121.
- Revalidar antes de cada fase y cada bloque: lo hará el motor de descargas (P051/P052) con `consume` y `admit`.

## Revalidación del entorno (2026-10-25)

El primer intento dentro del sandbox no pudo crear el principal temporal: Credential Manager devolvió `AUTH_BACKEND_UNAVAILABLE` (error 5). En la repetición con acceso local autorizado, la observación mostró que el disco C: está bajo el umbral operativo por defecto y el daemon marcó `LOW_SPACE` las ubicaciones. El runner ahora configura umbrales de alerta mínimos solo en su daemon aislado; P014 sigue cubriendo el umbral real de `LOW_SPACE`, mientras esta fase ejercita estimación y admisión sobre el espacio físico disponible.

La aceptación real actual pasó **9/9 pasos**: lectura de espacio libre por `GetDiskFreeSpaceExW`, destinos que caben/no caben, cota superior estimada, UNKNOWN con presupuesto, ejemplo 30/18/9 MB, overflow, reservas entre dos raíces del mismo volumen y admisiones concurrentes. La salida está en [P015-REAL-preflight-2026-10-25.log](P015_runs/P015-REAL-preflight-2026-10-25.log) y el detalle estructurado en [steps-2026-10-25.json](P015_runs/steps-2026-10-25.json). La evidencia histórica del 2026-10-11 se conserva en `P015_runs/real-2026-10-11`. P015 continúa `BLOCKED_REAL` porque P014 aún no tiene `REAL_PASS`.
