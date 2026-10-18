# Local diagnostics only. Records numeric counters; does not save the screen.
$ErrorActionPreference = 'Stop'
$HostProbe = Join-Path $PSScriptRoot 'lrd-windows-host.exe'
$ResultPath = Join-Path $PSScriptRoot 'windows-capture-diagnostics.json'
Write-Host 'LAN Remote Desktop: local DXGI capture test (no networking)'
& $HostProbe --list-monitors
if ($LASTEXITCODE -ne 0) { throw 'Monitor enumeration failed' }
$AdapterNumber = Read-Host 'Adapter number (Enter for 0)'
$MonitorNumber = Read-Host 'Monitor number (Enter for 0)'
if (-not $AdapterNumber) { $AdapterNumber = '0' }
if (-not $MonitorNumber) { $MonitorNumber = '0' }
if ($AdapterNumber -notmatch '^\d+$' -or $MonitorNumber -notmatch '^\d+$') { throw 'Use nonnegative numbers' }
Write-Host 'Move a window for 10 seconds so the desktop produces new frames.'
$ProbeJson = & $HostProbe --probe --adapter $AdapterNumber --monitor $MonitorNumber --seconds 10
if ($LASTEXITCODE -ne 0) { throw 'Capture probe failed; see the error above' }
$null = $ProbeJson | ConvertFrom-Json
[IO.File]::WriteAllText($ResultPath, ($ProbeJson -join [Environment]::NewLine), [Text.UTF8Encoding]::new($false))
Write-Host "Saved numeric diagnostics: $ResultPath"
