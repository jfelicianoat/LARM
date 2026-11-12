# Fronteras de dependencias entre crates

Generado por `tools/gen_dependencies_doc.py` desde [crate-graph.json](../contracts/crate-graph.json),
copia íntegra del contrato del paquete v3. No editar a mano: cambiar el grafo exige nueva revisión del paquete.
Verificación: `tools/check_architecture.ps1` contra `cargo metadata` del workspace y del shell Tauri.

## Grafo permitido

Cada crate solo puede depender (normal, dev o build) de los indicados. Lo no listado está prohibido.

| Crate | Puede depender de | Lo usan |
|---|---|---|
| `larm-domain` | — | `larm-adapter-host`, `larm-adapter-sdk`, `larm-api`, `larm-benchmark`, `larm-core`, `larm-db`, `larm-download`, `larm-events`, `larm-hardware`, `larm-models`, `larm-protocol`, `larm-runtime`, `larm-storage`, `larm-transform` |
| `larm-db` | `larm-domain` | `larm-benchmark`, `larm-download`, `larm-events`, `larm-hardware`, `larm-models`, `larm-runtime`, `larm-storage`, `larm-transform` |
| `larm-adapter-sdk` | `larm-domain` | `larm-adapter-host`, `larm-benchmark`, `larm-core`, `larm-models`, `larm-protocol`, `larm-runtime` |
| `larm-events` | `larm-domain`, `larm-db` | `larm-adapter-host`, `larm-benchmark`, `larm-download`, `larm-models`, `larm-protocol`, `larm-runtime`, `larm-storage`, `larm-transform` |
| `larm-core` | `larm-domain`, `larm-adapter-sdk` | `larm-api` |
| `larm-api` | `larm-domain`, `larm-core` | — |
| `larm-hardware` | `larm-domain`, `larm-db` | `larm-runtime` |
| `larm-storage` | `larm-domain`, `larm-db`, `larm-events` | `larm-benchmark`, `larm-download`, `larm-models`, `larm-runtime`, `larm-transform` |
| `larm-adapter-host` | `larm-domain`, `larm-adapter-sdk`, `larm-events` | `larm-download`, `larm-protocol`, `larm-runtime`, `larm-transform` |
| `larm-protocol` | `larm-domain`, `larm-adapter-sdk`, `larm-adapter-host`, `larm-events` | `larm-benchmark`, `larm-runtime` |
| `larm-models` | `larm-domain`, `larm-db`, `larm-adapter-sdk`, `larm-storage`, `larm-events` | `larm-benchmark`, `larm-download`, `larm-transform` |
| `larm-runtime` | `larm-domain`, `larm-db`, `larm-hardware`, `larm-storage`, `larm-adapter-sdk`, `larm-adapter-host`, `larm-protocol`, `larm-events` | `larm-benchmark`, `larm-transform` |
| `larm-download` | `larm-domain`, `larm-db`, `larm-models`, `larm-storage`, `larm-adapter-host`, `larm-events` | `larm-transform` |
| `larm-transform` | `larm-domain`, `larm-db`, `larm-models`, `larm-storage`, `larm-download`, `larm-runtime`, `larm-adapter-host`, `larm-events` | — |
| `larm-benchmark` | `larm-domain`, `larm-db`, `larm-runtime`, `larm-models`, `larm-protocol`, `larm-storage`, `larm-adapter-sdk`, `larm-events` | — |

## Reglas

- **Raíz de composición**: `apps/daemon` puede depender de todos los crates; ningún crate depende de `apps/*`.
- **Dominio puro**: `larm-domain` no depende de IO, SQLx, Axum, Tauri, Windows. El verificador
  traduce cada categoría a crates concretos (p. ej. IO → tokio, reqwest, tempfile; Windows → windows, winreg)
  y rechaza además `std::fs`, `std::net` y `std::process` en su código.
- **GUI**: HTTP administrative API only; Tauri bootstraps local endpoint/auth securely. `apps/desktop` no depende de ningún crate interno.
- **Puertos**: los traits que implementa un crate que no puede ver `larm-core` (p. ej. `larm-runtime`) viven en
  `larm-domain::ports`; `larm-core` los reexporta para `larm-api`.
