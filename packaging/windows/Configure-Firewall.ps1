# Explicit, local administrator operation. Never broadens all-program access.
param([Parameter(Mandatory=$true)][ValidateRange(1,2147483647)][int]$InterfaceIndex,
      [switch]$Remove)
$ErrorActionPreference = 'Stop'
$principal = [Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) { throw '管理者のPowerShellで実行してください。' }
$exe = (Resolve-Path (Join-Path $PSScriptRoot 'lrd-windows-host.exe')).Path
$adapter = Get-NetAdapter -InterfaceIndex $InterfaceIndex
if ($adapter.Virtual -or -not $adapter.HardwareInterface -or -not $adapter.ConnectorPresent -or $adapter.MediaType -ne '802.3' -or $adapter.Status -ne 'Up') { throw '接続中の物理Ethernetだけを指定できます。' }
$prefix = 'LANRemoteDesktop-' + (Get-FileHash $exe -Algorithm SHA256).Hash.Substring(0,12) + '-' + $InterfaceIndex
if ($Remove) { Get-NetFirewallRule -Name ($prefix + '*') -ErrorAction SilentlyContinue | Remove-NetFirewallRule; return }
$tcpPorts = @(0..31 | ForEach-Object { 47990 + $_ * 32 })
$udpPorts = @($tcpPorts | ForEach-Object { $_ + 1; $_ + 2 })
foreach ($item in @(@('TCP',$tcpPorts),@('UDP',$udpPorts))) {
  Get-NetFirewallRule -Name ($prefix + '-' + $item[0]) -ErrorAction SilentlyContinue | Remove-NetFirewallRule
  New-NetFirewallRule -Name ($prefix + '-' + $item[0]) -DisplayName ('LAN Remote Desktop — ' + $adapter.Name + ' ' + $item[0]) -Direction Inbound -Action Allow -Enabled True -Profile Any -Program $exe -Protocol $item[0] -LocalPort $item[1] -InterfaceAlias $adapter.Name -RemoteAddress LocalSubnet -EdgeTraversalPolicy Block | Out-Null
}
Write-Host 'この実行ファイル・Ethernet・LAN内・32ユーザー分の限定ポートだけの受信規則を追加しました。-Removeで削除できます。'
