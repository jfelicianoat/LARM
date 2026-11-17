# P137 — Packaging Windows, User Background Agent y Windows Service opcional

Fecha: 2026-11-16.  
Estado: **BLOCKED_REAL** — el paquete portátil se construye, el User Background Agent (modo predeterminado, sin admin) arranca y sirve la API con autarranque real en la carpeta Startup del usuario, la desinstalación nunca borra datos y la purga es separada y confirmada — todo REAL sobre el PC IA; el servicio Windows opcional exige consola elevada y queda **NO VERIFICADO** sin ella (conducta esperada y documentada).

## Implementación

- **Modos de daemon** (`apps/daemon/src/windows.rs`, área indicada por el prompt; spec §101/§102/§128): el mismo Core en dos modos.
  - **User Background Agent (predeterminado)**: proceso del usuario **sin privilegios** (detectado `elevated=false` real). El autarranque escribe `LARM User Agent.cmd` en la **carpeta Startup del usuario** (`%APPDATA%\...\Startup`) con `start "LARM agent" /MIN` y los argumentos del contrato (`--serve-api --bind 127.0.0.1 --data-root … --config …`) — **sin `/RL HIGHEST` ni PATH global ni drivers ni WSL**. Se eligió la carpeta Startup en lugar de `schtasks /SC ONLOGON` porque el trigger ONLOGON **exige elevación** en este host (`Acceso denegado` real sin admin, verificado a mano) y el agente predeterminado no puede exigirla. Determinismo verificado por test.
  - **Windows Service (opcional)**: `sc create LARM binPath= "… --serve-api --data-root … --config …" start= demand DisplayName= …` — **sólo con comando explícito del operador y consola elevada** (`service_requires_elevation` si no), arranque **a demanda**, con **rutas separadas** del usuario: `ProgramData\LARM` por defecto para datos y config. El guard `service_data_root` **rechaza unidades mapeadas de sesión** (`service_session_drive_rejected`; sólo local o UNC — documentado con credenciales).
  - **Desinstalación** (`uninstall_plan`): quita agente, servicio y binarios; **nunca borra modelos ni la base**. La **purga es separada y confirmada**: `purge-data --confirm` (sin `--confirm` → `uninstall_purge_requires_confirmation`).
- **CLI del daemon** (subcomandos nuevos): `install-agent`, `uninstall-agent`, `install-service`, `uninstall-service`, `install-status` (JSON con `agent_task_registered`/`service_registered`/`elevated`/`mode_default`) y `purge-data --confirm`.
- **Shell Tauri** (`apps/desktop/src-tauri/src/commands.rs` + `lib.rs`, área indicada por el prompt): comando `larm_install_status` que consulta el estado real del equipo (autarranque del agente y servicio) para la GUI; instalar/desinstalar queda en el CLI del operador.
- **Packaging** (`tools/package_windows.py`): paquete portátil `dist/LARM-0.1.0-windows-x64.zip` con el binario del daemon, `docs/operations/*.md`, `VERSION` y `manifest.json` con SHA-256 por fichero + `SHA256SUMS` del zip. **Sólo copia ficheros** (spec P137.4: no modifica PATH global, drivers, WSL ni BIOS).
- **Guía de operación** (`docs/operations/install-uninstall.md`, área indicada por el prompt): los dos modos, rutas separadas del servicio y la nota UNC/credenciales, qué NO hace el instalador, desinstalación sin borrado de datos y purga separada, empaquetado.

## Evidencia real (PC IA, 2026-11-16)

`apps/daemon/tests/windows_agent.rs` **PASS** sobre el binario real con seis salidas marcadas `REAL P137:`: el agente **arranca y sirve la API** (`elevated=false`); el autarranque se registra y elimina en la **carpeta Startup real** del usuario (el `.cmd` contiene `--serve-api`); el servicio **sin elevación queda NO VERIFICADO** (`service_requires_elevation`); `purge-data` sin confirmación se **rechaza** y con `--confirm` borra la raíz de datos. Paquete portátil construido: `dist/LARM-0.1.0-windows-x64.zip` (18.603.980 bytes, `sha256=e804fc0d41cff5f6db97e590ee21af0b754028514793eef7e363e7c2df3739fa`). Evidence formal: **BLOCKED_REAL (3 PASS, 0 FAIL, 1 BLOCKED)** — CONTRACT modos 5/5 + REAL agente + REAL package build; el bloqueado es el servicio instalado y verificado unattended (exige consola elevada).

## Defectos detectados por la puerta formal y corregidos

1. **`desktop_fmt` FAIL**: el comando nuevo `larm_install_status` del shell Tauri no estaba formateado — `cargo fmt` del crate.
2. **Tests de cuota de P119 con fecha hardcodeada** (`cargo_test`, en dos sitios): `admit_registra_bloque_y_admite_dentro_de_cuota_diaria` (larm-db) y `flujo_real_notificacion_cuota_pequena_admision_y_bloqueo` (daemon REAL) afirmaban `day_key == "2026-11-16"` — se rompieron al cruzar la medianoche en las corridas del gate (2026-11-16 → 2026-11-17). El day_key esperado se deriva ahora del reloj UTC en ambos tests (el day_key real del contrato sigue siendo la fecha de la admisión; las llamadas deterministas al evaluador puro con fecha explícita no cambian).
3. Primer intento de autarranque con `schtasks /SC ONLOGON`: **exige elevación** en este host (`Acceso denegado` real sin admin) — sustituido por la carpeta Startup del usuario antes de la puerta.

## Verificación

- Modos de daemon 5/5; REAL PASS con seis marcadores; paquete portátil construido con hash; clippy `-p larm-daemon --all-targets -D warnings`, clippy del shell Tauri y fmt limpios. Evidence formal **BLOCKED_REAL (3 PASS, 0 FAIL, 1 BLOCKED)**; dependencias P132/P131/P133 con informe (BLOCKED_REAL — el contrato 00 exige REAL_PASS para el REAL_PASS del consumidor).
