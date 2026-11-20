# Installs (or with -Remove, removes) Swiftlane's AirPlay receiver. Started as administrator by Swiftlane.
# The receiver goes into its own folder under Program Files, which only administrators can change, so
# nothing else on the PC can swap uxplay.exe for something that rides on its firewall rule.
# The zip is read once and checked in memory; only that checked copy is unpacked (no swap in between).
param([string]$Zip = "", [string]$Sha256 = "", [switch]$Remove)
$ErrorActionPreference = 'Stop'
$Dest = Join-Path $env:ProgramFiles 'Swiftlane AirPlay'
$Rule = 'Swiftlane AirPlay'

Get-Process uxplay -ErrorAction SilentlyContinue | Where-Object { $_.Path -like "$Dest\*" } | Stop-Process -Force
Get-NetFirewallRule -DisplayName $Rule -ErrorAction SilentlyContinue | Remove-NetFirewallRule
if ($Remove) {
    if (Test-Path $Dest) { Remove-Item $Dest -Recurse -Force }
    exit 0
}

$bytes = [IO.File]::ReadAllBytes($Zip)
$hash = -join ([Security.Cryptography.SHA256]::Create().ComputeHash($bytes) | ForEach-Object { $_.ToString('x2') })
if ($hash -ne $Sha256.ToLower()) { exit 3 }

$new = "$Dest.new"
$old = "$Dest.old"
foreach ($d in $new, $old) { if (Test-Path $d) { Remove-Item $d -Recurse -Force } }
New-Item -ItemType Directory $new | Out-Null
Add-Type -AssemblyName System.IO.Compression, System.IO.Compression.FileSystem
$archive = New-Object IO.Compression.ZipArchive (New-Object IO.MemoryStream (, $bytes))
try { [IO.Compression.ZipFileExtensions]::ExtractToDirectory($archive, $new) } finally { $archive.Dispose() }
if (-not (Test-Path "$new\uxplay.exe")) { Remove-Item $new -Recurse -Force; exit 4 }

if (Test-Path $Dest) { Move-Item $Dest $old }
Move-Item $new $Dest
if (Test-Path $old) { Remove-Item $old -Recurse -Force }

# only this program, only home (Private) networks
New-NetFirewallRule -DisplayName $Rule -Direction Inbound -Action Allow -Profile Private -Program "$Dest\uxplay.exe" | Out-Null
exit 0
