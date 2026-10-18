#!/usr/bin/env python3
"""Build a deterministic-layout, AppKit client bundle. Never publishes."""
import argparse
import hashlib
import os
import pathlib
import plistlib
import shutil
import subprocess
import sys
import zipfile

ROOT = pathlib.Path(__file__).resolve().parents[2]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target", choices=["x86_64-apple-darwin", "aarch64-apple-darwin"], default="x86_64-apple-darwin")
    parser.add_argument("--sdk", type=pathlib.Path, help="explicit macOS SDK root")
    args = parser.parse_args()
    if sys.platform != "darwin":
        parser.error("build on macOS with Xcode Command Line Tools")
    environment = dict(os.environ, MACOSX_DEPLOYMENT_TARGET="10.13" if args.target.startswith("x86_64") else "11.0")
    # The 27.0 CoreMedia stub redirects pre-10.14 clients to a Swift dylib.
    # Apple's python3 launcher injects the default SDKROOT. Prefer the installed
    # C-ABI SDK over that default; --sdk always chooses the SDK explicitly.
    legacy_sdk = pathlib.Path("/Library/Developer/CommandLineTools/SDKs/MacOSX26.5.sdk")
    current_sdk = pathlib.Path(environment.get("SDKROOT", "/Library/Developer/CommandLineTools/SDKs/MacOSX.sdk")).resolve()
    if args.sdk:
        environment["SDKROOT"] = str(args.sdk.resolve(strict=True))
    elif legacy_sdk.exists() and ("SDKROOT" not in environment or current_sdk.name == "MacOSX27.0.sdk"):
        environment["SDKROOT"] = str(legacy_sdk)
    print("Build SDK:", environment.get("SDKROOT", "toolchain default"), flush=True)
    subprocess.run(["cargo", "build", "--locked", "--release", "-p", "lrd-macos-client", "-p", "lrd-update-helper", "--target", args.target], cwd=ROOT, env=environment, check=True)
    source = ROOT / "target" / args.target / "release" / "lrd-macos-client"
    subprocess.run([sys.executable, str(ROOT / "packaging/macos/verify_binary.py"), str(source), "--target", args.target], check=True)
    destination = ROOT / "dist" / args.target
    app = destination / "LAN Remote Desktop.app"
    if app.exists():
        shutil.rmtree(app)
    executable = app / "Contents/MacOS/lrd-macos-client"
    resources = app / "Contents/Resources"
    executable.parent.mkdir(parents=True)
    resources.mkdir()
    shutil.copy2(source, executable)
    helper = app / "Contents/Helpers/lrd-update-helper"
    helper.parent.mkdir()
    helper_source = source.with_name("lrd-update-helper")
    subprocess.run([sys.executable, str(ROOT / "packaging/macos/verify_binary.py"), str(helper_source), "--target", args.target], check=True)
    shutil.copy2(helper_source, helper)
    shutil.copy2(ROOT / "assets/test-pattern.mp4", resources / "test-pattern.mp4")
    shutil.copy2(ROOT / "docs/manual-testing.md", resources / "manual-testing.md")
    with (app / "Contents/Info.plist").open("wb") as stream:
        plistlib.dump({
            "CFBundleIdentifier": "local.lanremotedesktop.client",
            "CFBundleName": "LAN Remote Desktop",
            "CFBundleDisplayName": "LAN Remote Desktop",
            "CFBundleExecutable": "lrd-macos-client",
            "CFBundlePackageType": "APPL",
            "CFBundleShortVersionString": "0.9.0",
            "CFBundleVersion": "9.0.1",
            "LSMinimumSystemVersion": "10.13" if args.target.startswith("x86_64") else "11.0",
            "NSHighResolutionCapable": True,
            "NSPrincipalClass": "NSApplication",
        }, stream)
    # An ad-hoc signature is required to run arm64 executables on modern Macs.
    # It is not a Developer ID or update signature.
    if args.target.startswith("aarch64"):
        subprocess.run(["codesign", "--force", "--sign", "-", str(helper)], check=True)
        subprocess.run(["codesign", "--force", "--sign", "-", str(app)], check=True)
    archive = destination / "LAN-Remote-Desktop-macOS-client.zip"
    # ditto preserves bundle metadata, modes and signatures on macOS.
    subprocess.run(["ditto", "-c", "-k", "--sequesterRsrc", "--keepParent", str(app), str(archive)], check=True)
    digest = hashlib.sha256(archive.read_bytes()).hexdigest()
    (destination / "SHA256SUMS").write_text(f"{digest}  {archive.name}\n")
    with zipfile.ZipFile(archive) as check:
        assert check.testzip() is None
        assert "LAN Remote Desktop.app/Contents/Info.plist" in check.namelist()
    print(app)
    print(archive)


if __name__ == "__main__":
    main()
