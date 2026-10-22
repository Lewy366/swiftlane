# Removes the machine-wide parts of Swiftlane: program folder, firewall rules, all-users Start menu
# entry and the Settings > Apps entry (also leftovers of the old name, PocketDrop).
# Started (as administrator) by "Swiftlane.exe --uninstall", which removes the per-user parts itself.
param([int]$WaitPid = 0, [string]$InstallDir = "")
$ErrorActionPreference = 'SilentlyContinue'

# Let the uninstall window close first, so its files can be deleted.
if ($WaitPid) { Wait-Process -Id $WaitPid -Timeout 60 }
Get-Process Swiftlane, PocketDrop | Stop-Process -Force
Start-Sleep -Milliseconds 700

if (-not $InstallDir) { $InstallDir = Join-Path $env:ProgramFiles 'Swiftlane' }
Get-NetFirewallRule -DisplayName 'Swiftlane', 'PocketDrop' | Remove-NetFirewallRule
foreach ($app in 'Swiftlane', 'PocketDrop') {
    Remove-Item (Join-Path $env:ProgramData "Microsoft\Windows\Start Menu\Programs\$app.lnk") -Force
    Remove-Item "HKLM:\Software\Microsoft\Windows\CurrentVersion\Uninstall\$app" -Recurse -Force
}
Remove-Item (Join-Path $env:ProgramFiles 'PocketDrop') -Recurse -Force
# A few tries: Windows can hold on to a just-closed program's files for a moment.
foreach ($i in 1..5) {
    if (-not (Test-Path $InstallDir)) { break }
    Remove-Item $InstallDir -Recurse -Force
    Start-Sleep -Seconds 1
}
