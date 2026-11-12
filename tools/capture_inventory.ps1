# P000 - Captura real read-only del PC (sin secretos). Solo lectura; no modifica nada.
param([Parameter(Mandatory = $true)][string]$OutFile)
$ErrorActionPreference = 'SilentlyContinue'
$out = [ordered]@{}

# --- OS ---
$os = Get-CimInstance Win32_OperatingSystem
$cs = Get-CimInstance Win32_ComputerSystem
$out.os = [ordered]@{
  caption        = $os.Caption
  version        = $os.Version
  build          = $os.BuildNumber
  ubr            = (Get-ItemProperty 'HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion').UBR
  displayVersion = (Get-ItemProperty 'HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion').DisplayVersion
  architecture   = $os.OSArchitecture
  locale         = (Get-Culture).Name
  timezone       = (Get-TimeZone).Id
  hostname       = $env:COMPUTERNAME
  domainJoined   = $cs.PartOfDomain
}

# --- CPU ---
$cpu = Get-CimInstance Win32_Processor | Select-Object -First 1
$out.cpu = [ordered]@{
  name          = $cpu.Name
  manufacturer  = $cpu.Manufacturer
  cores         = $cpu.NumberOfCores
  logicalCores  = $cpu.NumberOfLogicalProcessors
  maxClockMhz   = $cpu.MaxClockSpeed
  description   = $cpu.Description
}

# --- RAM ---
$rams = Get-CimInstance Win32_PhysicalMemory
$out.ram = [ordered]@{
  totalBytes  = $cs.TotalPhysicalMemory
  modules     = @($rams | ForEach-Object { [ordered]@{ capacityBytes = $_.Capacity; speedMhz = $_.Speed; manufacturer = $_.Manufacturer; partNumber = ($_.PartNumber -replace '\s+','') } })
}

# --- GPU ---
# Win32_VideoController.AdapterRAM es uint32 y satura en ~4 GiB: no es la VRAM real.
# El valor de 64 bits lo publica el driver en HardwareInformation.qwMemorySize.
# Enlace exacto dispositivo -> driver: HKLM\...\Enum\<PNPDeviceID>\Driver = '{clase}\NNNN'.
$out.gpus = @(Get-CimInstance Win32_VideoController | ForEach-Object {
  $vc = $_
  $key = $null
  if ($vc.PNPDeviceID) {
    $driverRef = (Get-ItemProperty ('HKLM:\SYSTEM\CurrentControlSet\Enum\' + $vc.PNPDeviceID) -ErrorAction SilentlyContinue).Driver
    if ($driverRef) {
      $key = Get-ItemProperty ('HKLM:\SYSTEM\CurrentControlSet\Control\Class\' + $driverRef) -ErrorAction SilentlyContinue
      if (-not $key.'HardwareInformation.qwMemorySize') { $key = $null }
    }
  }
  if ($key) {
    $vram = [string][uint64]$key.'HardwareInformation.qwMemorySize'; $vramSource = 'registry:HardwareInformation.qwMemorySize'
  } else {
    $vram = 'NO_DISPONIBLE'; $vramSource = 'NO_DISPONIBLE: el driver no publica qwMemorySize; AdapterRAM (uint32) no es fiable'
  }
  [ordered]@{
    name             = $vc.Name
    adapterCompat    = $vc.AdapterCompatibility
    pnpDeviceId      = $vc.PNPDeviceID
    driverVersion    = $vc.DriverVersion
    driverDate       = if ($vc.DriverDate) { $vc.DriverDate.ToString('yyyy-MM-dd') } else { 'NO_DISPONIBLE' }
    vramBytes        = $vram
    vramSource       = $vramSource
    adapterRamWmiUint32 = if ($vc.AdapterRAM) { [string]$vc.AdapterRAM } else { 'NO_DISPONIBLE' }
    videoProcessor   = $vc.VideoProcessor
    status           = $vc.Status
  }
})

# --- NPU / aceleradores (PnP; no asumir Ryzen/Radeon) ---
$npuHits = @(Get-PnpDevice -PresentOnly | Where-Object { ($_.Class -eq 'ComputeAccelerator') -or ($_.FriendlyName -match 'NPU|Neural|AI Boost|VPU|Movidius') })
$out.npu = if ($npuHits.Count -gt 0) { @($npuHits | ForEach-Object { [ordered]@{ class = $_.Class; name = $_.FriendlyName; instanceId = $_.InstanceId; status = $_.Status; problem = $_.Problem } }) } else { 'NO_DISPONIBLE' }

# --- Drivers graficos ---
$out.graphicsDrivers = @(Get-CimInstance Win32_PnPSignedDriver | Where-Object { $_.DeviceClass -eq 'DISPLAY' } | ForEach-Object {
  [ordered]@{ deviceName = $_.DeviceName; driverVersion = $_.DriverVersion; driverDate = if ($_.DriverDate) { $_.DriverDate.ToString('yyyy-MM-dd') } else { 'NO_DISPONIBLE' }; infName = $_.InfName }
})

# --- Volumenes: Windows Volume GUID (spec 28), no el GUID de particion ---
$out.volumes = @(Get-Volume | Where-Object { $_.UniqueId -and $_.UniqueId.StartsWith('\\?\Volume{') } | Sort-Object UniqueId | ForEach-Object {
  $v = $_
  $part = Get-Partition | Where-Object { $_.AccessPaths -contains $v.UniqueId } | Select-Object -First 1
  [ordered]@{
    volumeGuidPath = $v.UniqueId
    volumeGuid     = ([regex]::Match($v.UniqueId, '\{([0-9a-fA-F-]{36})\}')).Groups[1].Value.ToLowerInvariant()
    driveLetter    = if ($v.DriveLetter) { [string]$v.DriveLetter } else { 'NO_DISPONIBLE' }
    gptPartitionGuid = if ($part -and $part.Guid) { $part.Guid.Trim('{}').ToLowerInvariant() } else { 'NO_DISPONIBLE' }
    diskNumber     = if ($part) { $part.DiskNumber } else { 'NO_DISPONIBLE' }
    driveType      = [string]$v.DriveType
    sizeBytes      = [string]$v.Size
    label          = if ($v.FileSystemLabel) { $v.FileSystemLabel } else { 'NO_DISPONIBLE' }
    fileSystem     = if ($v.FileSystem) { $v.FileSystem } else { 'NO_DISPONIBLE' }
    healthStatus   = [string]$v.HealthStatus
  }
})

# --- Procesos host de IA detectados (read-only) ---
$aiPatterns = 'ollama|llama|lmstudio|lemonade|comfy|kobold|text-generation|vllm|jan|gpt4all|openvino|onnx'
$out.aiHostProcesses = @(Get-Process | Where-Object { $_.ProcessName -match $aiPatterns } | ForEach-Object {
  [ordered]@{ name = $_.ProcessName; pid = $_.Id; path = if ($_.Path) { $_.Path } else { 'NO_DISPONIBLE' } }
})

# --- Versiones detectadas en PATH (sin instalar nada) ---
$toolChecks = [ordered]@{}
foreach ($t in @('ollama','llama-server','llama-cli','python','node','npm','git','docker')) {
  $cmd = Get-Command $t -ErrorAction SilentlyContinue
  if ($cmd) {
    $ver = 'DETECTADO_SIN_VERSION'
    try {
      $outp = & $t --version 2>$null | Select-Object -First 1
      if ($outp) { $ver = ($outp -join ' ').Trim() }
    } catch { $ver = 'DETECTADO_SIN_VERSION' }
    $toolChecks[$t] = [ordered]@{ path = $cmd.Source; version = $ver }
  } else {
    $toolChecks[$t] = 'NO_DISPONIBLE'
  }
}
$out.detectedTools = $toolChecks

# --- Accesibilidad HTTPS real (HEAD, timeout corto, sin credenciales) ---
$endpoints = @('https://huggingface.co','https://github.com','https://modelscope.cn','https://www.kaggle.com','https://registry.npmjs.org','https://static.crates.io')
$out.httpsReachability = @()
foreach ($e in $endpoints) {
  $sw = [Diagnostics.Stopwatch]::StartNew()
  try {
    $resp = Invoke-WebRequest -Uri $e -Method Head -TimeoutSec 8 -UseBasicParsing -MaximumRedirection 3
    $sw.Stop()
    $out.httpsReachability += [ordered]@{ endpoint = $e; status = [int]$resp.StatusCode; ok = $true; ms = $sw.ElapsedMilliseconds }
  } catch {
    $sw.Stop()
    $code = $null
    if ($_.Exception.Response) { $code = [int]$_.Exception.Response.StatusCode }
    $out.httpsReachability += [ordered]@{ endpoint = $e; status = $code; ok = $false; ms = $sw.ElapsedMilliseconds; error = $_.Exception.Message.Split("`n")[0] }
  }
}

# --- Broker capabilities (requiere credencial autorizada; sin ella NO_DISPONIBLE) ---
# GET /api/v1/capabilities es publico (Client API 2.11 sec. 127); auth/check exige credencial.
try {
  $cap = Invoke-WebRequest -Uri 'http://127.0.0.1:8765/api/v1/capabilities' -TimeoutSec 8 -UseBasicParsing
  $capJson = $cap.Content | ConvertFrom-Json
  $sha = [BitConverter]::ToString([Security.Cryptography.SHA256]::Create().ComputeHash([Text.Encoding]::UTF8.GetBytes($cap.Content))).Replace('-','').ToLowerInvariant()
  $out.brokerCapabilities = [ordered]@{ endpoint = 'http://127.0.0.1:8765/api/v1/capabilities'; status = [int]$cap.StatusCode; contractVersion = $capJson.contract_version; bodySha256 = $sha }
} catch {
  $out.brokerCapabilities = 'NO_DISPONIBLE_BROKER_INALCANZABLE'
}
$out.brokerAuthCheck = 'NO_DISPONIBLE_SIN_CREDENCIAL_AUTORIZADA'
$repoRoot = Split-Path -Parent $PSScriptRoot
if (Test-Path -LiteralPath (Join-Path $repoRoot '.local/integrations.json')) {
  $authPython = Join-Path $repoRoot '.venv-tools/Scripts/python.exe'
  if (-not (Test-Path -LiteralPath $authPython)) { $authPython = 'python' }
  $authOutput = & $authPython (Join-Path $PSScriptRoot 'check_integration_credentials.py') --broker-only 2>&1
  if ($LASTEXITCODE -eq 0) {
    $out.brokerAuthCheck = 'VERIFIED_HTTP_200_AUTHENTICATED_AUTH_REQUIRED'
  } else {
    $out.brokerAuthCheck = 'NO_DISPONIBLE_CREDENCIAL_O_BROKER'
  }
}


# UTF-8 sin BOM: la consola usa la pagina OEM y corrompe los no-ASCII
[IO.File]::WriteAllText($OutFile, ($out | ConvertTo-Json -Depth 6), [Text.UTF8Encoding]::new($false))
