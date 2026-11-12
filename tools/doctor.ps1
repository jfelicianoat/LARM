# tools/doctor.ps1 - Prerrequisitos de desarrollo de LARM en Windows (P008).
# Solo lee: no instala, no cambia PATH ni el registro. Dos fases:
#   recogida (salidas crudas, solo lectura)  ->  evaluacion (PASS/WARN/FAIL)
# Uso:
#   tools/doctor.ps1 [-OutJson ruta]          recoge en este equipo y evalua
#   tools/doctor.ps1 -FromRaw raw.json        evalua salidas crudas dadas (pruebas del parser)
#   tools/doctor.ps1 -RawOut raw.json         guarda ademas la recogida cruda
# Exit: 0 sin FAIL, 1 con algun FAIL.
param(
    [string]$OutJson = "",
    [string]$FromRaw = "",
    [string]$RawOut = ""
)

$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent $PSScriptRoot

function Invoke-Version([string]$exe, [string[]]$arguments) {
    $cmd = Get-Command $exe -ErrorAction SilentlyContinue
    if (-not $cmd) { return $null }
    try {
        $prev = $ErrorActionPreference
        $ErrorActionPreference = 'Continue'
        $out = (& $cmd.Source @arguments 2>&1 | Select-Object -First 1) -join ''
        $ErrorActionPreference = $prev
        return [ordered]@{ path = $cmd.Source; output = "$out".Trim() }
    } catch {
        return [ordered]@{ path = $cmd.Source; output = '' }
    }
}

function Get-RegValue([string]$key, [string]$name) {
    $item = Get-ItemProperty -Path $key -ErrorAction SilentlyContinue
    if ($null -eq $item) { return $null }
    return $item.$name
}

function Collect {
    $raw = [ordered]@{}
    $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
    $raw.vswhere = if (Test-Path $vswhere) {
        (& $vswhere -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -format json) -join "`n"
    } else { $null }
    $raw.webview2 = Get-RegValue 'HKLM:\SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}' 'pv'
    if (-not $raw.webview2) {
        $raw.webview2 = Get-RegValue 'HKCU:\Software\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}' 'pv'
    }
    $raw.windows_sdk = Get-RegValue 'HKLM:\SOFTWARE\Microsoft\Windows Kits\Installed Roots' 'KitsRoot10'
    $raw.tools = [ordered]@{
        rustc  = Invoke-Version 'rustc' @('--version')
        cargo  = Invoke-Version 'cargo' @('--version')
        node   = Invoke-Version 'node' @('--version')
        npm    = Invoke-Version 'npm.cmd' @('--version')
        python = Invoke-Version 'python' @('--version')
        git    = Invoke-Version 'git' @('--version')
        cmake  = Invoke-Version 'cmake' @('--version')
        ninja  = Invoke-Version 'ninja' @('--version')
        pwsh   = Invoke-Version 'pwsh' @('--version')
    }
    $raw.powershell = "$($PSVersionTable.PSVersion)"
    $raw.developer_mode = Get-RegValue 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\AppModelUnlock' 'AllowDevelopmentWithoutDevLicense'
    $raw.long_paths = Get-RegValue 'HKLM:\SYSTEM\CurrentControlSet\Control\FileSystem' 'LongPathsEnabled'
    # Symlink: se prueba en una carpeta temporal propia y se borra; no cambia el sistema.
    $probe = Join-Path ([IO.Path]::GetTempPath()) ("larm-doctor-" + [guid]::NewGuid())
    New-Item -ItemType Directory -Path $probe | Out-Null
    try {
        Set-Content -Path (Join-Path $probe 't.txt') -Value 'x'
        # Sin el privilegio, mklink escribe en stderr: es el dato que se recoge, no un error del script.
        $prev = $ErrorActionPreference
        $ErrorActionPreference = 'Continue'
        $null = cmd /c "mklink `"$probe\l.txt`" `"$probe\t.txt`"" 2>&1
        $ErrorActionPreference = $prev
        $raw.symlink = Test-Path (Join-Path $probe 'l.txt')
    } finally {
        Remove-Item -Recurse -Force $probe -ErrorAction SilentlyContinue
    }
    return $raw
}

function Result([string]$check, [string]$status, [string]$observed, [string]$fix = '') {
    [ordered]@{ check = $check; status = $status; observed = $observed; fix = $fix }
}

function Parse-Version([string]$text) {
    $m = [regex]::Match("$text", '(\d+)\.(\d+)(?:\.(\d+))?')
    if (-not $m.Success) { return $null }
    $patch = if ($m.Groups[3].Success) { [int]$m.Groups[3].Value } else { 0 }
    return [version]::new([int]$m.Groups[1].Value, [int]$m.Groups[2].Value, $patch)
}

function Evaluate($raw, $lock) {
    $r = [System.Collections.Generic.List[object]]::new()

    # MSVC (vswhere con el componente VC.Tools x86/x64)
    $instances = @()
    # En Windows PowerShell 5.1, ConvertFrom-Json de un array JSON devuelve el array como un
    # solo objeto: hay que enumerarlo antes de filtrar cada instancia.
    if ($raw.vswhere) { try { $instances = @((ConvertFrom-Json $raw.vswhere) | ForEach-Object { $_ }) } catch { $instances = @() } }
    $complete = @($instances | Where-Object { "$($_.isComplete)" -eq 'True' -and "$($_.isPrerelease)" -ne 'True' })
    if ($complete.Count -gt 0) {
        $names = ($complete | ForEach-Object { "$($_.displayName) $($_.installationVersion)" }) -join '; '
        $r.Add((Result 'msvc' 'PASS' $names))
    } else {
        $r.Add((Result 'msvc' 'FAIL' 'sin instalacion con Microsoft.VisualStudio.Component.VC.Tools.x86.x64' 'Instalar Visual Studio Build Tools con la carga "Desarrollo para el escritorio con C++" (MSVC x64/x86 y Windows SDK).'))
    }
    if ($raw.windows_sdk) { $r.Add((Result 'windows_sdk' 'PASS' "$($raw.windows_sdk)")) }
    else { $r.Add((Result 'windows_sdk' 'FAIL' 'KitsRoot10 ausente' 'Instalar el Windows 10/11 SDK desde Visual Studio Installer.')) }

    $wv = Parse-Version $raw.webview2
    if ($wv -and $wv.Major -gt 0) { $r.Add((Result 'webview2' 'PASS' "$($raw.webview2)")) }
    else { $r.Add((Result 'webview2' 'FAIL' 'runtime WebView2 no registrado' 'Instalar Microsoft Edge WebView2 Runtime (Evergreen) de https://developer.microsoft.com/microsoft-edge/webview2/.')) }

    # Toolchains fijados por environment.lock.json (P000)
    $rust = $raw.tools.rustc
    $wantRust = "$($lock.rust.rustc)"
    if (-not $rust) { $r.Add((Result 'rustc' 'FAIL' 'no encontrado' 'Instalar rustup (https://rustup.rs); rust-toolchain.toml fija la version.')) }
    elseif ($rust.output -match "rustc $([regex]::Escape($wantRust))\b") { $r.Add((Result 'rustc' 'PASS' $rust.output)) }
    else { $r.Add((Result 'rustc' 'FAIL' "$($rust.output) (lock: $wantRust)" "Ejecutar cargo en el repo: rustup instala $wantRust desde rust-toolchain.toml.")) }

    $node = $raw.tools.node
    $wantNode = "$($lock.node.node)"
    if (-not $node) { $r.Add((Result 'node' 'FAIL' 'no encontrado' "Instalar Node.js $wantNode LTS.")) }
    elseif ($node.output -eq $wantNode) { $r.Add((Result 'node' 'PASS' $node.output)) }
    else { $r.Add((Result 'node' 'WARN' "$($node.output) (lock: $wantNode)" "Usar Node.js $wantNode para reproducir los builds.")) }

    $py = Parse-Version ($raw.tools.python.output)
    if (-not $raw.tools.python) { $r.Add((Result 'python' 'FAIL' 'no encontrado' 'Instalar Python 3.11 o posterior (herramientas de verificacion).')) }
    elseif ($py -ge [version]'3.11.0') { $r.Add((Result 'python' 'PASS' $raw.tools.python.output)) }
    else { $r.Add((Result 'python' 'FAIL' $raw.tools.python.output 'Se necesita Python 3.11 o posterior.')) }

    if ($raw.tools.git) { $r.Add((Result 'git' 'PASS' $raw.tools.git.output)) }
    else { $r.Add((Result 'git' 'FAIL' 'no encontrado' 'Instalar Git for Windows.')) }

    $ps = Parse-Version $raw.powershell
    if ($ps -ge [version]'5.1.0') { $r.Add((Result 'powershell' 'PASS' "Windows PowerShell $($raw.powershell)$(if ($raw.tools.pwsh) { '; ' + $raw.tools.pwsh.output })")) }
    else { $r.Add((Result 'powershell' 'FAIL' "$($raw.powershell)" 'Se necesita Windows PowerShell 5.1.')) }

    foreach ($t in 'cmake', 'ninja') {
        if ($raw.tools.$t) { $r.Add((Result $t 'PASS' $raw.tools.$t.output)) }
        else { $r.Add((Result $t 'WARN' 'no encontrado' "Solo lo necesitan los runtimes con SOURCE_BUILD (p. ej. llama.cpp desde fuente). Instalar $t y anadirlo al PATH del usuario.")) }
    }

    if ("$($raw.developer_mode)" -eq '1') { $r.Add((Result 'developer_mode' 'PASS' 'activado')) }
    else { $r.Add((Result 'developer_mode' 'WARN' 'desactivado o sin valor' 'Opcional: Configuracion > Sistema > Para programadores > Modo de programador (permite symlinks sin privilegios).')) }
    if ($raw.symlink -eq $true) { $r.Add((Result 'symlink' 'PASS' 'se pueden crear symlinks')) }
    else { $r.Add((Result 'symlink' 'WARN' 'no se pueden crear symlinks con este usuario' 'Las vistas de runtime usaran hard links o copias; activar el Modo de programador para symlinks.')) }
    if ("$($raw.long_paths)" -eq '1') { $r.Add((Result 'long_paths' 'PASS' 'LongPathsEnabled=1')) }
    else { $r.Add((Result 'long_paths' 'WARN' 'LongPathsEnabled distinto de 1' 'Recomendado para modelos con rutas largas: directiva "Habilitar rutas largas de Win32".')) }
    return $r
}

$lock = Get-Content -Raw -Encoding UTF8 (Join-Path $repo 'environment.lock.json') | ConvertFrom-Json
if ($FromRaw) {
    $raw = Get-Content -Raw -Encoding UTF8 $FromRaw | ConvertFrom-Json
} else {
    $raw = Collect
    if ($RawOut) { [IO.File]::WriteAllText($RawOut, ($raw | ConvertTo-Json -Depth 6), [Text.UTF8Encoding]::new($false)) }
}
$results = @(Evaluate $raw $lock)
$fails = @($results | Where-Object { $_.status -eq 'FAIL' }).Count
$warns = @($results | Where-Object { $_.status -eq 'WARN' }).Count
$overall = if ($fails -gt 0) { 'FAIL' } elseif ($warns -gt 0) { 'WARN' } else { 'PASS' }
$report = [ordered]@{ status = $overall; host = $env:COMPUTERNAME; checks = $results }

foreach ($c in $results) {
    $color = switch ($c.status) { 'PASS' { 'Green' } 'WARN' { 'Yellow' } default { 'Red' } }
    Write-Host ("[{0}] {1}: {2}" -f $c.status, $c.check, $c.observed) -ForegroundColor $color
    if ($c.fix) { Write-Host ("       -> {0}" -f $c.fix) }
}
Write-Host "DOCTOR: $overall ($fails FAIL, $warns WARN)"
if ($OutJson) { [IO.File]::WriteAllText($OutJson, ($report | ConvertTo-Json -Depth 6), [Text.UTF8Encoding]::new($false)) }
if ($fails -gt 0) { exit 1 } else { exit 0 }
