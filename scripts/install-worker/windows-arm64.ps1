# Windows ARM64 worker installer.
$ErrorActionPreference = "Stop"
$env:FLEET_PLATFORM = "windows-arm64"
$Origin = $env:FLEET_INSTALL_ORIGIN
if (-not $Origin) { $Origin = $env:FLEET_URL }
if (-not $Origin) { throw "set FLEET_URL to the control plane" }
$Origin = $Origin.TrimEnd("/")

$Prefix = $env:FLEET_PREFIX
if (-not $Prefix) { $Prefix = Join-Path $env:LOCALAPPDATA "fleet-host" }
$BinDir = Join-Path $Prefix "bin"
New-Item -ItemType Directory -Force -Path $BinDir | Out-Null

function Get-RemoteSha256([string]$Url) {
    try {
        $r = Invoke-WebRequest -UseBasicParsing -TimeoutSec 30 -Uri $Url
        return (($r.Content.ToString().Trim() -split "\s+")[0]).ToLowerInvariant()
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

Write-Host "this machine is a self-hosted worker"
Write-Host "  fleet-host   stays up; pairs and takes jobs"
Write-Host "  fleet-worker runs each job"
Write-Host "  fleet-driver runs jobs that use an agent harness (Grok)"

$base = "$Origin/install/worker/bin/windows-arm64"
$installed = $false
foreach ($bin in @("fleet-host.exe", "fleet-worker.exe", "fleet-driver.exe")) {
    $dest = Join-Path $BinDir $bin
    $url = "$base/$bin"
    $tmp = "$dest.tmp"
    Write-Host "checking $bin"
    $want = Get-RemoteSha256 "$url.sha256"
    if ($want -and (Test-Path $dest) -and ((Get-LocalSha256 $dest) -eq $want)) {
        Write-Host "unchanged $dest"
        $installed = $true
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
        Move-Item -Force $tmp $dest
        Write-Host "installed $dest"
        $installed = $true
    } catch {
        if (Test-Path $tmp) { Remove-Item -Force $tmp }
        Write-Host "no prebuilt $bin at $url"
    }
}
# gnullvm PE imports libunwind.dll. 404 = skip (x86_64 gnu unchanged).
foreach ($dll in @("libunwind.dll")) {
    $dest = Join-Path $BinDir $dll
    $url = "$base/$dll"
    $tmp = "$dest.tmp"
    Write-Host "checking $dll"
    $want = Get-RemoteSha256 "$url.sha256"
    if ($want -and (Test-Path $dest) -and ((Get-LocalSha256 $dest) -eq $want)) {
        Write-Host "unchanged $dest"
        continue
    }
    Write-Host "downloading $dll"
    try {
        Save-Url $url $tmp
        if ($want) {
            $got = Get-LocalSha256 $tmp
            if ($got -ne $want) { throw "checksum mismatch $dll" }
        }
        Move-Item -Force $tmp $dest
        Write-Host "installed $dest"
    } catch {
        if (Test-Path $tmp) { Remove-Item -Force $tmp }
        Write-Host "no runtime $dll at $url"
    }
}
if (-not (Test-Path (Join-Path $BinDir "fleet-host.exe"))) {
    throw @"
control plane did not serve fleet-host.exe for windows-arm64.
Build with: cargo build --release -p fleet-host
and set FLEET_WORKER_DIST_DIR on the control plane.
"@
}

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
