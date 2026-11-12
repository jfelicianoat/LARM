# P027 — CUSTOM_RULES como datos y estrategia NuGet gestionada

**Estado: `BLOCKED_REAL`, 2026-10-22.** La evidencia formal vigente registra 9 pruebas PASS, cero FAIL y una prueba REAL BLOCKED. El gate oficial pasó 13 de 14 comprobaciones; la única incidencia es `package_copy`, por tres copias contractuales locales que divergen del paquete canónico por cambios de P031/P099. P023 y P031 continúan en `BLOCKED_REAL`, por lo que P027 no puede cerrarse.

## Implementación de esta fase

CUSTOM_RULES ahora es un DTO JSON cerrado y versionado (`schema_version: 1.0`), con campos, operadores y efectos enumerados. La validación rechaza propiedades/campos desconocidos, predicates vacías o inválidas, límites incoherentes, targets que no sean UUIDv7 y contradicciones detectables. La evaluación es determinista, combina predicates con AND y aplica la precedencia `deny > require > limit`; no interpreta expresiones ni scripts.

El dominio añade una función de colocación que aplica las reglas después de filtrar candidatos por sus hard gates. La política productiva `PlacementPolicy::CustomRules` aún devuelve `custom_rules_not_implemented`: esta función todavía no está conectada a la selección real del catálogo. La vista previa de la pantalla de Configuración se ejecuta en el cliente y ayuda a revisar una regla; no acredita esa integración productiva.

La API ofrece `GET` y `PATCH /api/v1/custom-rules`. El `GET` entrega la revisión en `ETag`; el `PATCH` exige `If-Match`, scope `settings:write`, schema estricto y validación semántica. SQLite guarda el documento en la tabla `settings` existente y cambia la revisión mediante CAS. El evento `settings.updated` se escribe en el outbox dentro de la misma transacción: una prueba inyecta un fallo del outbox y verifica que tampoco sobreviven el payload ni la revisión. No fue necesaria una migración.

El escritorio incluye la pantalla para editar condiciones y efectos, previsualizar reglas y guardar o descartar cambios usando la revisión ETag. La ruta y sus DTO se conectan al daemon y al contrato público ya presente en el paquete.

El plan de instalación ahora acepta NuGet sólo con arquitectura fijada y un `packages.lock.json` válido (formatos 1–3), con versión resuelta y SHA-512 de contenido para cada paquete, incluido todo el grafo declarado. Rechaza entradas `Project`, identificadores que podrían escapar una ruta, versiones no resueltas y hashes ausentes o inválidos. El hash RFC 8785 del lock se incorpora al binding `InstallSpec`; junto con el hash inmutable del perfil, queda cubierto por `plan_hash`, así que cambiar el catálogo de paquetes invalida el plan y cualquier aprobación ligada a él. NuGet mantiene su destino lado a lado y no recibe un prefijo de entorno Python. Aún falta ejecutar restore en un worker privado con caché dentro de ese destino. La documentación de NuGet describe el lock versionado y `--locked-mode` como mecanismo para restaurar la clausura exacta o fallar ([PackageReference y lock files](https://learn.microsoft.com/en-us/nuget/consume-packages/package-references-in-project-files)); la caché global puede aislarse con `NUGET_PACKAGES` ([carpetas de caché](https://learn.microsoft.com/nuget/consume-packages/managing-the-global-packages-and-cache-folders)).

## Verificación

El runner `tools/p027_evidence.py` ejecutó estos casos, todos PASS:

- `P027-CUSTOM-RULES-DOMAIN`: schema cerrado, operadores, AND, precedencia, límites y contradicciones.
- `P027-CUSTOM-RULES-PLACEMENT`: hard gates, deny/require/limit y desempate de candidatos.
- `P027-NUGET-LOCK-PLAN-BINDING`: arquitectura, lock NuGet versionado, versiones y hashes de paquete, rechazo de rutas/proyectos y cambio de hash del plan.
- `P027-SCHEMA-COMPATIBILITY`: siete formas admitidas y tres casos de campos u operadores desconocidos rechazados.
- `P027-SQLITE-CAS`: persistencia, revisión CAS, rechazo de datos obsoletos/inválidos, corrupción y rollback integral por fallo de outbox.
- `P027-API-CAS-ETAG`: GET/PATCH, ETag, If-Match, revisión obsoleta y entradas inválidas.
- `P027-DAEMON-WIRING`: compilación del daemon con el repositorio SQLite.
- `P027-FRONTEND-PREVIEW-ETAG`: test de la pantalla y del PATCH con If-Match.
- `P027-FRONTEND-QUALITY`: build de la interfaz.

`tools/check.ps1 -Prompt P027 -Real` pasó formato, Clippy, fixtures, `cargo test --workspace`, arquitectura, formato/Clippy/tests de Tauri y lint, typecheck, tests y build del frontend. El único FAIL común fue `package_copy`, en `adapter-methods.json`, `api-operations.json` y `schemas/larm.schema.json`; sus diferencias corresponden al estado local de P031/P099, no a cambios de P027. `tools/validate_evidence.py` validó todas las evidencias del repositorio.

La transcripción y el recibo vigentes están en [check_P027_real.json](check_P027_real.json), [la evidencia formal](P027_custom-rules-como-datos-y-estrategia-nuget-gestionada.evidence.json) y [las pruebas de P027](P027_runs/).

## Pendientes

No se implementó ni ejecutó el worker de restore NuGet. Sigue pendiente lanzar `dotnet restore --locked-mode` con `NUGET_PACKAGES` confinado a una caché privada dentro de la instalación lateral, verificar los bytes restaurados contra `contentHash` y probar dos versiones coexistentes junto con reglas sobre un catálogo local real. No se modificó la configuración global de NuGet ni se declara una restauración real.

También queda conectar la configuración guardada a la selección productiva del catálogo e incluir la revisión de reglas en el hash del plan para invalidar aprobaciones cuando cambien. El lock y la arquitectura NuGet ya quedan fijados en el plan. P023 y P031 deben alcanzar `REAL_PASS`. Hasta entonces, P027 permanece `BLOCKED_REAL`.
