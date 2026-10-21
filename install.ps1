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

function Invoke-Uninstall {
    $exe = Join-Path $InstallDir "sakuio-agent.exe"
    $service = Get-Service -Name $ServiceName -ErrorAction SilentlyContinue
    if ($service) {
        if ($service.Status -ne "Stopped") { Stop-Service -Name $ServiceName -Force }
        sc.exe delete $ServiceName | Out-Null
    }
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

# (Re)install the Windows service
$existing = Get-Service -Name $ServiceName -ErrorAction SilentlyContinue
if ($existing) {
    if ($existing.Status -ne "Stopped") { Stop-Service -Name $ServiceName -Force }
    sc.exe delete $ServiceName | Out-Null
    Start-Sleep -Seconds 1
}

$binPath = "`"$ExePath`" $($AgentArgs -join ' ')"
New-Service -Name $ServiceName -BinaryPathName $binPath -DisplayName "Sakuio Agent" -StartupType Automatic | Out-Null
Start-Service -Name $ServiceName

Write-Host "sakuio-agent v$Version installed as service '$ServiceName'."
