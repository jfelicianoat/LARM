@echo off
setlocal
cd /d "%~dp0"
if errorlevel 1 (
  echo No se pudo abrir la carpeta de LARM.
  pause
  exit /b 1
)

where cargo >nul 2>nul
if errorlevel 1 (
  echo No se encontro Cargo. Instala Rust y vuelve a intentarlo.
  pause
  exit /b 1
)
where npm >nul 2>nul
if errorlevel 1 (
  echo No se encontro npm. Instala Node.js y vuelve a intentarlo.
  pause
  exit /b 1
)
if not exist "node_modules\" (
  echo Faltan las dependencias de la interfaz. Ejecuta npm ci en la carpeta de LARM.
  pause
  exit /b 1
)
if /I "%~1"=="--check" (
  echo Requisitos de inicio encontrados.
  exit /b 0
)

echo Comprobando el daemon de LARM...
powershell.exe -NoProfile -ExecutionPolicy Bypass -Command "try { $r = Invoke-RestMethod -Uri 'http://127.0.0.1:8790/api/v1/health/live' -TimeoutSec 2; if ($r.status -eq 'LIVE' -and $r.schema_version -eq '1') { exit 0 } } catch {}; exit 1" >nul 2>nul
if errorlevel 1 (
  echo Preparando el daemon optimizado de LARM...
  cargo build --release -p larm-daemon
  if errorlevel 1 (
    echo No se pudo preparar el daemon. Revisa los mensajes anteriores.
    pause
    exit /b 1
  )
  echo Iniciando el daemon. Su ventana mostrara los mensajes de estado.
  start "LARM - Daemon" /D "%~dp0" cmd.exe /k "cargo run --release -p larm-daemon -- --serve-api"
  echo Esperando a que la API este disponible...
  powershell.exe -NoProfile -ExecutionPolicy Bypass -Command "$deadline = (Get-Date).AddMinutes(10); do { try { $r = Invoke-RestMethod -Uri 'http://127.0.0.1:8790/api/v1/health/live' -TimeoutSec 2; if ($r.status -eq 'LIVE' -and $r.schema_version -eq '1') { exit 0 } } catch {}; Start-Sleep -Milliseconds 500 } while ((Get-Date) -lt $deadline); exit 1"
  if errorlevel 1 (
    echo El daemon no respondio en 10 minutos. Revisa la ventana LARM - Daemon.
    pause
    exit /b 1
  )
) else (
  echo El daemon ya estaba activo; se reutilizara.
)

echo Iniciando la interfaz de LARM...
npm run tauri:dev
set "LARM_EXIT_CODE=%ERRORLEVEL%"
echo.
echo La interfaz se ha cerrado. Si este fichero inicio el daemon, puedes cerrar su ventana.
exit /b %LARM_EXIT_CODE%
