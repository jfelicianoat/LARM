# Autenticación, secretos y autorizaciones

P009 · spec §90–§94 · contratos 03, 07 y 11 (H06) · ADR-0009.

## Principios

- La API escucha en `127.0.0.1`. Toda operación salvo `GET /api/v1/health/live` exige `Authorization: Bearer <token>`, también en loopback.
- Los secretos (tokens de principales, credenciales de fuentes, tokens de endpoint) viven solo en **Windows Credential Manager**. SQLite guarda únicamente la referencia (`credential_reference`): servicio, usuario, tipo, scopes y caducidad.
- Ningún secreto va por URL, línea de comandos ni logs. El redactor (`larm_domain::redact`) limpia toda línea de log y todo `original_error`.
- Se autentica y autoriza **antes** de leer el cuerpo: una petición sin credencial válida no llega a parsearse.
- La identidad y la audiencia salen de la credencial; ninguna cabecera del cliente las cambia.

## Principales

Cada principal es un `credential_reference` con servicio `larm/principal/{id}`; su nombre es el `owner_id` de sus recursos. El token está en Credential Manager con TargetName `<nombre>.larm/principal/<id>`.

| Tipo | Scopes permitidos | Uso |
|---|---|---|
| `OPERATOR` | cualquiera (`admin` = todos los de API) | administración local |
| `DESKTOP` | `admin` (concesión del usuario dueño del SID) | la app de escritorio, vía bootstrap |
| `BROKER` | solo `read` y `leases:manage` | AI Broker |
| `MCP` | scopes de API + `mcp_scopes`, audiencia `larm-mcp-admin` | agentes vía MCP de administración |

Scopes de API: `read`, `models:write`, `storage:write`, `runtimes:write`, `jobs:write`, `leases:manage`, `benchmarks:write`, `adapters:write`, `credentials:write`, `credentials:read`, `settings:write`, `admin`. `credentials:read` es distinto de `read`, así que el Broker no puede leer credenciales.

### MCP (H06)

Un principal MCP necesita el **permiso MCP de la herramienta y el scope de API** de la operación (`mcp-permissions.json`), entre por MCP o directamente por la API. Una operación sin herramienta MCP se le niega (`mcp_operation_unmapped`). El `admin` MCP es comodín de herramientas, no añade scopes de API. `start`/`stop`, `install`/`update`, `read`/`search` y `move`/`delete` son concesiones independientes.

### Comandos

```text
larm-daemon enroll-principal --name ai-broker --kind BROKER --scopes read,leases:manage
larm-daemon enroll-principal --name ai-broker --kind BROKER --scopes read,leases:manage --token-stdin   # usar el token ya configurado en el Broker
larm-daemon rotate-principal --name ai-broker      # token nuevo; el anterior deja de valer
larm-daemon revoke-principal --name ai-broker      # borra token y referencia
larm-daemon list-principals                        # sin tokens
```

El token se imprime una sola vez por stdout. El Broker lo guarda en **su propia** entrada de Credential Manager (`larm.keyring_service/keyring_username` de su configuración).

### Rotación

En el mismo proceso, una rotación invalida la caché al momento. Hecha desde otro proceso (la CLI), el token anterior deja de valer en cuanto caduca la caché de principales del daemon (5 s como máximo).

## Bootstrap del escritorio

`\\.\pipe\LARM-{sha256(SID)[0:16]}-bootstrap`, DACL `D:P(A;;GA;;;<SID>)(A;;GA;;;SY)` (ADR-0009), sin clientes remotos. Cada conexión recibe `{"principal": "desktop", "token": …, "api_base": …}` y se cierra. Entregar un token rota el anterior: solo vale la última sesión.

## Credenciales (`/api/v1/credentials`)

- `POST` (`credentials:write`, `Idempotency-Key`): `CredentialWrite`. El servicio debe estar en `larm/` y no en los espacios reservados `larm/principal/` ni `larm/lease/`, que gestiona LARM; crearlos permitiría fabricar principales. El secreto va a Credential Manager antes que la referencia; si la base falla, se retira. Nunca se sobrescribe un secreto existente (`CONFLICT`).
- `GET` lista (`credentials:read`) con `cursor`/`limit` (50 por defecto, 200 como máximo), cursor firmado. `GET /{id}` devuelve la referencia, nunca el secreto.
- `DELETE /{id}` exige `If-Match: <row_version>`: sin él `428`, distinto `409 STALE_REVISION`.
- Credential Manager caído → `503 AUTH_BACKEND_UNAVAILABLE`, reintentable. Se falla cerrado, sin confundirlo con un token incorrecto.

## Aprobaciones (`POST /api/v1/approvals`, `admin`)

`Approval {operation_digest, resource_ids, code_hash, permissions, approved}`. Queda ligada al actor que la concede, al digest (sha256 de JSON canónico RFC 8785), a los recursos y al `code_hash`. Caduca a los `security.approval_ttl_seconds` (900 s) y se consume una sola vez. Las operaciones destructivas (borrar, aceptar licencia, ejecutar código) la consumen con `ApprovalUse`; si no cuadra, `409 APPROVAL_REQUIRED` con el motivo en `details.reason`.

## Eventos

Cada mutación escribe su evento en el outbox en la misma transacción: `credential.created`, `credential.rotated`, `credential.deleted`, `approval.created`. El contrato 07 fija el formato `dominio.acción` pero no enumera eventos para estas operaciones; los nombres siguen ese formato.
