# Instalación, agente de fondo y desinstalación de LARM en Windows (P137)

## Dos modos de daemon (spec §101, §102)

El mismo Core (`larm-daemon`) funciona en los dos modos; cerrar la GUI nunca
detiene los runtimes gestionados (§128):

- **User Background Agent (predeterminado)** — se ejecuta como el usuario,
  **sin privilegios de administrador**: acceso natural a sus carpetas y a sus
  unidades mapeadas. Se registra como tarea de logon **del propio usuario**:

  ```text
  larm-daemon --data-root <raíz> --config <config> install-agent
  ```

  Crea la tarea de logon `LARMUserAgent` con `schtasks` (`/SC ONLOGON`, sin
  `/RL HIGHEST`). No toca el PATH global, drivers, WSL ni el BIOS: la
  instalación **sólo copia ficheros** y registra la tarea del usuario.

- **Windows Service (opcional)** — sólo para funcionamiento unattended, y
  **sólo mediante instalación explícita y elevada**:

  ```text
  larm-daemon --data-root <raíz-del-servicio> install-service   (consola elevada)
  larm-daemon uninstall-service                                  (consola elevada)
  ```

  Sin elevación el comando falla con `service_requires_elevation` y el
  servicio queda **NO VERIFICADO** (comportamiento esperado, no defecto).
  Arranque a demanda (`start= demand`): el operador decide cuándo arrancarlo.

## Rutas separadas del servicio (spec §102)

El agente de usuario vive en `%LOCALAPPDATA%\LARM` (datos y config del
usuario). El servicio usa rutas **separadas**: por defecto
`C:\ProgramData\LARM` para datos y config, o las que el operador pase con
`--data-root`/`--config`. **El servicio no puede depender de unidades
mapeadas de sesión** (no sobreviven al contexto del servicio): su raíz debe
ser una ruta **local o UNC** — una unidad de red montada con letra se
rechaza en la instalación (`service_session_drive_rejected`). Para recursos
remotos usa UNC con credenciales del servicio (cuenta del servicio o
`cmdkey` persistida del equipo); documéntalo en la ubicación de
almacenamiento correspondiente (P014).

## Qué NO hace el instalador (spec P137.4)

El instalador no modifica drivers, WSL, BIOS ni el PATH global; los
prerrequisitos estándar (WebView2 Runtime) se instalan aparte y de forma
explícita por el operador — ver
[windows-prerequisites.md](windows-prerequisites.md). `tools/doctor.ps1`
comprueba el equipo sin modificarlo.

## Desinstalación (spec P137.5)

```text
larm-daemon uninstall-agent       # quita la tarea de logon del usuario
larm-daemon uninstall-service     # quita el servicio (consola elevada)
```

La desinstalación **nunca borra modelos ni la base de datos**: sólo quita
tarea, servicio y binarios. La **purga de datos es una operación separada y
confirmada**:

```text
larm-daemon --data-root <raíz> purge-data --confirm
```

Sin `--confirm` falla con `uninstall_purge_requires_confirmation`. Antes de
una reinstalación o migración, usa el backup verificado de P133
([backup-restore.md](backup-restore.md)).

## Empaquetado

- **GUI**: `npm run tauri build` genera el instalador NSIS de la GUI (el
  instalador de la GUI no registra servicios ni tareas).
- **Paquete portátil del daemon**: `tools/package_windows.py` construye
  `dist/LARM-<versión>-windows-x64.zip` con el binario del daemon, esta
  documentación, `VERSION` y `SHA256SUMS`; sólo copia ficheros.

## Comprobación

```text
larm-daemon install-status
```

Devuelve si la tarea del agente y el servicio están registrados y si la
consola está elevada (`mode_default: user-background-agent`).
