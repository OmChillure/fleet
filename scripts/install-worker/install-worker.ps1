# Agent Fleet worker bootstrap -- Windows PowerShell.
# Same A/B source choice as install-worker.sh.
#
#   irm http://127.0.0.1:8080/install-worker.ps1 | iex
#   $env:FLEET_URL = "http://127.0.0.1:8080"; irm "$env:FLEET_URL/install-worker.ps1" | iex

$ErrorActionPreference = "Stop"

function Get-FleetPlatform {
    # OS arch, not this process. x64 PowerShell on ARM64 Windows reports AMD64
    # and .NET Framework OSArchitecture often lies as X64 under emulation.
    $machine = [Environment]::GetEnvironmentVariable("PROCESSOR_ARCHITECTURE", "Machine")
    if (-not $machine) {
        try {
            $machine = (Get-ItemProperty -LiteralPath "HKLM:\SYSTEM\CurrentControlSet\Control\Session Manager\Environment" -ErrorAction Stop).PROCESSOR_ARCHITECTURE
        } catch { }
    }
    switch -Regex ("$machine") {
        "ARM64" { return "windows-arm64" }
        "AMD64" { return "windows-x86_64" }
        "x86" { throw "32-bit Windows is not supported" }
    }
    try {
        $n = [int]((Get-CimInstance -ClassName Win32_Processor -ErrorAction Stop | Select-Object -First 1).Architecture)
        if ($n -eq 12) { return "windows-arm64" }
        if ($n -eq 9) { return "windows-x86_64" }
    } catch { }
    try {
        $osArch = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString()
        switch -Regex ($osArch) {
            "Arm64" { return "windows-arm64" }
            "X64" { return "windows-x86_64" }
        }
    } catch { }
    $arch = $env:PROCESSOR_ARCHITECTURE
    if ($env:PROCESSOR_ARCHITEW6432) { $arch = $env:PROCESSOR_ARCHITEW6432 }
    switch -Regex ($arch) {
        "ARM64" { return "windows-arm64" }
        "AMD64" { return "windows-x86_64" }
        default { throw "unsupported PROCESSOR_ARCHITECTURE=$arch" }
    }
}

function Normalize-Url([string]$u) {
    if ($u -notmatch "^https?://") { $u = "https://$u" }
    return $u.TrimEnd("/")
}

# 127/8, localhost, or [::1] -- another computer cannot reach this URL.
function Test-FleetUrlLoopback([string]$Url) {
    if (-not $Url) { return $false }
    $hostPart = $Url -replace '^https?://', ''
    $hostPart = $hostPart.Split('/')[0]
    # last @ (userinfo), same as host_from_url
    $at = $hostPart.LastIndexOf('@')
    if ($at -ge 0) { $hostPart = $hostPart.Substring($at + 1) }
    if ($hostPart -match '^\[(.+)\](?::\d+)?$') {
        $hostPart = $Matches[1]
    } elseif ($hostPart -notmatch ':.*:') {
        $hostPart = $hostPart.Split(':')[0]
    }
    $h = $hostPart.Trim().TrimStart('[').TrimEnd(']').ToLowerInvariant()
    if ($h -eq 'localhost' -or $h -eq '::1') { return $true }
    if ($h -match '^127\.(\d{1,3})\.(\d{1,3})\.(\d{1,3})$') {
        foreach ($p in @($Matches[1], $Matches[2], $Matches[3])) {
            $n = [int]$p
            if ($n -gt 255) { return $false }
        }
        return $true
    }
    return $false
}

# Warn on loopback. Off-box (GET /health fails) stops before pair/bin fetch.
# Same-box /health may continue (local fleet-dev).
function Assert-FleetLoopbackOrigin([string]$Origin) {
    if (-not (Test-FleetUrlLoopback $Origin)) { return }
    Write-Warning "FLEET_URL $Origin is loopback. Another computer needs a LAN or public control-plane URL."
    try {
        $null = Invoke-WebRequest -UseBasicParsing -TimeoutSec 5 -Uri "$Origin/health"
    } catch {
        throw "refusing to pair: $Origin is loopback and this host is not the control plane"
    }
}

if ($env:FLEET_DETECT_ONLY -eq "1") {
    Write-Output (Get-FleetPlatform)
    return
}

$official = $env:FLEET_OFFICIAL_BASE
$origin = $env:FLEET_URL
if (-not $origin) {
    Write-Host "Install this worker from:"
    if ($official) {
        Write-Host "  1) Official  ($official)"
    } else {
        Write-Host "  1) Official  (not configured -- you will be asked for a URL)"
    }
    Write-Host "  2) Control plane (type hostname or URL)"
    $choice = Read-Host "Choice [1/2]"
    switch ($choice) {
        "2" {
            $u = Read-Host "Control plane URL or domain"
            if (-not $u) { throw "empty url" }
            $origin = Normalize-Url $u
        }
        default {
            if ($official) {
                $origin = Normalize-Url $official
            } else {
                $u = Read-Host "Official / download URL"
                if (-not $u) { throw "empty url" }
                $origin = Normalize-Url $u
            }
        }
    }
} else {
    $origin = Normalize-Url $origin
}

$platform = Get-FleetPlatform
$env:FLEET_URL = $origin
$env:FLEET_INSTALL_ORIGIN = $origin
$env:FLEET_PLATFORM = $platform
$machineArch = [Environment]::GetEnvironmentVariable("PROCESSOR_ARCHITECTURE", "Machine")
Write-Host "platform: $platform"
Write-Host "  machine arch: $machineArch"
Write-Host "  process arch: $($env:PROCESSOR_ARCHITECTURE)"
Write-Host "source:   $origin"
Assert-FleetLoopbackOrigin $origin

$scriptUrl = "$origin/install/worker/${platform}.ps1"
Write-Host "downloading $platform installer"
& ([scriptblock]::Create((Invoke-WebRequest -UseBasicParsing $scriptUrl).Content))

# Optional gnullvm runtime. 404 = skip so x86_64 gnu is unchanged.
if ($BinDir -and -not (Test-Path (Join-Path $BinDir "libunwind.dll"))) {
    $dllDest = Join-Path $BinDir "libunwind.dll"
    $dllUrl = "$origin/install/worker/bin/$platform/libunwind.dll"
    Write-Host "checking libunwind.dll"
    try {
        $curl = Get-Command curl.exe -ErrorAction SilentlyContinue
        if ($curl) {
            Write-Host "downloading libunwind.dll"
            & curl.exe -fL --progress-bar -o $dllDest $dllUrl
            if ($LASTEXITCODE -ne 0) { throw "download failed" }
        } else {
            Write-Host "downloading libunwind.dll"
            Invoke-WebRequest -UseBasicParsing -TimeoutSec 120 -Uri $dllUrl -OutFile $dllDest
        }
        Write-Host "installed $dllDest"
    } catch {
        if (Test-Path $dllDest) { Remove-Item -Force $dllDest }
        Write-Host "no runtime libunwind.dll at $dllUrl"
    }
}
