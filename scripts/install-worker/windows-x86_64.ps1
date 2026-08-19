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

$base = "$Origin/install/worker/bin/windows-x86_64"
$installed = $false
foreach ($bin in @("fleet-host.exe", "fleet-worker.exe", "fleet-driver.exe")) {
    $dest = Join-Path $BinDir $bin
    $url = "$base/$bin"
    try {
        Invoke-WebRequest -UseBasicParsing -Uri $url -OutFile $dest
        Write-Host "installed $dest"
        $installed = $true
    } catch {
        Write-Host "no prebuilt $bin at $url"
    }
}
if (-not (Test-Path (Join-Path $BinDir "fleet-host.exe"))) {
    throw @"
control plane did not serve fleet-host.exe for windows-x86_64.
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
    & (Join-Path $BinDir "fleet-host.exe") pair --url $Origin --token $env:FLEET_PAIR_TOKEN
    # Pair is a handshake. Offline after 30s unless run is started now.
    Start-Process -FilePath (Join-Path $BinDir "fleet-host.exe") -ArgumentList "run" -WindowStyle Hidden
    Write-Host "started fleet-host run (also at logon: $startup)"
} else {
    Write-Host "next: fleet-host pair --url $Origin --token <pair-token-from-ui>"
    Write-Host "then: Start-Process fleet-host.exe -ArgumentList run"
    Write-Host "local settings: %USERPROFILE%\.config\fleet-host\config.toml  (run_host / run_container)"
}
