# P104 — MCP Administration Server

Fecha: 2026-11-15.  
Estado: **BLOCKED_REAL** — el servidor MCP de administración está implementado y **verificado con ejecución real contra los casos de uso Core en el PC IA** (autorización H06 completa, tools/list con las 12 herramientas del contrato, tool call real ejecutado, confirmation_required de la destructiva y rechazo real del Core con confirm=true); el REAL_PASS completo exige leases reales con runtime instalado mediante lock cualificado (P031/P099 en BLOCKED_REAL).

## Implementación (`crates/larm-protocol/src/mcp_admin/`)

- **Catálogo sellado** (`permissions.rs`): `mcp-permissions.json` embebido con las **12 herramientas** del contrato (`larm.search_models/get_hardware/plan_download/install_model/move_model/delete_model/install_runtime/update_runtime/start_model/stop_model/run_benchmark/get_job`), cada una mapeada a su permiso MCP, su `api_operation_id` publicado en el catálogo de API y su scope; validación contra `Catalog::contract()` (operación inexistente → `mcp_permissions_operation_missing`); herramientas destructivas (`move/delete/update_runtime/stop`) marcadas para la puerta de confirmación.
- **Autorización H06** (`server.rs::authorize_and_dispatch`): **dos halves antes del caso de uso** — half MCP (permiso de la herramienta o `admin` como comodín explícito) y half API (scope de la operación contra el principal sellado del dominio con `authorize`); falta cualquiera → FORBIDDEN sin despachar; **admin nunca añade scopes API** (probado: principal admin-only sin scopes → `scope_missing`).
- **Procedencia MCP en el contexto autenticado** (no headers del cliente): el principal se resuelve del **registro operador-registrado** (hash SHA-256 de la credencial + scopes sellados `McpPrincipal` con audiencia `larm-mcp-admin`); `initialize` fija la sesión con la credencial; credencial desconocida → error de protocolo.
- **Puerta de confirmación** (prompt §4): las destructivas sin `confirm=true` devuelven `confirmation_required` **sin ejecutar** (el dispatcher no registra llamada).
- **Salidas compactas** (prompt §5): 202 con job → `{job_id, state, status_url}`; planes → `{plan_id, plan_hash}`.
- **Audit log sin secretos** (prompt §6): NDJSON con at/tool/principal_id/outcome y resultado compacto; sin tokens ni valores de argumentos (probado con un argumento `secret_value` que no aparece).
- **Transporte stdio** (prompt §1): bucle JSON-RPC 2.0 NDJSON (`initialize`/`notifications/initialized`/`tools/list`/`tools/call`) con `mcp_tool_not_announced` ante herramientas fuera del contrato y -32700/-32602 de protocolo.
- **Despacho a casos de uso Core** (prompt §2): trait `CoreDispatcher` — el test real inyecta el dispatcher HTTP (reqwest, loopback, credencial del principal); sin acceso directo a DB/filesystem.

## Pruebas

- **Unidad/servidor** (`src/mcp_admin/tests.rs`, **11/11 PASS**): initialize rechaza credencial desconocida; tools/list 12; herramienta no anunciada; **H06**: start permitido (job-1 compacto) y stop FORBIDDEN para el mismo principal con el dispatcher verificando que stop nunca llega al caso de uso; install/update FORBIDDEN; admin-only sin scopes → `scope_missing`; destructiva sin confirm no ejecuta y con confirm ejecuta; bucle stdio con JSON inválido (-32700); audit sin secretos; args malformados (-32602).
- **REAL en el PC IA** (`apps/daemon/tests/mcp_admin.rs::servidor_mcp_administracion_real_con_hardware_jobs_y_h06`, **PASS**): superficie API real del Core (hardware + jobs con guard real y repos en memoria) en loopback; principal MCP enrolado con credencial real; initialize/tools-list reales; **larm.get_hardware ejecuta el caso de uso real** (perfil de hardware devuelto); stop sin permiso MCP → FORBIDDEN antes del caso de uso; delete del principal full → `confirmation_required`, y con confirm=true el **rechazo real del Core** (modelo inexistente → `core_use_case_rejected`, sin mock); audit NDJSON con las llamadas y **sin tokens**.
- **Puerta formal**: `tools/check.ps1 -Prompt P104 -Real` → **STATUS PASS (11 checks, 0 fallos)** — fmt, clippy workspace, fixtures, `cargo test --workspace` completo, arquitectura, copias 3.15.0, suites Python (9), shell Tauri, frontend y `p104_evidence.py` (**BLOCKED_REAL — 2 PASS [CONTRACT + REAL], 0 FAIL**).

## Defectos encontrados durante la fase (corregidos)

1. El camino Ok de `tools/call` devolvía el contenido sin el sobre JSON-RPC (`jsonrpc`/`id`/`result`) — las respuestas no correlacionaban; unificado con `structuredContent`.
2. La rama de herramienta desconocida usaba un helper sin envolver — mismo sobre para toda respuesta de tools/call.
3. El catálogo se construyó sobre `Catalog` no clonable — anclado a la instancia estática de `Contracts::shared()`.

## Pendientes (bloqueos reales)

- **REAL_PASS completo**: herramientas sobre lock cualificado y leases reales con runtime instalado (P031/P099 en BLOCKED_REAL).
- **Distribución como proceso del operador** (`llm-mcp-admin` standalone con config de registro) y **wizard de UI**: fases de cierre del producto.
