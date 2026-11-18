param(
    [string]$Target = "127.0.0.1:8765"
)

$ErrorActionPreference = "Stop"

$exe = Join-Path $PSScriptRoot "..\target\release\sonic-bridge-client.exe"

if (-not (Test-Path $exe)) {
    throw "Build the Windows client first with: cargo build --release -p sonic-bridge-client"
}

$taskName = "SonicBridge"
$action = New-ScheduledTaskAction -Execute $exe -Argument "--target $Target"
$trigger = New-ScheduledTaskTrigger -AtLogOn
$settings = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries

Register-ScheduledTask -TaskName $taskName -Action $action -Trigger $trigger -Settings $settings -Force | Out-Null

Write-Host "SonicBridge will start automatically at logon."
Write-Host "Target: $Target"
