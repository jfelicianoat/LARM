# tools/check_architecture.ps1 - Verifica el grafo de crates contra docs/contracts/crate-graph.json.
# Comprueba el workspace y el shell Tauri (workspace propio). -SelfTest ejecuta los negativos.
param([switch]$SelfTest)

$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent $PSScriptRoot
Set-Location $repo
$python = Join-Path $repo '.venv-tools/Scripts/python.exe'
if (-not (Test-Path $python)) { $python = 'python' }

if ($SelfTest) {
    & $python tools/check_architecture.py --selftest
    exit $LASTEXITCODE
}
& $python tools/check_architecture.py
$workspace = $LASTEXITCODE
& $python tools/check_architecture.py --root apps/desktop/src-tauri
if ($workspace -ne 0 -or $LASTEXITCODE -ne 0) { exit 1 }
exit 0
