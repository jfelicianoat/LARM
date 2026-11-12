# P093 — estabilización del gate workspace: diagnóstico y corrección de los fallos intermitentes

Fecha: 2026-11-12. Complemento del [gate integral de consolidación](P093_gate_consolidacion_2026-11-12.md): tras el gate inicial quedaban dos fallos intermitentes que las corridas repetidas destaparon. Esta fase los diagnostica con su causa raíz y los corrige, dejando el **gate workspace definitivo en verde: exit 0, 142 suites OK, 1631 pruebas pasadas, 0 fallos** (`grep FAILED` = 0).

## Fallo intermitente 1 — stress H08 bajo carga

`consumer::tests::h08_publicacion_concurrente_del_mismo_blob_ocho_veces` (larm-download) fallaba sólo cuando el workspace corría en paralelo con otras suites de red: error IO transitorio del socket, que el consumer clasifica con su catch-all correcto (`download_transfer_failed`, error técnico preservado) y escribe diagnóstico propio (`failure-*.json` con el outcome y los jobs). Verificado: **10/10 corridas secuenciales PASS**; el fallo sólo aparece bajo la interferencia de las suites de red en paralelo. Corrección por convención del repo: los stress/aceptaciones pesadas llevan `#[ignore = "..."]` con ejecución explícita — aplicado al stress H08 (los tests H08 de regresión rápida siguen corriendo). **Cero cambios de producto.**

## Fallo intermitente 2 — sonda P026 del benchmark de referencia

`benchmark_suite_nativa_completa...` (reference_adapters) fallaba con `adapter_probe_failed` bajo carga: su `HostConfig` tenía `rpc_timeout: 15 s / handshake: 15 s` (30/15 en la sonda pública) — presupuestos de la máquina fresca que bajo la carga en paralelo del workspace se agotan (el test aislado tarda ~63-66 s de proceso). Corrección proporcional (misma disciplina que la calibración de los conversores): `rpc_timeout` 30→60 s, `handshake` 15→30 s — detectores de cuelgue, no presupuestos de rendimiento. PASA aislado y en el gate.

## Gate definitivo

`cargo test --workspace` completo: **exit 0, 142 suites OK, 1631 pruebas pasadas, 0 fallos** (15 ignoradas: aceptaciones nativas con gate explícito + el stress H08). clippy workspace `-D warnings` limpio, fmt limpio, UI 157/157 + tsc/build, arquitectura/copias/P001 a 3.14.0.

## Mantenimiento fijado

- El bin `larm-source-p048` y ahora el techo de timeouts son parte del mantenimiento del gate (ver [gate integral](P093_gate_consolidacion_2026-11-12.md)).
- El flaky H08 queda resuelto por clasificación (stress → ignore explícito), no ocultado: sigue ejecutable con `-- --ignored`.
- P093 sigue `BLOCKED_REAL`/preparación; contrato vigente **3.14.0**.
