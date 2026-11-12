# P009 — Autenticación, secretos y autorizaciones desde la primera API

**Fecha**: 2026-10-11 · **Host**: A9_MEGA (PC IA) · **Estado**: **BLOCKED_REAL** — 29 PASS, 0 FAIL, 1 prueba real bloqueada (segundo usuario de Windows) más las dependencias P003/P004. [evidence.json](P009_autenticacion-secretos-y-autorizaciones-desde-la-primera-api.evidence.json) · [operación](../operations/security.md) · [ADR-0009](../adr/0009-excepcion-unsafe-dacl-pipes.md)

## Qué se construyó

- **Dominio** (puro):
  - `auth`: 12 scopes de API, 11 permisos MCP, tipos de principal con sus restricciones (el Broker solo `read` + `leases:manage`) y el catálogo de las 142 operaciones y 12 herramientas MCP cargado de los contratos. `authorize` aplica la intersección de H06.
  - `approval`: aprobaciones ligadas a digest, recursos, `code_hash` y actor, con TTL de 900 s y uso único.
  - `canonical`: JSON canónico RFC 8785.
  - `credential`: espacio `larm/` y los espacios reservados `larm/principal/` y `larm/lease/`.
- **Casos de uso** (`larm-core::security`):
  - registro de principales sobre `credential_reference` + Credential Manager, con caché de 5 s, revocación inmediata al rotar y fallo cerrado con 503;
  - CRUD de credenciales con compensación;
  - aprobaciones;
  - cursores firmados con HMAC-SHA256.
- **SQLite** (`larm-db`): repositorios de credenciales, aprobaciones (`operation_journal`) e idempotencia. Cada mutación escribe `event` + `event_outbox` en su transacción.
- **API** (`larm-api::security`): el guard autentica y autoriza antes de leer el cuerpo, y deja el contexto en la petición. Rutas `POST/GET /credentials`, `GET/DELETE /credentials/{id}` y `POST /approvals`, con `Idempotency-Key`, `If-Match` y paginación.
- **Daemon**:
  - `enroll-principal`, `rotate-principal`, `revoke-principal`, `list-principals`, y `--serve-api`;
  - pipe de bootstrap de un uso;
  - `winsec.rs`, la única excepción `unsafe`, que fija la DACL `D:P` solo para el SID del usuario y SYSTEM. La usan el bootstrap y el canal de credenciales de P107, que hasta ahora tenía la DACL por defecto.

## Defectos de la rebanada P107 resueltos aquí

- Tokens en el servicio `larm` (fuera del espacio `larm/` del contrato 03) y principales declarados en un JSON aparte: ahora son `credential_reference` con su token en `larm/principal/{id}`.
- **Un token rotado seguía valiendo** si se seguía usando, porque la caché solo se recargaba con tokens desconocidos. Corregido y probado: rotación en el mismo proceso → revocación inmediata; desde otro proceso → como mucho 5 s.
- El pipe de credenciales sin ACL por SID.
- Dos modelos de principal y de error, ahora unificados.

## Pruebas

- 19 de dominio y casos de uso; 8 de API con el router real; 4 de SQLite, entre ellas 8 consumos concurrentes de una aprobación con un único ganador.
- Sabotaje de control: al quitar `authorize` del guard fallan 3 pruebas de la API.
- **Real** (`tools/p009_real.py`, 10 pasos):
  - alta por CLI; 401/403;
  - el secreto está en Credential Manager (leído con `CredReadW`) y no aparece en SQLite, WAL ni logs;
  - `If-Match` y borrado de la entrada;
  - rotación desde otro proceso → el token anterior da 401;
  - bootstrap: dos conexiones dan dos tokens y el primero queda revocado;
  - **DACL efectiva del pipe `D:P(A;;FA;;;S-1-5-21-…-1001)(A;;FA;;;SY)`**;
  - ningún token en los logs; exit 0;
  - limpieza sin restos en Credential Manager.

## Pendiente y discrepancias

- **BLOCKED_REAL**: la prueba con un segundo usuario de Windows (que otra cuenta no pueda abrir el pipe ni leer los secretos) necesita una cuenta autorizada por el operador. La DACL efectiva ya está verificada.
- El contrato 07 fija el formato de evento `dominio.acción` pero no enumera eventos para credenciales ni aprobaciones. Se emiten `credential.created|rotated|deleted` y `approval.created` con ese formato.
- `admin` de API se interpreta como superconjunto de los scopes de API, porque el contrato lo lista como scope sin semántica adicional. `admin` MCP es solo comodín de herramientas (H06).
- El consumo de aprobaciones en operaciones destructivas reales llega con sus prompts (P016, P018, licencias); aquí está el mecanismo y sus pruebas.
