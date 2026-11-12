# P036 — InferenceEndpoint y registro de protocolos

**Estado:** `BLOCKED_REAL` · **Fecha:** 2026-10-23

P036 añade un registro durable de endpoints asociados a instancias y califica cada endpoint sólo después de un probe de salud y protocolo. La fase permanece `BLOCKED_REAL` porque P033, dependencia obligatoria para enlazarlo con instancias y leases productivas, no tiene `REAL_PASS`. La revisión contractual 3.3.0 ya define `GET /api/v1/broker/catalog` con scope `read`, propietario P101 y respuesta `BrokerCatalog`; P101 implementa la ruta.

## Implementación

- El dominio registra perfiles por identificador exacto de protocolo y hash SHA-256, admite varias versiones y separa `Known`, `UnknownProtocol` y `UnknownProfile`. Conserva los identificadores desconocidos sin tratarlos como soporte verificado.
- `InferenceEndpointCreate` e `InferenceEndpoint` aplican DTO estrictos conforme al schema: transportan sólo `credential_reference_id`, capacidades y evidencia, y rechazan campos de secreto, hashes inválidos e identidades mal formadas. Se reusa la tabla `inference_endpoint` de `0001_bootstrap.sql`; no hacía falta una migración nueva.
- `EndpointRegistryService` consulta el puerto de probe específico del adaptador. Sólo cualifica cuando salud, protocolo, perfil y capacidades observados coinciden con el perfil registrado. Unknown se persiste como `FAILED`; timeout también deja evidencia de fallo. Un error interno de la sonda no crea un registro parcial.
- `SqlInferenceEndpoints` confirma endpoint y evento/outbox en una sola transacción. La lectura del Broker filtra `QUALIFIED`; un endpoint que deja de responder pasa a `FAILED` mediante CAS, añade evidencia y emite el evento correspondiente.
- P036 no añade una ruta propia para listar endpoints. El catálogo read-only del Broker es propiedad de P101; la prueba vigente verifica que la operación canónica, su espejo local, el schema `BrokerCatalog` y OpenAPI coinciden. La integración del catálogo con instancias y leases reales sigue ligada a P033/P100/P101.

## Verificación

La evidencia vigente registra **5 PASS, 0 FAIL y 0 pruebas BLOCKED**, con estado `BLOCKED_REAL` por la dependencia P033. Pasan perfiles y DTO estrictos, almacenamiento SQLite multi-endpoint con rollback de outbox y CAS, cualificación/descalificación con unknown/timeout/error, la prueba real de Windows con HTTP loopback y SQLite, y la consistencia contractual del catálogo P101.

La prueba Windows `P036-REAL-WINDOWS-LOOPBACK-AND-SQLITE` verificó que el servidor fixture responde a health y al handshake OpenAI `/v1/models`, que la fila queda `QUALIFIED` en SQLite y visible en la vista Broker, y que al cerrar el servidor el siguiente probe la marca `FAILED`, emite el segundo evento/outbox y la elimina de esa vista. El sandbox bloqueó esas conexiones loopback; la misma prueba pasó al ejecutarse con acceso local de Windows. El fixture acredita el registro y el ciclo de vida del módulo con datos de prueba, pero no la conexión a un runtime gestionado ni a un endpoint de producción.

`cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings` y `cargo test --workspace -j 1` pasaron en la última puerta completa registrada, con **906 pruebas pasadas, 0 fallidas y 0 ignoradas** en 97 resultados. La puerta histórica `tools/check.ps1 -Prompt P036 -Real` pasó **13/14** antes de reconciliar el paquete 3.3.0. La comprobación actual confirma que las 57 copias contractuales coinciden con el paquete; la prueba Windows focal requiere loopback local y pasó al ejecutarse con ese acceso.

## Pendientes

- P100 ya reutiliza `EndpointQualificationService` para cualificar endpoints externos y guardar instancia+endpoint juntos antes de activar la lease; esta integración sólo tiene evidencia de fixture. Alcanzar `REAL_PASS` en P033 y aceptar el ciclo con un runtime instalado.
- Completar y aceptar la lectura de endpoints cualificados en el catálogo P101 dentro del lifecycle de una lease productiva, con autorización por principal.
- Repetir la puerta completa para producir un recibo actual tras la reconciliación del paquete 3.3.0. Las credenciales compartidas no se almacenaron ni se usaron durante esta revalidación.

La evidencia, transcripciones con hashes y recibo del gate están en [evidencia P036](P036_inferenceendpoint-y-registro-de-protocolos.evidence.json), [runs P036](P036_runs/) y [gate P036](check_P036_real.json).
