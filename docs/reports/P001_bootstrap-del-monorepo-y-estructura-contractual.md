# P001 — Bootstrap del monorepo y estructura contractual

**Revalidado**: 2026-10-11 · **Host**: A9_MEGA (PC IA) · **Estado**: **BLOCKED_REAL** — 22/22 pruebas propias PASS (5 reales); bloqueado solo por la dependencia P000 (BLOCKED_REAL de entorno). [evidence.json](P001_bootstrap-del-monorepo-y-estructura-contractual.evidence.json) · regenerar con `tools/check.ps1 -Prompt P001 -Real`.

## Revalidación del trabajo heredado

| Hallazgo | Corrección |
|---|---|
| Faltaban las cinco carpetas `adapters/{runtime,source,protocol,transformation,benchmark}` que el informe daba por creadas | creadas, con README por familia |
| `docs/contracts/crate-graph.json` (contrato vinculante) **sobrescrito** con un grafo inventado por `tools/gen_p002_docs.py` | restaurado del paquete; ver P002 |
| No se copiaron `contracts/external/*` (snapshots obligatorios), `manifest.json`, spec original/baseline ni el índice | `tools/package_copy.py sync`: 53 ficheros con sha256; `check` falla ante cualquier copia alterada o ausente |
| `apps/daemon/tests/lifecycle.rs` no probaba nada (comprobaba un texto del Cargo.toml) | ejecuta el binario: `--help` → 0; config inválida o inexistente → 2 sin escribir logs |
| Test de frontend con **stub exitoso**: fuera de Tauri `ping` devolvía «pong» y el test lo aceptaba | fuera de Tauri se dice que no hay backend; tests con `mockIPC` (éxito, error, sin Tauri) |
| El shell Tauri tenía 0 tests y el `ping` real nunca se ejecutó | test unitario + **ping real por WebView2** (`LARM_DESKTOP_SMOKE=1` → `pong`, exit 0) |
| `tauri.conf.json` con `--prefix ..`: `npm run tauri:dev` y `tauri build` no funcionaban | comandos `before*` corregidos; `tauri:dev` y `tauri build` probados |
| `daemon:dev` no reenviaba argumentos | `cargo run -p larm-daemon --` |
| La prueba del ciclo de vida escribía en el `%LOCALAPPDATA%\LARM` real | raíz de datos gestionada y comprobación de su log |
| `check.ps1` no cubría el shell Tauri y llamaba a scripts sueltos | puertas del workspace y del shell; `-Real` ejecuta `tools/<prompt>_evidence.py` |

## Pruebas (todas PASS)

- CONTRACT: estructura, scripts, políticas (`unsafe` prohibido y lints heredados, toolchain 1.99.0 activo), copia del paquete.
- FAULT: copia manipulada y snapshot borrado, detectados.
- UNIT: fmt, clippy `-D warnings`, tests del workspace y del shell; lint/typecheck/test/build.
- INTEGRATION: binario del daemon.
- REAL: build del daemon, ciclo de vida (CTRL_BREAK → exit 0), `npm run daemon:dev`, `tauri build`, ping real en la ventana, `check.ps1` en PowerShell 5.1.

Nota del modo npm: CTRL_BREAK llega también a node y cargo, que salen con `0xC000013A`. El oráculo es el «cierre limpio» registrado por el propio daemon; su exit 0 lo prueba el modo exe.
