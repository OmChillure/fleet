# Read-only install check for an offline self-hosted runner (Windows).
# Does not start, stop, pair, or print the machine token.
# verdict: not_installed | bins_missing | service_disabled | service_stopped | service_running
$ErrorActionPreference = "Continue"

$cfgDir = if ($env:FLEET_HOST_CONFIG_DIR) { $env:FLEET_HOST_CONFIG_DIR } else { Join-Path $env:USERPROFILE ".config\fleet-host" }
$prefix = if ($env:FLEET_PREFIX) { $env:FLEET_PREFIX } else { Join-Path $env:LOCALAPPDATA "fleet-host" }
$bin = Join-Path $prefix "bin\fleet-host.exe"
$startup = Join-Path $env:APPDATA "Microsoft\Windows\Start Menu\Programs\Startup\fleet-host.cmd"
$tokenPath = Join-Path $cfgDir "machine.token"
$configPath = Join-Path $cfgDir "config.toml"
$hbPath = Join-Path $cfgDir "last_heartbeat.json"

$hasBin = Test-Path -LiteralPath $bin
$hasConfig = Test-Path -LiteralPath $configPath
$hasStartup = Test-Path -LiteralPath $startup
$tokenPresent = $false
if (Test-Path -LiteralPath $tokenPath) {
    $raw = (Get-Content -LiteralPath $tokenPath -Raw -ErrorAction SilentlyContinue)
    if ($null -ne $raw -and $raw.Trim().Length -gt 0) { $tokenPresent = $true }
}
$proc = Get-Process -Name fleet-host -ErrorAction SilentlyContinue

$verdict = "not_installed"
$code = 4
$next = "On Runners > Self-hosted, use Add worker and paste the install command on this computer."
if (-not $hasBin -and -not $hasConfig -and -not $hasStartup) {
    $verdict = "not_installed"
} elseif (-not $hasBin) {
    $verdict = "bins_missing"
    if ($tokenPresent) {
        $next = "The program is gone and the machine token is still here. Re-run the install command from Add worker. Do not re-pair."
    } else {
        $next = "The program is gone. On Runners > Self-hosted, use Add worker and paste the install command on this computer."
    }
} elseif ($null -ne $proc) {
    $verdict = "service_running"
    $code = 0
    $next = "This computer is running fleet-host. If the card stays offline, the process is not reaching the control plane. Do not re-pair."
} elseif (-not $hasStartup) {
    $verdict = "service_disabled"
    $code = 2
    $next = "The program is installed and the logon startup entry is missing. Re-run the install command from Add worker so it is written again. Do not re-pair if the token is present."
} else {
    $verdict = "service_stopped"
    $code = 3
    $next = "Start-Process -FilePath `"$bin`" -ArgumentList run -WindowStyle Hidden"
}

Write-Host "fleet-host verify"
if ($hasBin) { Write-Host "binary:          $bin" } else { Write-Host "binary:          missing" }
if ($hasConfig) { Write-Host "config:          $configPath" } else { Write-Host "config:          missing" }
if ($tokenPresent) { Write-Host "token:           present" } else { Write-Host "token:           missing" }
if ($hasStartup) { Write-Host "service:         logon startup entry present" } else { Write-Host "service:         logon startup entry missing" }
if ($null -ne $proc) { Write-Host "process:         running" } else { Write-Host "process:         not running" }
if (Test-Path -LiteralPath $hbPath) {
    $hb = (Get-Content -LiteralPath $hbPath -Raw -ErrorAction SilentlyContinue) -replace "\s+", " "
    Write-Host "last_heartbeat:  $hb"
} else {
    Write-Host "last_heartbeat:  (none)"
}
Write-Host "verdict:         $verdict"
Write-Host "next:            $next"
exit $code
