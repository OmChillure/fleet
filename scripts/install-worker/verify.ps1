# Compatibility entry for the old verify.ps1 URL.
# The check and the repair live in install-worker.ps1 (FLEET_ACTION=verify|repair).
$ErrorActionPreference = "Stop"
if (-not $env:FLEET_ACTION) { $env:FLEET_ACTION = "verify" }
if (-not $env:FLEET_URL) { throw "set FLEET_URL to the control plane" }
$url = $env:FLEET_URL.Trim().TrimEnd("/") + "/install-worker.ps1"
Invoke-Expression (Invoke-RestMethod -UseBasicParsing $url)
