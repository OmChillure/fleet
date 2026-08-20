# Windows x86_64 (amd64) worker installer.
$ErrorActionPreference = "Stop"
$env:FLEET_PLATFORM = "windows-x86_64"
$Origin = $env:FLEET_INSTALL_ORIGIN
if (-not $Origin) { $Origin = $env:FLEET_URL }
if (-not $Origin) { throw "set FLEET_URL to the control plane" }
$Origin = $Origin.TrimEnd("/")

$Prefix = $env:FLEET_PREFIX
if (-not $Prefix) { $Prefix = Join-Path $env:LOCALAPPDATA "fleet-host" }
$BinDir = Join-Path $Prefix "bin"
New-Item -ItemType Directory -Force -Path $BinDir | Out-Null
$ThisPlat = "windows-x86_64"
$WantPe = "amd64"

function Get-FleetPlatform {
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
function Get-PeMachine([string]$Path) {
    $fs = [IO.File]::OpenRead($Path)
    try {
        $hdr = New-Object byte[] 64
        if ($fs.Read($hdr, 0, 64) -lt 64) { return "short" }
        if ($hdr[0] -ne 0x4D -or $hdr[1] -ne 0x5A) { return "not-mz" }
        $e = [BitConverter]::ToInt32($hdr, 0x3C)
        [void]$fs.Seek($e, [IO.SeekOrigin]::Begin)
        $pe = New-Object byte[] 6
        if ($fs.Read($pe, 0, 6) -lt 6) { return "short" }
        if ($pe[0] -ne 0x50 -or $pe[1] -ne 0x45) { return "not-pe" }
        $m = [BitConverter]::ToUInt16($pe, 4)
        if ($m -eq 0x8664) { return "amd64" }
        if ($m -eq 0xAA64) { return "arm64" }
        if ($m -eq 0x14C) { return "i386" }
        return ("0x{0:X4}" -f $m)
    } finally {
        $fs.Dispose()
    }
}

$detected = Get-FleetPlatform
if ($detected -ne $ThisPlat) {
    throw "this machine is $detected, not $ThisPlat. Re-run install-worker.ps1 so it picks the matching installer."
}

function Get-RemoteSha256([string]$Url) {
    try {
        $r = Invoke-WebRequest -UseBasicParsing -TimeoutSec 30 -Uri $Url
        $c = $r.Content
        if ($c -is [byte[]]) {
            $text = [Text.Encoding]::ASCII.GetString($c)
        } else {
            $text = [string]$c
        }
        return (($text.Trim() -split "\s+")[0]).ToLowerInvariant()
    } catch {
        return $null
    }
}
function Get-LocalSha256([string]$Path) {
    return (Get-FileHash -Algorithm SHA256 -LiteralPath $Path).Hash.ToLowerInvariant()
}
function Format-Bytes([object]$n) {
    if (-not $n) { return "" }
    $n = [int64]$n
    if ($n -ge 1048576) { return (" ({0:N0} MB)" -f ($n / 1MB)) }
    if ($n -ge 1024) { return (" ({0:N0} KB)" -f ($n / 1KB)) }
    return " ($n B)"
}
function Get-RemoteLength([string]$Url) {
    try {
        $r = Invoke-WebRequest -UseBasicParsing -Method Head -TimeoutSec 15 -Uri $Url
        $cl = $r.Headers["Content-Length"]
        if ($cl) { return [int64]$cl }
    } catch { }
    return $null
}
function Save-Url([string]$Url, [string]$Dest) {
    $curl = Get-Command curl.exe -ErrorAction SilentlyContinue
    if ($curl) {
        & curl.exe -fL --progress-bar -o $Dest $Url
        if ($LASTEXITCODE -ne 0) { throw "download failed $Url" }
        return
    }
    Invoke-WebRequest -UseBasicParsing -TimeoutSec 600 -Uri $Url -OutFile $Dest
}

Write-Host "Agent Fleet worker installer"
Write-Host "  platform: windows-x86_64"
Write-Host "  origin:   $Origin"
Write-Host "  prefix:   $Prefix"
Write-Host "this machine is a self-hosted worker"
Write-Host "  fleet-host   stays up; pairs and takes jobs"
Write-Host "  fleet-worker runs each job"
Write-Host "  fleet-driver runs jobs that use an agent harness (Grok)"

$base = "$Origin/install/worker/bin/windows-x86_64"
$hostReady = $false
foreach ($bin in @("fleet-host.exe", "fleet-worker.exe", "fleet-driver.exe")) {
    $dest = Join-Path $BinDir $bin
    $url = "$base/$bin"
    $tmp = "$dest.tmp"
    Write-Host "checking $bin"
    $want = Get-RemoteSha256 "$url.sha256"
    if ($want -and (Test-Path $dest) -and ((Get-LocalSha256 $dest) -eq $want)) {
        $pe = Get-PeMachine $dest
        if ($pe -ne $WantPe) {
            throw "$dest is $pe; expected $WantPe for $ThisPlat"
        }
        Write-Host "unchanged $dest"
        if ($bin -eq "fleet-host.exe") { $hostReady = $true }
        continue
    }
    if ($want -and (Test-Path $dest)) {
        Write-Host "local $bin is stale; downloading again"
    }
    $note = Format-Bytes (Get-RemoteLength $url)
    Write-Host "downloading $bin$note"
    try {
        Save-Url $url $tmp
        if ($want) {
            Write-Host "verifying $bin"
            $got = Get-LocalSha256 $tmp
            if ($got -ne $want) { throw "checksum mismatch $bin" }
        }
        $pe = Get-PeMachine $tmp
        if ($pe -ne $WantPe) {
            throw "$bin is $pe; expected $WantPe for $ThisPlat"
        }
        Move-Item -Force $tmp $dest
        Write-Host "installed $dest"
        if ($bin -eq "fleet-host.exe") { $hostReady = $true }
    } catch {
        if (Test-Path $tmp) { Remove-Item -Force $tmp }
        Write-Host "$bin : $($_.Exception.Message)"
    }
}
if (-not $hostReady) {
    throw @"
fleet-host.exe is missing or not valid for $ThisPlat.
If this machine is ARM64, re-run install-worker.ps1 (do not pin windows-x86_64).
Build with: cargo build --release -p fleet-host
and set FLEET_WORKER_DIST_DIR on the control plane.
"@
}

# User PATH (session + persistent user env).
$env:Path = "$BinDir;$env:Path"
$userPath = [Environment]::GetEnvironmentVariable("Path", "User")
if (-not $userPath) { $userPath = "" }
if ($userPath -notlike "*$BinDir*") {
    [Environment]::SetEnvironmentVariable("Path", "$BinDir;$userPath", "User")
    Write-Host "added $BinDir to user PATH"
}

$startup = Join-Path $env:APPDATA "Microsoft\Windows\Start Menu\Programs\Startup\fleet-host.cmd"
"@echo off`r`n`"$BinDir\fleet-host.exe`" run`r`n" | Set-Content -Path $startup -Encoding ASCII
Write-Host "startup shortcut: $startup"

if ($env:FLEET_PAIR_TOKEN) {
    Write-Host "pairing with the control plane"
    & (Join-Path $BinDir "fleet-host.exe") pair --url $Origin --token $env:FLEET_PAIR_TOKEN
    Write-Host "starting fleet-host (heartbeats; offline after 30s if this fails)"
    Start-Process -FilePath (Join-Path $BinDir "fleet-host.exe") -ArgumentList "run" -WindowStyle Hidden
    Write-Host "started fleet-host run (also at logon: $startup)"
} else {
    Write-Host "next: fleet-host pair --url $Origin --token <pair-token-from-ui>"
    Write-Host "then: Start-Process fleet-host.exe -ArgumentList run"
    Write-Host "local settings: %USERPROFILE%\.config\fleet-host\config.toml  (run_host / run_container)"
}
