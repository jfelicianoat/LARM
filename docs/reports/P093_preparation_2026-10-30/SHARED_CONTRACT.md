# Contrato compartido de desarrollo LARM — revisión3

LARM administra capacidad local en Windows/PC IA. AI Broker en PC IA conserva routing y gateway de inferencia. Agora coordina tareas; Local AI Lab y Model Drift evalúan en PC principal. No crear chat, RAG, router de negocio ni evaluador funcional dentro de LARM.

Stack: Rust/Tokio,Axum JSON/SSE,Tauri2 React/TypeScript,SQLite SQLx WAL/foreign keys,reqwest/rustls,tracing; npm. Versiones exactas resueltas y bloqueadas en P000/P001 según §127, no latest mutable.

## Estructura obligatoria

```text
/
├─ Cargo.toml
├─ rust-toolchain.toml
├─ package.json
├─ package-lock.json
├─ .gitignore
├─ .editorconfig
├─ apps/
│  ├─ daemon/
│  └─ desktop/
├─ crates/
│  ├─ larm-domain/
│  ├─ larm-core/
│  ├─ larm-db/
│  ├─ larm-api/
│  ├─ larm-hardware/
│  ├─ larm-storage/
│  ├─ larm-adapter-sdk/
│  ├─ larm-adapter-host/
│  ├─ larm-runtime/
│  ├─ larm-models/
│  ├─ larm-download/
│  ├─ larm-transform/
│  ├─ larm-benchmark/
│  ├─ larm-protocol/
│  └─ larm-events/
├─ adapters/
│  ├─ runtime/
│  ├─ source/
│  ├─ protocol/
│  ├─ transformation/
│  └─ benchmark/
├─ migrations/
├─ docs/
│  ├─ spec/
│  ├─ adr/
│  ├─ api/
│  ├─ adapters/
│  ├─ operations/
│  └─ reports/
├─ tests/
│  ├─ fixtures/
│  ├─ integration/
│  └─ e2e/
└─ tools/
```

Añadir docs/contracts,docs/implementation y carpetas de las cinco familias que aparecen arriba. contracts/crate-graph.json fija dependencias; traits de casos de uso viven en domain/core, composición en daemon; GUI usa API sin lógica de negocio.

## Contratos vinculantes

- [contracts/00_PACKAGE.md](contracts/00_PACKAGE.md)
- [contracts/01_DOMAIN_STATES.md](contracts/01_DOMAIN_STATES.md)
- [contracts/02_STORAGE_DOWNLOAD.md](contracts/02_STORAGE_DOWNLOAD.md)
- [contracts/03_ADAPTERS_SECURITY.md](contracts/03_ADAPTERS_SECURITY.md)
- [contracts/04_RUNTIME_LEASES.md](contracts/04_RUNTIME_LEASES.md)
- [contracts/05_BENCHMARK_FINGERPRINT.md](contracts/05_BENCHMARK_FINGERPRINT.md)
- [contracts/06_ECOSYSTEM.md](contracts/06_ECOSYSTEM.md)
- [contracts/07_API_EVENTS_MCP.md](contracts/07_API_EVENTS_MCP.md)
- [contracts/08_UI_RELEASE.md](contracts/08_UI_RELEASE.md)
- [contracts/09_EXTERNAL_PROFILES.md](contracts/09_EXTERNAL_PROFILES.md)
- [contracts/10_MULTIMODAL_PROTOCOL.md](contracts/10_MULTIMODAL_PROTOCOL.md)
- [contracts/11_CONFORMIDAD_V3.md](contracts/11_CONFORMIDAD_V3.md)
- [permisos MCP](contracts/mcp-permissions.json)

Tipos: [domain](schemas/domain.schema.json) y [API/SDK](schemas/larm.schema.json). Persistencia: [DDL](contracts/persistence.sql). Operaciones: [API](contracts/api-operations.json), [SDK](contracts/adapter-methods.json), [capacidades](contracts/capability-contracts.json). Valores: [defaults](contracts/defaults.json). Fixtures: [suites](fixtures/benchmark-suites.json).

## Avance y pruebas

Leer todas las entradas obligatorias del prompt; ausencia es BLOCKED. Contratos/versiones/algoritmos públicos se aplican tal cual. ADR no modifica la spec ni las puertas de aceptación. Verificar dependencias con evidencia: CODE_PASS permite preparación independiente; completar/avanzar un consumidor requiere REAL_PASS de dependencias. Un mock o CI no sustituye pruebas reales. Reportar BLOCKED_REAL cuando falten equipo, credenciales, modelo, volumen o autorización requerida.

Antes de cierre: fmt,clippy -D warnings,cargo test y pruebas de módulo; frontend lint,typecheck,test,build cuando afectado; tools/check.ps1 -Prompt PXXX -Real. Report y evidence.json por prompt con archivos,tests exactos,hashes,versiones,entorno,negativos,pruebas reales y pendientes. P138 exige todos los módulos y15 escenarios reales.

Permisos: trabajo en repo y carpetas gestionadas autorizado por implementación. No descartar cambios del usuario. No modificar instalaciones externas ni configuración global, borrar modelos personales, aceptar licencias, abrir LAN o publicar cambios sin autorización del operador. Usar autorización ya existente sin volver a pedirla. Una prueba impedida queda pendiente, nunca PASS por sustitución mock.

No stubs exitosos,TODO/FIXME/unimplemented/panic injustificado en producción. IO con timeout/cancel,original_error seguro,secretos en Credential Manager,rutas reales validadas,comandos executable+args,estados durables+journal/outbox.
