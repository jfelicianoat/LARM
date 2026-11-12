# P037 — Onboarding genérico de runtimes desde GUI/API

**Estado:** `BLOCKED_REAL` · **Fecha:** 2026-10-22

P037 implementa las operaciones de onboarding que ya están publicadas en el contrato: registrar un host externo y crear/probar un adaptador runtime declarativo. El wizard muestra una revisión de comandos, protocolo, estrategias y permisos; conserva la instalación como `CUSTOM`/`UNTRUSTED`. La fase queda `BLOCKED_REAL`: faltan cuatro dependencias en `REAL_PASS` y el contrato canónico no define las operaciones restantes de borrador, probe previo o activación.

## Implementación

- `ExternalHostCreate` valida la URL HTTP(S), rechaza credenciales embebidas, query, fragmento, campos desconocidos y el modo `MANAGED`. Sólo acepta una referencia UUIDv7 de Credential Manager. La conversión crea estado `UNKNOWN`, propiedad `EXTERNAL` y versión observada nula; registrar un host no lo prueba ni lo inicia.
- `POST /api/v1/external-hosts` implementa la operación P037 existente, con `runtimes:write` y respuesta 201. OpenAPI marca la operación como implementada. El handler persiste mediante el repositorio existente y no exige cabeceras ausentes del contrato.
- La pantalla Runtimes permite registrar hosts externos y crear adaptadores declarativos. El editor valida la forma básica del JSON localmente, presenta comandos exactos, protocolo, estrategias anunciadas, raíces, orígenes, procesos y referencias secretas, y envía el documento a `POST /adapters/declarative` con `Idempotency-Key`. El servidor vuelve a validar el schema y las rutas antes de materializarlo.
- Tras el alta, la interfaz informa que el adaptador es `UNTRUSTED` y permite solicitar/consultar su job de conformidad mediante la operación P026. Un probe encolado no se presenta como resultado aprobado ni habilita ejecución.
- No hizo falta una migración: el registro de hosts y su journal/outbox ya existen. La prueba de integración vive en `apps/daemon`, raíz de composición autorizada para combinar API y SQLite; el crate `larm-api` conserva el grafo contractual.
- El alta de host sólo admite una referencia UUIDv7 protegida. Los tokens incluidos en la conversación no se usaron ni se copiaron al código, las pruebas o la evidencia de P037.

## Verificación

La evidencia formal registra **4 PASS, 0 FAIL y 2 BLOCKED**, con estado `BLOCKED_REAL`. La prueba Windows levanta el servidor HTTP loopback de la API, registra el host en SQLite, confirma `UNKNOWN`/`EXTERNAL`, verifica que una URL con userinfo devuelve 400 y que un principal sin `runtimes:write` obtiene 403 antes de analizar el cuerpo. El test confirma que sólo quedó la fila válida. Es un fixture local: no acredita disponibilidad de Broker, Agora ni otro host productivo.

El wizard tiene **5 pruebas focales**. La suite de interfaz completa pasó **64/64**; `cargo fmt`, clippy del workspace, tests del workspace (**910 pasadas, 0 fallidas, 0 ignoradas**), arquitectura de workspace/Tauri, clippy/tests de Tauri, lint, typecheck y build pasan. `tools/validate_evidence.py` validó todas las evidencias disponibles. La puerta oficial `tools/check.ps1 -Prompt P037 -Real` quedó en **13/14**: `package_copy` detecta diferencias heredadas de las copias locales `adapter-methods.json`, `api-operations.json` y `schemas/larm.schema.json` frente al paquete canónico.

## Pendientes

- `api-operations.json` sólo define alta durable de host y alta durable de adaptador. No define API para draft manifest, validación sin guardar, dry-run, prueba de salud/versión de host ni enable/disable. La UI ofrece preview estructural local y el probe P026 después del alta, sin sustituir esas operaciones.
- Por ese límite contractual siguen pendientes los casos de executable inexistente y health HTTP de host con éxito/fallo como operaciones del wizard. No se añade una ruta o transición propia.
- P023, P030, P031 y P036 siguen `BLOCKED_REAL`, por lo que P037 no puede cerrarse como `REAL_PASS`.
- `package_copy` requiere reconciliar las tres copias contractuales locales con el paquete propietario. Se conservan en este incremento para no descartar cambios previos.

La evidencia y sus transcripciones con hashes están en [evidencia P037](P037_onboarding-generico-de-runtimes-desde-gui-api.evidence.json), [runs P037](P037_runs/) y [gate oficial](check_P037_real.json).
