# P008 — Prerrequisitos y packaging de desarrollo Windows

**Fecha**: 2026-10-11 · **Host**: A9_MEGA (PC IA) · **Estado**: **BLOCKED_REAL**: 3/3 PASS (2 reales); solo bloqueado por las dependencias P000/P001/P007. [evidence.json](P008_prerequisitos-y-packaging-de-desarrollo-windows.evidence.json) · [prerrequisitos](../operations/windows-prerequisites.md)

## Qué se construyó

`tools/doctor.ps1`, en PowerShell puro para funcionar aunque falte Python, con dos fases:

- **Recogida de solo lectura**: `vswhere` (MSVC), WebView2, Windows SDK, versiones de rustc/cargo/node/npm/python/git/cmake/ninja/pwsh, PowerShell, modo de programador, rutas largas y una prueba de symlink en una carpeta temporal propia que se borra.
- **Evaluación** frente a `environment.lock.json`, con PASS/WARN/FAIL y el paso manual exacto de cada problema. Con `-FromRaw` se evalúan salidas falsas.

No instala nada ni toca PATH ni registro: la prueba real compara ambos antes y después.

## Defectos encontrados al probar

- Falso **FAIL de MSVC** en el PC IA: en Windows PowerShell 5.1, `ConvertFrom-Json` devuelve el array de `vswhere` como un único objeto y el filtro evaluaba la colección entera. La prueba de mutación confirma que el caso «todo bien» lo detecta.
- `mklink` sin privilegio escribe en stderr, lo que abortaba el script con `ErrorActionPreference=Stop`. Ese error es el dato que se mide, no un fallo del doctor.

## Resultado en el PC IA

0 FAIL y 4 WARN:

- CMake y Ninja ausentes: los necesitarán los runtimes con `SOURCE_BUILD`.
- Modo de programador desactivado y symlinks no disponibles: las vistas de runtime usarán hard links o copias.

El resto está en PASS: MSVC 2019 y 2026, Windows SDK, WebView2 154, Rust 1.99.0, Node v24.18.0, Python 3.14.6, Git 2.55, PowerShell 5.1 y rutas largas.
