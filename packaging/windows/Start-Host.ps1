# Run in the logged-in Windows 11 desktop; no administrator rights needed.
$ErrorActionPreference = 'Stop'
$hostExe = Join-Path $PSScriptRoot 'lrd-windows-host.exe'
Write-Host 'LAN Remote Desktop — Windows Host'
& $hostExe --interfaces
$nicIndex = Read-Host 'Ethernetの番号を入力してください'
if ($nicIndex -notmatch '^\d+$' -or [uint32]$nicIndex -eq 0) { throw 'Invalid interface index' }
Write-Host '初回はMacとWindowsの確認コードが同じことを確かめて、両方で許可してください。'
Write-Host '終了・ファイル送信・受信フォルダーは通知領域アイコンの右クリックから操作できます。'
& $hostExe --serve --interface $nicIndex --auto
if ($LASTEXITCODE -ne 0) { Read-Host '終了しました。Enterで閉じます' }
