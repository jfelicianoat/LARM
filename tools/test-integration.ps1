# tools/test-integration.ps1 - Suites de integracion (P007).
# Ejecuta las pruebas de integracion de Rust (tests/*.rs de cada crate) y la verificacion de
# los fixtures comunes con procesos reales. No necesita hardware especial ni red.
# Uso: tools/test-integration.ps1
$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent $PSScriptRoot
Set-Location $repo
$python = Join-Path $repo '.venv-tools/Scripts/python.exe'
if (-not (Test-Path $python)) { $python = 'python' }

$failed = 0
function Step([string]$name, [scriptblock]$action) {
    Write-Host "==> $name" -ForegroundColor Cyan
    $prev = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    & $action
    $code = $LASTEXITCODE
    $ErrorActionPreference = $prev
    if ($code -ne 0) { $script:failed++; Write-Host "FAIL $name (exit $code)" -ForegroundColor Red }
}

# Los tests de proceso copian ejemplos desde target/debug/examples; reconstruirlos
# aquí evita reutilizar fixtures obsoletos o depender de una compilación previa.
Step 'cargo_examples' { cargo build --workspace --examples }

# Integracion Rust: solo los objetivos tests/*.rs, no las pruebas unitarias.
Step 'cargo_integration' { cargo test --workspace --test '*' }
foreach ($f in @('jsonrpc', 'http', 'sparse')) {
    Step "fixtures_$f" { & $python tools/verify_fixtures.py $f }
}

if ($failed -gt 0) { Write-Host "INTEGRACION: FAIL ($failed)" -ForegroundColor Red; exit 1 }
Write-Host 'INTEGRACION: PASS' -ForegroundColor Green
exit 0
