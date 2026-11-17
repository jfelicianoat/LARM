# P131 — Secretos, auth local y permisos API

Fecha: 2026-11-16.  
Estado: **BLOCKED_REAL** — el rate limit de fallos de autenticación, la matriz H06 completa con concesiones distintas y admin nunca implícito, la rotación/revocación con eventos de auditoría y el Credential Manager del sistema operativo, y la credencial dedicada del AI Broker están completos y PASS; el arranque/parada e2e con lease ACTIVE y el cliente remoto real con TLS exigen dependencias en REAL_PASS (contrato 00).

## Gap y decisiones

Las partes ya implementadas por fases anteriores se verificaron y se citan como evidencia: la credencial dedicada del AI Broker (`PrincipalKind::Broker` con token en el backend seguro y referencia `larm/principal/{id}` sin token, P009), el pipe de bootstrap del GUI Tauri (el WebView nunca recibe el token ni la URL: el backend Rust lo obtiene por pipe de un uso y reintenta ante 401 por rotación de otra sesión), el opt-in remoto (`remote_enabled=false` por defecto en defaults.json y el arranque aborta con bind no loopback sin opt-in), y la rotación/revocación del CLI del operador (`larm-daemon enroll/rotate/revoke`). El único requisito sin implementación era el **rate limit de fallos de auth** (4) y la aceptación de la **matriz H06 completa** (oráculo del contrato 11).

Decisión de contrato: la respuesta del bloqueo por rate limit usa **`AUTH_REQUIRED` (401) del catálogo sellado del contrato 07** con la razón `auth_rate_limited` y cabecera `Retry-After` — el catálogo de errores no tiene código 429 y añadirlo habría exigido revisión contractual para un comportamiento que el prompt no concreta.

## Implementación

- **Rate limiter** (`crates/larm-api/src/auth.rs`, área indicada por el prompt): `AuthRateLimiter` con ventana deslizante de 60 s y **10 fallos por fuente** (IP remota del socket; `local` como clave de respaldo en routers aislados). Tras el décimo fallo la fuente queda bloqueada — **incluso con un token válido** — hasta que el fallo más antiguo salga de la ventana; un éxito auténtico resetea el historial. Sin estado contractual nuevo; el bloqueo viaja como `AUTH_REQUIRED` 401 + `auth_rate_limited` + `Retry-After`. Tests deterministas 6/6 (ventana, liberación, reset, fuentes independientes, recarga de la ventana).
- **Guard** (`crates/larm-api/src/security.rs`): lee el limiter como extensión (el daemon lo inserta; los routers aislados de pruebas siguen sin él y su conducta no cambia), registra `bearer_invalid` como fallo — un Bearer ausente no cuenta (no intentó credencial) — y resetea el historial ante un éxito.
- **Cableado**: `AppState.auth_limiter` + capa `Extension` en `app.rs` (el daemon comparte una instancia por proceso); `serve.rs` construye el limiter. El guard lee la IP de origen de la extensión `ConnectInfo` (el servicio real se sirve con `into_make_service_with_connect_info`; cae a `local` si el router no la expone).
- **Matriz H06 REAL** (`apps/daemon/tests/h06_matrix.rs`): el oráculo del contrato 11 completo contra el servidor MCP de administración con **7 principales reales enrolados** y un despachador spy que registra cada llamada hacia el **Core real en loopback** (hardware + jobs con guard real): start despacha `POST /api/v1/leases` y stop sin concesión es `mcp_permission_missing` sin despachar; install despacha y update sin concesión FORBIDDEN; read y search son distintas **en ambas direcciones** (el lector no busca, el buscador no lee); move/delete son destructivas con `confirmation_required` sin despachar y se despachan con `confirm=true` con concesiones distintas; el comodín MCP `admin` **nunca añade scopes API** (`scope_missing` sin despachar en tres herramientas); audit NDJSON con cada herramienta, su resultado y sin tokens.
- **Credenciales REAL** (`apps/daemon/tests/auth_security.rs`): rate limit sobre **TCP real con IP de origen** (bloqueo tras 10 fallos, reset por éxito) y rotación/revocación reales contra **SQLite migrado** con `SqlCredentials` + **Windows Credential Manager** (el backend seguro del daemon): el token vive sólo en el SO (la referencia `larm/principal/{id}` no lo contiene y ninguna fila de la base lo contiene), la rotación invalida el anterior inmediatamente y actualiza el Credential Manager, y los eventos `credential.created`/`credential.rotated`/`credential.deleted` quedan en la base real; la revocación borra el secreto del SO.

## Evidencia real (PC IA, 2026-11-16)

`h06_matrix.rs` **PASS** con seis salidas marcadas (`REAL H06: …`): las cuatro parejas de concesiones distintas, las destructivas con confirmación y `admin` sin scopes API. `auth_security.rs` **PASS** con seis salidas marcadas (`REAL P131: …`): bloqueo real de la fuente TCP, reset por éxito, secreto sólo en el Credential Manager, eventos de auditoría en SQLite y revocación que borra del SO. Evidence formal: **BLOCKED_REAL (4 PASS, 0 FAIL, 2 BLOCKED)** — CONTRACT limiter 6/6 + guard/router 9/9; los bloqueados son el arranque/parada e2e con lease ACTIVE (P100/P031/P099 en BLOCKED_REAL) y el bind remoto con cliente remoto real y TLS del sistema.

## Nota sobre requisitos existentes verificados

- **AI Broker**: credencial dedicada con `PrincipalKind::Broker` y scopes API propias (`read` + `leases:manage` en la aceptación de P009; el principal se autentica y la referencia no contiene el token) — test `alta_autentica_y_el_token_no_se_guarda_en_la_referencia`.
- **GUI Tauri sin secreto en JS**: el WebView llama a `api_request` del backend Rust, que obtiene el token del principal `desktop` por el pipe de bootstrap (un uso, SID-only) y reintenta una vez ante 401 por rotación de otra sesión.
- **Remote bind opt-in**: `remote_enabled=false` por defecto; `serve.rs` aborta el arranque con `bind {} no es loopback y remote_enabled=false`.
- **No secret in logs**: el audit MCP no guarda tokens (verificado en la matriz) y el token no va por URL, línea de comandos ni log en el bootstrap del pipe.

## Verificación

- Limiter 6/6; guard/router 9/9 (suite security completa); larm-api completa 25 suites OK; security de larm-core 13/13; H06 REAL PASS; credenciales REAL PASS; clippy `-p larm-api -p larm-daemon -p larm-core --all-targets -D warnings` y fmt limpios. Evidence formal **BLOCKED_REAL (4 PASS, 0 FAIL, 2 BLOCKED)**; dependencias P045/P005/P104/P009 con informe (BLOCKED_REAL — el contrato 00 exige REAL_PASS para el REAL_PASS del consumidor).
- **Puerta formal: `tools/check.ps1 -Prompt P131 -Real` → STATUS PASS (15 checks, 0 fallos)** en la primera corrida, con recibo en `check_P131_real.json`.
