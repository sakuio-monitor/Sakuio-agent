# Sakuio Agent Windows installer.
#
# One-liner (PowerShell):
#   powershell.exe -NoProfile -ExecutionPolicy Bypass -Command ^
#     "iwr 'https://raw.githubusercontent.com/sakuio-monitor/Sakuio-agent/main/install.ps1' -UseBasicParsing -OutFile 'install.ps1'; & '.\install.ps1' -e <endpoint> -t <token>"
#
# Usage:
#   .\install.ps1 -e <endpoint> -t <token> [-i <interval>]
#   .\install.ps1 [-Version V] [-InstallDir DIR] [-ServiceName NAME]
#                 [-DisableRemoteControl] [-DisableAutoUpdate] [-Insecure]
#                 [-IncludeBufferMemory] [-IpFromNic] [-GpuDetail]
#                 [-IncludeNics "eth0,eth1"] [-ExcludeNics "docker0"]
#                 [-IncludeMountpoints "C:\,D:\"] [-MonthlyResetDay DAY]
#   .\install.ps1 -Uninstall [-ServiceName NAME] [-InstallDir DIR]
#
# 服务用 nssm 包装：agent 本身不实现 Windows 服务控制协议（SCM），
# 裸 New-Service 注册的 exe 无法通过 Start-Service 启动；nssm 自己对接 SCM 并守护子进程。

param(
    [Alias("e")]
    [string]$Endpoint,
    [Alias("t")]
    [string]$Token,
    [Alias("i")]
    [string]$Interval,
    [string]$Version,
    [string]$GithubProxy,
    [string]$InstallDir,
    [string]$ServiceName,
    [string]$IncludeNics,
    [string]$ExcludeNics,
    [string]$IncludeMountpoints,
    [string]$MonthlyResetDay,
    [switch]$DisableRemoteControl,
    [switch]$DisableAutoUpdate,
    [switch]$Insecure,
    [switch]$IncludeBufferMemory,
    [switch]$IpFromNic,
    [switch]$GpuDetail,
    [switch]$Uninstall
)

$ErrorActionPreference = "Stop"

$Repo = "sakuio-monitor/Sakuio-agent"
$DefaultDir = "$env:ProgramFiles\sakuio-agent"
$DefaultService = "sakuio-agent"

if (-not $InstallDir) { $InstallDir = $DefaultDir }
if (-not $ServiceName) { $ServiceName = $DefaultService }

$NssmExe = Join-Path $InstallDir "nssm.exe"

function Invoke-Uninstall {
    if (Test-Path $NssmExe) { & $NssmExe stop $ServiceName 2>$null | Out-Null }
    & $NssmExe remove $ServiceName confirm 2>$null | Out-Null
    $svc = Get-Service -Name $ServiceName -ErrorAction SilentlyContinue
    if ($svc) {
        if ($svc.Status -ne "Stopped") { Stop-Service -Name $ServiceName -Force -ErrorAction SilentlyContinue }
        sc.exe delete $ServiceName | Out-Null
    }
    $exe = Join-Path $InstallDir "sakuio-agent.exe"
    if (Test-Path $exe) { Remove-Item -Force $exe }
    if ((Test-Path $InstallDir) -and -not (Get-ChildItem $InstallDir)) {
        Remove-Item -Force $InstallDir
    }
    Write-Host "sakuio-agent uninstalled."
    exit 0
}

if ($Uninstall) { Invoke-Uninstall }

if (-not $Endpoint) {
    Write-Error "missing required argument: -e <endpoint>"
}
if (-not $Token) {
    Write-Error "missing required argument: -t <token>"
}

# Resolve version (latest release tag if not specified)
if (-not $Version) {
    $latest = Invoke-RestMethod -Uri "https://api.github.com/repos/$Repo/releases/latest" -UseBasicParsing
    $Version = $latest.tag_name.TrimStart("v")
}
if (-not $Version) {
    Write-Error "failed to resolve latest version; pass -Version"
}

# Detect architecture
switch ($env:PROCESSOR_ARCHITECTURE) {
    "AMD64" { $Arch = "amd64" }
    "ARM64" { $Arch = "arm64" }
    "x86"   { $Arch = "386" }
    default { Write-Error "unsupported architecture: $env:PROCESSOR_ARCHITECTURE" }
}

$Asset = "sakuio-agent-windows-$Arch.exe"
$Base = if ($GithubProxy) { $GithubProxy.TrimEnd("/") } else { "https://github.com" }
$DownloadUrl = "$Base/$Repo/releases/download/v$Version/$Asset"
$ExePath = Join-Path $InstallDir "sakuio-agent.exe"

# Build agent arguments
$AgentArgs = @("-e", $Endpoint, "-t", $Token)
if ($Interval) { $AgentArgs += @("-i", $Interval) }
if ($DisableRemoteControl) { $AgentArgs += "--disable-remote-control" }
if ($DisableAutoUpdate) { $AgentArgs += "--disable-auto-update" }
if ($Insecure) { $AgentArgs += "--insecure" }
if ($IncludeBufferMemory) { $AgentArgs += "--include-buffer-memory" }
if ($IpFromNic) { $AgentArgs += "--ip-from-nic" }
if ($GpuDetail) { $AgentArgs += "--gpu-detail" }
if ($IncludeNics) { $AgentArgs += @("--include-nics", $IncludeNics) }
if ($ExcludeNics) { $AgentArgs += @("--exclude-nics", $ExcludeNics) }
if ($IncludeMountpoints) { $AgentArgs += @("--include-mountpoints", $IncludeMountpoints) }
if ($MonthlyResetDay) { $AgentArgs += @("--monthly-reset-day", $MonthlyResetDay) }

Write-Host "Downloading $Asset (v$Version) ..."
New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
Invoke-WebRequest -Uri $DownloadUrl -UseBasicParsing -OutFile $ExePath

# --- nssm setup (wraps the agent for the Windows Service Control Manager) ---
New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
$env:Path = "$InstallDir;$env:Path"
$hasNssm = $false
if (Test-Path $NssmExe) {
    try { & $NssmExe version | Out-Null; $hasNssm = $true } catch { $hasNssm = $false }
}
if (-not $hasNssm) {
    $NssmVersion = "2.24"
    $ZipUrl = "https://nssm.cc/release/nssm-$NssmVersion.zip"
    $TempZip = Join-Path $env:TEMP "nssm-$NssmVersion.zip"
    $TempDir = Join-Path $env:TEMP "sakuio_nssm_extract"
    try {
        Write-Host "Downloading nssm from $ZipUrl ..."
        Invoke-WebRequest -Uri $ZipUrl -OutFile $TempZip -UseBasicParsing
        if (Test-Path $TempDir) { Remove-Item -Recurse -Force $TempDir }
        Expand-Archive -Path $TempZip -DestinationPath $TempDir -Force
        # win32 build of nssm works for amd64/arm64 PowerShell
        $src = Join-Path (Join-Path $TempDir "nssm-$NssmVersion") "win32\nssm.exe"
        if (-not (Test-Path $src)) {
            $found = Get-ChildItem -Path $TempDir -Recurse -Filter "nssm.exe" | Select-Object -First 1
            if ($found) { $src = $found.FullName }
        }
        if (-not (Test-Path $src)) { throw "nssm.exe not found in archive" }
        Copy-Item -Path $src -Destination $NssmExe -Force
    }
    finally {
        if (Test-Path $TempZip) { Remove-Item -Force $TempZip -ErrorAction SilentlyContinue }
        if (Test-Path $TempDir) { Remove-Item -Recurse -Force $TempDir -ErrorAction SilentlyContinue }
    }
}

# Remove any previous installation of the service
$svc = Get-Service -Name $ServiceName -ErrorAction SilentlyContinue
if ($svc) {
    if ($svc.Status -ne "Stopped") { Stop-Service -Name $ServiceName -Force -ErrorAction SilentlyContinue }
    sc.exe delete $ServiceName | Out-Null
    Start-Sleep -Seconds 1
}

# Register and start the service via nssm
$argString = $AgentArgs -join " "
& $NssmExe install $ServiceName $ExePath $argString | Out-Null
& $NssmExe set $ServiceName DisplayName "Sakuio Agent" | Out-Null
& $NssmExe set $ServiceName Start SERVICE_AUTO_START | Out-Null
& $NssmExe set $ServiceName AppExit Default Restart | Out-Null
& $NssmExe set $ServiceName AppRestartDelay 5000 | Out-Null
& $NssmExe start $ServiceName

Write-Host "sakuio-agent v$Version installed as nssm service '$ServiceName'."
