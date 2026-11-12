# P002 — Reglas de arquitectura, ADR y contratos de dependencia

**Revalidado**: 2026-10-11 · **Estado**: **BLOCKED_REAL** — 9/9 PASS; bloqueado por la dependencia P001. [evidence.json](P002_reglas-de-arquitectura-adr-y-contratos-de-dependencia.evidence.json)

## Revalidación

- **El PASS anterior no demostraba nada**: `gen_p002_docs.py` reemplazó `crate-graph.json` por un grafo propio (con `larm-core` como dependencia de casi todo) y el verificador validaba contra él. Contrato restaurado y generador eliminado.
- El verificador **nunca podía detectar** dependencias prohibidas en `larm-domain`, porque solo miraba crates `larm-*`. Reescrito:
  - traduce las categorías del contrato (IO/SQLx/Axum/Tauri/Windows) a crates concretos;
  - busca `std::fs/net/process` en el código del dominio;
  - comprueba las dependencias hacia `apps/*` y la frontera GUI;
  - verifica también el shell Tauri.
- Con el contrato real apareció **una violación**: `larm-runtime → larm-core` (rebanada P107). Los traits de puertos pasan a `larm_domain::ports` (`larm-core::bridge` los reexporta) y el workspace cumple el grafo.
- `docs/architecture/dependencies.md` documentaba el grafo inventado. Ahora se genera desde el contrato, con `--check` contra deriva.
- ADR-0007 no recogía los milisegundos ni la `Z` del contrato 00 (ni bytes como string, RFC 8785 o SHA256 en minúsculas). ADR-0008 decía `protocol_version = 1` cuando el contrato 03 fija `"1.0"`. Corregidos ambos.
- `verify_p002.py conventions` comprueba que el código cumple los ADR. Detectó un `to_rfc3339()` en `larm-db`, corregido en P004.

## Pruebas

- CONTRACT: graph-pristine, dependencies-doc, adrs, conventions.
- FAULT: 7 workspaces reales temporales, uno por violación.
- UNIT: fmt/clippy/test.
- REAL: `cargo metadata` real del workspace y del shell.
