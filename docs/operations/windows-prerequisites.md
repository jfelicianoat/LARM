# Prerrequisitos de desarrollo en Windows

`tools/doctor.ps1` comprueba este equipo sin modificarlo: no instala nada ni cambia el PATH o el registro. Genera un informe legible y, con `-OutJson`, uno en JSON con PASS/WARN/FAIL por comprobación. Sale con 1 si hay algún FAIL.

```text
powershell -ExecutionPolicy Bypass -File tools/doctor.ps1 -OutJson doctor.json
```

## Qué comprueba y cómo resolverlo

| Comprobación | Nivel si falta | Paso manual exacto |
|---|---|---|
| MSVC (`Microsoft.VisualStudio.Component.VC.Tools.x86.x64`, vía `vswhere`) | FAIL | Visual Studio Installer → Build Tools → carga «Desarrollo para el escritorio con C++» (MSVC x64/x86 + Windows SDK). |
| Windows SDK (`KitsRoot10`) | FAIL | Visual Studio Installer → Componentes individuales → Windows 11 SDK. |
| WebView2 Runtime | FAIL | Instalar «Evergreen Bootstrapper» de https://developer.microsoft.com/microsoft-edge/webview2/. |
| `rustc` = versión de `environment.lock.json` | FAIL | Instalar rustup (https://rustup.rs). Al ejecutar `cargo` en el repo, `rust-toolchain.toml` instala 1.99.0 con `rustfmt`, `clippy` y `llvm-tools`. |
| Node.js = versión del lock | WARN (otra versión) / FAIL (ausente) | Instalar Node.js v24.18.0 LTS. |
| Python ≥ 3.11 | FAIL | Instalar Python 3.11+; las herramientas usan `.venv-tools` (`python -m venv .venv-tools` e instalar `tools/requirements.txt`). |
| Git | FAIL | Git for Windows. |
| Windows PowerShell 5.1 | FAIL | Incluido en Windows 10/11. `pwsh` 7 es opcional. |
| CMake, Ninja | WARN | Solo para runtimes con `SOURCE_BUILD`. Instalar y añadir al PATH **del usuario**. |
| Modo de programador | WARN | Configuración → Sistema → Para programadores → Modo de programador. |
| Symlinks sin privilegio | WARN | Se obtiene con el Modo de programador; sin él, las vistas de runtime usan hard links o copias. |
| Rutas largas (`LongPathsEnabled`) | WARN | Directiva «Habilitar rutas largas de Win32» (`gpedit.msc`) o la clave `LongPathsEnabled=1`. |

## Estado del PC IA (2026-10-11)

0 FAIL y 4 WARN: faltan CMake y Ninja, el modo de programador está desactivado y no se pueden crear symlinks sin privilegios. Hay MSVC de Build Tools 2019 y 2026, Windows SDK, WebView2 154, Rust 1.99.0, Node v24.18.0, Python 3.14.6, Git 2.55 y rutas largas activadas. Informe: [docs/reports/P008_runs/doctor.json](../reports/P008_runs/doctor.json).
