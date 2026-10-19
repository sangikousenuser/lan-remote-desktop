# Windows Host packaging

Build on Windows MSVC, or cross-compile x64 GNU for development. NSIS 3 is required for the optional setup EXE.

```powershell
cargo build --locked --release -p lrd-windows-host -p lrd-update-helper -p lrd-network-helper
python3 packaging/windows/package.py --binary target/release/lrd-windows-host.exe --installer
```

The portable ZIP contains the host, an Ethernet selector launcher, an explicit narrow firewall script and manual tests. The installer uses MultiUser for per-user / per-machine, refuses mixed modes, preserves user data on uninstall, and offers logon startup as an optional component. Both modes run capture in the interactive user session. No service, fixed IP, DNS or broad firewall rule is installed automatically.

The local temporary-IP wizard (`--network-wizard`, launcher F, or tray menu) invokes the bundled versioned network helper through UAC. It only adds the fixed /30 alias to the selected physical Ethernet after conflict checks. Unplug, restore, DHCP recovery or expiry removes it. The normal Host stays unelevated. Real network changes remain a final hardware test.

The installer is generated and inspected on the development Mac. Installation, uninstall, UAC, multiple users and signed updating still require Windows testing. It is an alpha artifact, not a completed Stable release.
