# P034 — Launch Profiles y configuración reproducible

**Estado:** `BLOCKED_REAL` · **Fecha:** 2026-10-22

P034 añade perfiles de lanzamiento persistentes y editables, con hash de parámetros efectivos, control de revisión y comparación por cambios. La fase queda `BLOCKED_REAL`: el daemon aún no carga un perfil guardado para construir el `LaunchPlan` que termina asociado a una `RuntimeInstance`, y P030/P033 no tienen `REAL_PASS`.

## Implementación

- El dominio expone DTO estrictos para ECO, BALANCED, PERFORMANCE, LONG_CONTEXT y CUSTOM. El hash canónico cubre preferencia de backend y parámetros efectivos; el diff usa rutas JSON Pointer estables.
- El runtime materializa los cuatro perfiles builtin desde CPU, backend, límite de contexto y batch observados o suministrados explícitamente. No inventa datos ausentes. Combina el límite del perfil y de la lease por el menor valor.
- El repositorio SQLite usa la tabla `launch_profile` existente, sin migración. Create, replay idempotente, edición CAS, eliminación, evento y outbox comparten transacción. La lectura vuelve a validar el registro y su hash; las escrituras parciales se revierten si falla el outbox.
- La API monta GET/POST en `/api/v1/launch-profiles` y GET/PATCH/DELETE en `/api/v1/launch-profiles/{id}`, con scopes contractuales, cuerpo/query estrictos, `Idempotency-Key`, `ETag` e `If-Match`. OpenAPI recoge las cinco operaciones.
- Runtimes incorpora una pantalla para listar, crear, copiar, editar y eliminar perfiles, mostrar el hash y comparar parámetros efectivos.
- La ruta GET que antes devolvía un perfil configurado del Broker ahora sirve el catálogo contractual de SQLite. La integración de Broker debe migrar a ese catálogo cuando P030/P033 habiliten el flujo consumidor.

## Verificación

El runner formal registra **10 PASS, 0 FAIL y 1 BLOCKED**, con estado `BLOCKED_REAL`. Pasan los tests de dominio/hash/diff, materialización builtin, CRUD SQLite/idempotencia/outbox, autorización y ETag de API, opciones inválidas del adaptador, inmutabilidad y round-trip SQLite del snapshot, flujo de interfaz, montaje del daemon y proceso real de fixture en Windows.

La prueba real `sesion_cualificada_inicia_proceso_propio_desde_snapshot_efectivo` verificó argumentos, entorno autorizado, límite, snapshot, parada y ausencia del valor secreto en los logs. Construye los parámetros directamente y **no** acredita el consumo de un LaunchProfile cargado desde SQLite; ese caso queda explícitamente BLOCKED.

`tools/check.ps1 -Prompt P034 -Real` completó **13/14 comprobaciones**. Pasan formato, Clippy de workspace, fixtures, tests completos de workspace, arquitectura, Tauri y los cuatro pasos del frontend. `package_copy` falla porque las copias locales de `adapter-methods.json`, `api-operations.json` y `schemas/larm.schema.json` difieren del paquete canónico; son divergencias heredadas de P031/P099.

El test `stderr_enorme_no_bloquea_y_se_limita` recibía un timeout de handshake de 2 segundos aunque su fixture escribe 12 MiB antes del handshake. Se amplió sólo el margen de ese test a 10 segundos; después pasó dentro de la suite completa.

## Pendientes

- Consumir el perfil persistido durante el flujo productivo `LaunchProfile → LaunchPlan → RuntimeInstance`, validar `adapter_options` y conservar el snapshot/hash correspondiente.
- Migrar el consumidor del Broker a la lista contractual de perfiles.
- Obtener `REAL_PASS` de P030 y P033; no declarar P034 completo antes de esos resultados.
- Resolver las tres divergencias del paquete contractual y volver a ejecutar `tools/check.ps1 -Prompt P034 -Real` hasta que las 14 comprobaciones pasen.

## Archivos principales

El cambio abarca el dominio y runtime, el repositorio SQLite, API/OpenAPI y daemon, la pantalla Runtimes, el test de robustez del fixture y `tools/p034_evidence.py`. La evidencia y el recibo del gate están en [P034 evidence](P034_launch-profiles-y-configuracion-reproducible.evidence.json) y [check P034](check_P034_real.json).
