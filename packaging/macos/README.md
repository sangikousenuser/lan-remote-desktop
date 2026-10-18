# macOS .app package

```sh
python3 packaging/macos/package.py                              # Intel, minimum 10.13
python3 packaging/macos/package.py --target aarch64-apple-darwin  # Apple Silicon, minimum 11
```

The bundle includes an AppKit Ethernet client and offline H.264 fixture. Mach-O architecture, minimum OS and dylib dependencies are checked. SDK 27 may redirect old CoreMedia clients to a missing Swift dylib; installed SDK 26.5 is preferred here. `--sdk` overrides the choice.

arm64 receives an ad-hoc execution signature; this is not Developer ID or an update signature. Intel is unsigned. Use the OS's app-specific first-open approval if offered; do not disable Gatekeeper globally.

Move the app to Applications before enabling its optional per-user LaunchAgent from the app menu. It monitors only the chosen Ethernet and asks before connecting. Toggle the menu again to remove it. The build/packaging script does not enable startup.

High Sierra compatibility and signed update apply/rollback remain acceptance gates. See implementation-status.md and manual-testing.md.
