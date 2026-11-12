# Contrato LARM–AI Broker

Estado: implementación parcial `BLOCKED_REAL`. La integración se habilita en el Broker solo después de cualificar el catálogo, el servicio durable de leases y el runtime elegido. El puente está desactivado por defecto.

## Operaciones publicadas

Todas las rutas administrativas requieren `Authorization: Bearer …` de un principal enrolado en LARM. El token identifica a ese principal y sus scopes; el token de administración del Broker es una credencial independiente y no se copia a la configuración, a la base de datos ni a los logs de LARM.

| Método y ruta | Scope | Respuesta | Uso |
| --- | --- | --- | --- |
| `GET /api/v1/broker/catalog` | `read` | `BrokerCatalog`, 200 | Devuelve solo variantes declaradas elegibles para Broker. |
| `GET /api/v1/leases` | `leases:manage` | lista de leases, 200 | Lista las leases del principal autenticado. |
| `POST /api/v1/leases` | `leases:manage` | `LeaseResponse`, 201; `JobReceipt`, 202 | Crea una lease; exige `Idempotency-Key`. Si se devuelve 202, consultar el job hasta estado terminal y usar el endpoint solo después de `ACTIVE`. |
| `GET /api/v1/leases/{id}` | `leases:manage` | lease, 200 | Consulta una lease propia. |
| `POST /api/v1/leases/{id}/renew` | `leases:manage` | lease, 201 | Renueva con `If-Match` e `Idempotency-Key`; respeta owner, generación y revisión. |
| `DELETE /api/v1/leases/{id}` | `leases:manage` | sin cuerpo, 204 | Libera una lease propia con `If-Match`; una liberación terminal repetida es idempotente. |
| `GET /api/v1/jobs/{id}` | `read` | estado de job, 200 | Consulta una admisión de lease asíncrona. |

Las peticiones de lease son estrictas. `owner_id` debe coincidir con el principal autenticado; una clave repetida con el mismo cuerpo reproduce la respuesta y con otro cuerpo produce conflicto. La lease fija variante, perfil, restricciones, generación y fingerprint; LARM no rebaja silenciosamente backend, presupuesto o capability.

## Acceso al endpoint

La respuesta de lease contiene un `credential_reference`, nunca el token. En Windows el Broker solicita la credencial por el named pipe local de un solo uso, con nonce, SID, owner y generación de lease comprobados. El token de endpoint vive en memoria del Broker y se revoca al liberar o expirar la lease.

La adopción Ollama actual usa un gateway por lease porque el listener de Ollama no autentica cada petición. El gateway fija el modelo de la lease, exige el token de endpoint y rechaza capacidades que el perfil no anuncia, incluido streaming. Su ruta interna actual todavía no aparece en `api-operations.json` ni en OpenAPI; por ello esta rebanada no se anuncia como contrato estable ni como integración lista para producción hasta que el contrato propietario la publique y pruebe.

## Límites y errores

`RESOURCE_UNAVAILABLE` antes de despachar es reintentable; incompatibilidad, modelo desconocido, endpoint degradado y runtime no disponible mantienen sus errores diferenciados. `fallback_allowed: false` nunca deriva a otro proveedor ni variante. El endpoint raw externo solo es elegible con aceptación explícita del modo `EXTERNAL_LOCAL_TRUSTED` y sus límites locales documentados.

La rebanada actual persiste en SQLite la solicitud `REQUESTED`, su respuesta idempotente, el paso a `ADMITTING` y el job de admisión. La activación del runtime adoptado y el token siguen ligados al proceso actual; al reiniciar, LARM invalida las generaciones antiguas antes de aceptar tráfico. P100 continúa hasta conectar la lease con `RuntimeInstance` e `InferenceEndpoint` cualificados y persistir renovación, expiración y liberación. El Broker no debe usar una lease hasta que el job termine y el estado consultado sea `ACTIVE`.

## Configuración sensible

LARM guarda referencias de credencial en SQLite y secretos de principal/endpoint en el almacén protegido del sistema. Nunca guardar tokens de AI Broker o Agora en este repositorio ni pasarlos por argumentos de proceso. El Broker permanece desactivado por defecto; el host y el administrador del Broker deben acordar explícitamente endpoint loopback, principal, scopes, timeout y runtime cualificado.
