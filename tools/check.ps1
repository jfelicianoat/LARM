# tools/check.ps1 - Quality gates de LARM sin tocar configuracion global.
# Uso: tools/check.ps1 [-Prompt P001] [-Real] [-SkipFrontend] [-SkipDesktop] [-Coverage]
#      tools/check.ps1 -Real -Scenario G01
#   Sin -Real: fmt, clippy -D warnings y test del workspace y del shell Tauri; lint, typecheck,
#              test y build del frontend; suites unittest de tools/ y adapters/*/*/tests.
#   Con -Real: ademas ejecuta tools/<prompt>_evidence.py, que corre las pruebas del prompt
#              (incluidas las reales) y escribe docs/reports/<prompt>_*.evidence.json.
#   -Scenario: ejecuta tools/scenarios/<gxx>.py (escenario global de acceptance.json). Si aun no
#              existe, el escenario queda BLOCKED (exit 2): nunca PASS por omision.
#   -Coverage: cobertura con cargo-llvm-cov fijado en tools/.bin (BLOCKED_TOOLING si falta).
param(
    [string]$Prompt = "",
    [switch]$Real,
    [switch]$SkipFrontend,
    [switch]$SkipDesktop,
    [string]$Scenario = "",
    [switch]$Coverage
)

$ErrorActionPreference = 'Stop'
$OutputEncoding = [Text.UTF8Encoding]::new($false)
[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
$repo = Split-Path -Parent $PSScriptRoot
Set-Location $repo
$testTemp = Join-Path $repo 'target/larm-test-tmp'
New-Item -ItemType Directory -Path $testTemp -Force | Out-Null
$env:TEMP = $testTemp
$env:TMP = $testTemp
$env:TMPDIR = $testTemp
$desktopManifest = 'apps/desktop/src-tauri/Cargo.toml'
$python = Join-Path $repo '.venv-tools/Scripts/python.exe'
if (-not (Test-Path $python)) { $python = 'python' }

$results = [System.Collections.Generic.List[object]]::new()
$transcripts = [ordered]@{}
$logRoot = Join-Path $repo ("docs/reports/check_runs/" + [DateTime]::UtcNow.ToString('yyyyMMddTHHmmssfffffffZ'))
New-Item -ItemType Directory -Path $logRoot -Force | Out-Null
function Add-Result([string]$name, [bool]$ok, [string]$detail = '') {
    $script:results.Add([ordered]@{ check = $name; result = $(if ($ok) { 'PASS' } else { 'FAIL' }); detail = $detail })
}

function Invoke-Step([string]$name, [scriptblock]$action) {
    Write-Host "==> $name" -ForegroundColor Cyan
    $prevEap = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'  # cargo/npm escriben progreso en stderr; no son errores
    try {
        $out = & $action 2>&1
        $code = $LASTEXITCODE
        $logPath = Join-Path $logRoot "$name.log"
        [IO.File]::WriteAllText($logPath, (($out | ForEach-Object { $_.ToString() }) -join "`n"), [Text.UTF8Encoding]::new($false))
        $hashAlgorithm = [Security.Cryptography.SHA256]::Create()
        $logStream = [IO.File]::OpenRead($logPath)
        try {
            $digest = [BitConverter]::ToString($hashAlgorithm.ComputeHash($logStream)).Replace('-', '').ToLowerInvariant()
        } finally {
            $logStream.Dispose()
            $hashAlgorithm.Dispose()
        }
        $script:transcripts[$name] = [ordered]@{
            path = $logPath.Substring($repo.Length + 1).Replace('\', '/')
            sha256 = $digest
            exit_code = $code
        }
        $tail = ($out | Select-Object -Last 5) -join ' | '
        Add-Result $name ($code -eq 0) "exit=$code $tail"
        if ($code -ne 0) { Write-Host ($out | Out-String) -ForegroundColor Yellow }
    } catch {
        Add-Result $name $false $_.Exception.Message
    } finally {
        $ErrorActionPreference = $prevEap
    }
}

Write-Host "LARM checks$(if ($Prompt) { " - $Prompt" })$(if ($Scenario) { " - $Scenario" })$(if ($Real) { ' [REAL]' })"

if ($Scenario) {
    if ($Scenario -notmatch '^G\d{2}$') { throw "escenario no valido: $Scenario" }
    $runner = "tools/scenarios/$($Scenario.ToLowerInvariant()).py"
    if (-not (Test-Path $runner)) {
        Write-Host "BLOCKED: el escenario $Scenario no tiene runner ($runner); sus prompts propietarios aun no estan hechos" -ForegroundColor Yellow
        exit 2
    }
    & $python $runner
    exit $LASTEXITCODE
}

# Rust: workspace
Invoke-Step 'cargo_fmt' { cargo fmt --all --check }
Invoke-Step 'cargo_clippy' { cargo clippy --workspace --all-targets -- -D warnings }
Invoke-Step 'test_fixtures' { cargo build -p larm-adapter-sdk -p larm-adapter-host --bins --examples }
Invoke-Step 'cargo_test' {
    $previousTarget = $env:CARGO_TARGET_DIR
    try {
        # El sandbox del host mantiene bloqueados algunos .exe de target/debug.
        # El target de verificación está bajo el repo y evita ese artefacto stale.
        $env:CARGO_TARGET_DIR = Join-Path $repo 'target/verification-final'
        # Las pruebas de integración de workers comparten fixtures del sistema y son
        # sensibles al paralelismo del harness; serializarlas evita carreras espurias.
        cargo test --workspace -j 1 -- --test-threads=1
    } finally {
        if ($null -eq $previousTarget) { Remove-Item Env:CARGO_TARGET_DIR -ErrorAction SilentlyContinue }
        else { $env:CARGO_TARGET_DIR = $previousTarget }
    }
}
Invoke-Step 'architecture' { powershell -NoProfile -ExecutionPolicy Bypass -File tools/check_architecture.ps1 }
Invoke-Step 'package_copy' { & $python tools/package_copy.py check }
Invoke-Step 'python_tests' { & $python tools/run_python_tests.py }

# Rust: shell Tauri (excluido del workspace)
if (-not $SkipDesktop) {
    Invoke-Step 'desktop_fmt' { cargo fmt --manifest-path $desktopManifest --check }
    Invoke-Step 'desktop_clippy' { cargo clippy --manifest-path $desktopManifest --all-targets -- -D warnings }
    Invoke-Step 'desktop_test' { cargo test --manifest-path $desktopManifest }
}

# Frontend
if (-not $SkipFrontend) {
    Invoke-Step 'npm_lint' { npm run lint --silent }
    Invoke-Step 'npm_typecheck' { npm run typecheck --silent }
    Invoke-Step 'npm_test' { npm test --silent }
    Invoke-Step 'npm_build' { npm run build --silent }
}

# Cobertura (P007): herramienta fijada dentro del repo; si falta, BLOCKED_TOOLING
if ($Coverage) {
    $cov = Join-Path $repo 'tools/.bin/bin/cargo-llvm-cov.exe'
    if (Test-Path $cov) {
        Invoke-Step 'coverage' { & $cov llvm-cov --workspace --summary-only --json --output-path docs/reports/coverage.json }
    } else {
        Add-Result 'coverage' $false 'BLOCKED_TOOLING: falta tools/.bin/bin/cargo-llvm-cov.exe; instalar con: cargo install cargo-llvm-cov --version =0.9.1 --locked --root tools/.bin'
    }
}

# Pruebas del prompt (incluidas las reales) y evidencia
if ($Real) {
    if (-not $Prompt) { throw '-Real exige -Prompt PXXX' }
    $runner = "tools/$($Prompt.ToLowerInvariant())_evidence.py"
    if (Test-Path $runner) {
        Invoke-Step "$($Prompt)_evidence" { & $python $runner }
    } else {
        Add-Result "$($Prompt)_evidence" $false "no existe ${runner}; las pruebas reales de $Prompt no estan implementadas"
    }
}

$failed = @($results | Where-Object { $_.result -eq 'FAIL' })
$status = if ($failed.Count -eq 0) { 'PASS' } else { 'FAIL' }
$report = [ordered]@{
    prompt = $Prompt
    real   = [bool]$Real
    status = $status
    checks = $results
    transcripts = $transcripts
}
$reportPath = Join-Path $repo "docs/reports/check$(if ($Prompt) { "_$Prompt" })$(if ($Real) { '_real' }).json"
New-Item -ItemType Directory -Force -Path (Split-Path -Parent $reportPath) | Out-Null
[IO.File]::WriteAllText($reportPath, ($report | ConvertTo-Json -Depth 5), [Text.UTF8Encoding]::new($false))
Write-Host "`nSTATUS: $status ($($results.Count) checks, $($failed.Count) fallos) -> $reportPath" -ForegroundColor $(if ($status -eq 'PASS') { 'Green' } else { 'Red' })
if ($status -eq 'FAIL') { exit 1 } else { exit 0 }
