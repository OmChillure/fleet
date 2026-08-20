# Agent Fleet worker bootstrap -- Windows PowerShell.
# Same A/B source choice as install-worker.sh.
#
#   irm http://127.0.0.1:8080/install-worker.ps1 | iex
#   $env:FLEET_URL = "http://127.0.0.1:8080"; irm "$env:FLEET_URL/install-worker.ps1" | iex

$ErrorActionPreference = "Stop"

function Get-FleetPlatform {
    # OS arch, not this process. x64 PowerShell on ARM64 Windows reports AMD64.
    $osArch = $null
    try {
        $osArch = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString()
    } catch { }
    switch -Regex ("$osArch") {
        "Arm64" { return "windows-arm64" }
        "X64" { return "windows-x86_64" }
        "X86" { throw "32-bit Windows is not supported" }
    }
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
Write-Host "platform: $platform"
Write-Host "source:   $origin"

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
