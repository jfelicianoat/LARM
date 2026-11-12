# P032 — actualización, rollback, pinning y retención de runtimes

Estado: `BLOCKED_REAL`. La revisión normativa [3.11.0](../contracts/REVISION_3_11_0_P032.md) resuelve los huecos del contrato 3.10.2; la aclaración [3.11.1](../contracts/REVISION_3_11_1_P032_P100.md) cierra la resolución efectiva P032/P100. El paquete 3.11.0 había pasado su verificación contractual (3.178 comprobaciones, 53 casos negativos y copia 73/73); la revisión 3.11.1 actualizó el manifiesto/hash y pasó `package_copy` 74/74.

## Implementación

- El dominio tipa `RuntimeVersion.lifecycle`, su transición inicial conservadora y `RuntimeRetentionPolicy` (2–50, default 3). La configuración raíz también tipa y valida `runtime_versions.retention`.
- SQLite conserva la política de retención, registra ciclo de instalación sin aceptar que el adapter lo promueva y persiste rollback/pins con CAS, idempotencia, journal, evento y outbox. Rollback exige destino instalado y validado. El pin exige versión instalada, registro exacto `COMPATIBLE` para variante/perfil y todos los hard gates en `PASS`; un gate ausente se interpreta como desconocido y bloquea.
- La API publica update, rollback, pin/unpin y lectura de pins. La actualización resuelve runtime/versión desde el ID de ruta y delega el job side-by-side a P031. No modifica el default al encolar.
- En Runtimes, el técnico ve ciclo de vida, selecciona un candidato para actualización, valida lock/estrategia/ubicación y confirma antes de encolar; el rollback requiere una segunda confirmación y usa el default observado. La política de retención usa CAS e idempotencia y avisa que no ejecuta borrados mientras el worker de limpieza no esté conectado.

## Verificación realizada

- `cargo test -p larm-domain`: pasó (158 pruebas unitarias y pruebas de integración/conformidad del crate).
- `cargo test -p larm-core -j 1`: 59/59 (54 unitarias y 5 de conformidad), incluida política de retención default 3, overrides y límites 2–50.
- `cargo test -p larm-db --test runtimes -j 1`: 12/12 pasaron, incluidos CAS/idempotencia de retención, rollback instalado y pin con compatibilidad.
- `cargo test -p larm-api -j 1`: primera corrida descubrió el nuevo campo de Settings no tipado y un conteo OpenAPI obsoleto; ambos se corrigieron; la repetición pasó con 34/34 pruebas unitarias y todos los binarios de integración del crate. La prueba nueva de P032 valida la resolución de versión/runtime y el bloqueo de lifecycle/canal.
- `npm test`: 114/114; `npm run typecheck`, `npm run lint` y `npm run build`: pasaron. La prueba de UI de P032 comprueba que no se envía actualización sin consentimiento, y que rollback no se envía antes de confirmar.
- `cargo fmt --all`: pasó.
- `cargo test --workspace --no-run -j 1`: compiló todos los binarios de pruebas del workspace (salida 0, 6m43s).

## Límites y dependencias abiertas

- La fase no demuestra actualización contra un runtime Windows cualificado. P031 continúa sin `REAL_PASS`; el formulario solo confirma el contrato de admisión y el job.
- P117 todavía no produce ni persiste `ComparisonResult`; por tanto no hay promoción gobernada por comparación en este incremento.
- P100 consume el pin exacto variante/perfil antes del default. La lease guarda la versión seleccionada y la admisión vuelve a resolverla antes de activar; si cambió, falla con conflicto. La UI permite crear, cambiar y retirar el pin desde Runtimes.
- No se implementó el worker de cleanup: ninguna versión se borra. Hace falta integrar leases y referencias de benchmarks/certificaciones antes de calcular elegibilidad y ejecutar borrado físico recuperable.

Por estas dependencias P032 se mantiene `BLOCKED_REAL`, no `REAL_PASS`. La evidencia de esta fase es focal; no representa una aceptación de hardware o proveedor externo.

## Revalidación P032/P100 — revisión 3.11.1

- `SqlPinnedRuntimeVersionResolver` consulta el pin de `(variant_id, launch_profile_id)`; si no hay, usa `runtime.default_version_id`. Sin pin y sin default devuelve `runtime_default_missing`: no selecciona por configuración inicial ni por orden del catálogo. Exige la misma familia, lifecycle `VALIDATED`/`DEFAULT`/`RETAINED`, una única instalación externa exacta para el host y coincidencia con la versión observada.
- P100 persiste el `runtime_version_id` resuelto en las constraints de la lease y lo revalida antes de activar. Las referencias cualificadas se indexan por variante y versión para que un cambio de pin no recicle endpoints de otra versión.
- Runtimes carga modelos, perfiles y pins; permite fijar/cambiar una versión compatible y retirar la fijación para solicitudes futuras. Ambas mutaciones envían `Idempotency-Key` y muestran el resultado.
- Verificación de este incremento: tests del bridge P100 3/3 (pin, default y discrepancias de versión), resolución SQL P032/P100 1/1, persistencia P032 12/12, escritorio 115/115, TypeScript, lint, build, Clippy con `-D warnings`, formato, conformidad documental 3.179 checks/53 negativos y `package_copy` 74/74.
- P032 sigue `BLOCKED_REAL`: la actualización real depende de P031, P117 aún no ofrece el resultado durable de comparación y el worker de retención/limpieza no está conectado. No se elimina ninguna instalación.

