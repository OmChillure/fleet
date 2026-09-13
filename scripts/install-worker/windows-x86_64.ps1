# Windows x86_64 (amd64) worker installer.
$ErrorActionPreference = "Stop"
$env:FLEET_PLATFORM = "windows-x86_64"
$Origin = $env:FLEET_INSTALL_ORIGIN
if (-not $Origin) { $Origin = $env:FLEET_URL }
if (-not $Origin) { throw "set FLEET_URL to the control plane" }
$Origin = $Origin.TrimEnd("/")

# Pair token is required for first install. Missing/empty/whitespace must
# fail closed before mkdir or fetch so we never write bins that never pair.
# A leftover empty or whitespace-only machine.token is unpaired (same as
# MachineToken::parse). A non-empty file may re-run without a new pair token.
$pairToken = "$env:FLEET_PAIR_TOKEN".Trim()
$cfgDir = if ($env:FLEET_HOST_CONFIG_DIR) { $env:FLEET_HOST_CONFIG_DIR } else { Join-Path $env:USERPROFILE ".config\fleet-host" }
$tokenFile = Join-Path $cfgDir "machine.token"
$alreadyPaired = $false
if (Test-Path -LiteralPath $tokenFile) {
    $existing = ([string](Get-Content -Raw -LiteralPath $tokenFile -ErrorAction SilentlyContinue)).Trim()
    if ($existing) { $alreadyPaired = $true }
}
if (-not $pairToken -and -not $alreadyPaired) {
    throw "FLEET_PAIR_TOKEN is not set"
}
if ($pairToken) {
    $env:FLEET_PAIR_TOKEN = $pairToken
}

function Normalize-PairUrl([string]$Url) {
    if (-not $Url) { return "" }
    return $Url.Trim().TrimEnd("/")
}

function Get-PairedControlPlane([string]$Dir) {
    $toml = Join-Path $Dir "config.toml"
    if (-not (Test-Path -LiteralPath $toml)) { return "" }
    foreach ($line in Get-Content -LiteralPath $toml) {
        if ($line -match '^\s*control_plane_url\s*=\s*"(.*)"\s*$') {
            return [string]$Matches[1]
        }
    }
    return ""
}

# Same URL trim as fleet-host refuse_url_switch. y/yes switches; anything else keeps the old pair.
function Confirm-UnpairAndPair([string]$Old, [string]$New) {
    if ($env:FLEET_PAIR_FORCE -eq "1") { return $true }
    Write-Host "This machine is already paired to $Old."
    Write-Host "Pair to $New instead? That unpairs the old control plane and rotates the machine token."
    $ans = Read-Host "Unpair and pair to $New? [y/N]"
    return ($ans -match '^[yY]([eE][sS])?$')
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
if (Test-FleetUrlLoopback $Origin) {
    Write-Warning "FLEET_URL $Origin is loopback. Another computer needs a LAN or public control-plane URL."
    $healthOk = $false
    try {
        $null = Invoke-WebRequest -UseBasicParsing -TimeoutSec 5 -Uri "$Origin/health"
        $healthOk = $true
    } catch { }
    if (-not $healthOk) {
        throw "refusing to pair: $Origin is loopback and this host is not the control plane"
    }
}

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
Write-Host "  fleet-driver runs each job"

# A running fleet-host.exe locks the PE; replace would leave the old process.
Get-Process -Name fleet-host -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep -Milliseconds 400

$base = "$Origin/install/worker/bin/windows-x86_64"
$hostReady = $false
foreach ($bin in @("fleet-host.exe", "fleet-driver.exe")) {
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

# .cmd in Startup opens a console (black flash). A .vbs Run(..., 0) does not.
$startupCmd = Join-Path $env:APPDATA "Microsoft\Windows\Start Menu\Programs\Startup\fleet-host.cmd"
$startupVbs = Join-Path $env:APPDATA "Microsoft\Windows\Start Menu\Programs\Startup\fleet-host.vbs"
if (Test-Path -LiteralPath $startupCmd) {
    Remove-Item -LiteralPath $startupCmd -Force
}
$exe = (Join-Path $BinDir "fleet-host.exe") -replace '\\', '\\'
$vbs = @"
Set sh = CreateObject("WScript.Shell")
sh.Run """$exe"" run", 0, False
"@
Set-Content -Path $startupVbs -Value $vbs -Encoding ASCII
Write-Host "startup shortcut: $startupVbs"

if ($pairToken) {
    $existingUrl = Normalize-PairUrl (Get-PairedControlPlane $cfgDir)
    $wantUrl = Normalize-PairUrl $Origin
    $pairArgs = @("pair", "--url", $Origin, "--token", $pairToken)
    if ($alreadyPaired -and $existingUrl -and ($existingUrl -ne $wantUrl)) {
        if (-not (Confirm-UnpairAndPair $existingUrl $wantUrl)) {
            throw "kept existing pair to $existingUrl"
        }
        $pairArgs += "--force"
    }
    Write-Host "pairing with the control plane"
    & (Join-Path $BinDir "fleet-host.exe") @pairArgs
    Write-Host "starting fleet-host (heartbeats; offline after 90s if this fails)"
    Start-Process -FilePath (Join-Path $BinDir "fleet-host.exe") -ArgumentList "run" -WindowStyle Hidden
    Write-Host "started fleet-host run (also at logon: $startupVbs)"
} elseif ($alreadyPaired) {
    Write-Host "already paired; starting fleet-host so the new bins are what heartbeats"
    Start-Process -FilePath (Join-Path $BinDir "fleet-host.exe") -ArgumentList "run" -WindowStyle Hidden
    Write-Host "started fleet-host run (also at logon: $startupVbs)"
} else {
    Write-Host "next: fleet-host pair --url $Origin --token <pair-token-from-ui>"
    Write-Host "then: Start-Process fleet-host.exe -ArgumentList run"
    Write-Host "local settings: %USERPROFILE%\.config\fleet-host\config.toml  (run_host / run_container)"
}
