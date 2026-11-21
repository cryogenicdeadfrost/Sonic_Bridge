param(
    [ValidatePattern('^[A-Za-z0-9.\-\[\]:]+:[0-9]{1,5}$')]
    [string]$Target = "127.0.0.1:8765"
)

$ErrorActionPreference = "Stop"

$portText = ($Target -split ':')[-1].TrimEnd(']')
$port = [int]$portText
if ($port -lt 1 -or $port -gt 65535) {
    throw "Target port must be between 1 and 65535."
}

$exe = Join-Path $PSScriptRoot "..\target\release\sonic-bridge-client.exe"
if (-not (Test-Path $exe)) {
    throw "Build the Windows client first with: cargo build --release -p sonic-bridge-client"
}

$taskName = "SonicBridge"
# Keep the target quoted as one argument, even when a future address format has special characters.
$action = New-ScheduledTaskAction -Execute $exe -Argument "--target `"$Target`""
$trigger = New-ScheduledTaskTrigger -AtLogOn
$settings = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -StartWhenAvailable

Register-ScheduledTask -TaskName $taskName -Action $action -Trigger $trigger -Settings $settings -Force | Out-Null

Write-Host "SonicBridge will start automatically at logon."
Write-Host "Target: $Target"
