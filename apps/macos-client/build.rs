use std::{env, path::PathBuf, process::Command};

fn run(command: &mut Command) {
    let status = command.status().expect("could not run native build tool");
    assert!(status.success(), "native build failed: {command:?}");
}

fn main() {
    println!("cargo:rerun-if-changed=native/client.m");
    println!("cargo:rerun-if-changed=native/network-ui.inc");
    println!("cargo:rerun-if-changed=native/transfer-ui.inc");
    println!("cargo:rerun-if-changed=native/audio.inc");
    println!("cargo:rerun-if-changed=native/display-ui.inc");
    println!("cargo:rerun-if-changed=native/agent-ui.inc");
    println!("cargo:rerun-if-changed=native/file-drop.inc");
    println!("cargo:rerun-if-changed=native/clipboard-ui.inc");
    println!("cargo:rerun-if-changed=native/network-support.inc");
    println!("cargo:rerun-if-changed=native/dashboard.inc");
    println!("cargo:rerun-if-env-changed=MACOSX_DEPLOYMENT_TARGET");
    println!("cargo:rerun-if-env-changed=SDKROOT");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }
    let arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap();
    let arch = if arch == "aarch64" { "arm64" } else { "x86_64" };
    let minimum = if arch == "arm64" { "11.0" } else { "10.13" };
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    if let Ok(sdk) = env::var("SDKROOT") {
        // Cargo does not fingerprint rustc's implicit SDK choice. Make the selected
        // SDK an explicit link argument as well as a native compiler environment.
        println!("cargo:rustc-link-arg=-Wl,-syslibroot,{sdk}");
    }
    run(Command::new("xcrun")
        .args(["clang", "-c", "native/client.m", "-o"])
        .arg(out.join("client.o"))
        .args([
            "-arch",
            arch,
            &format!("-mmacosx-version-min={minimum}"),
            &format!("-DLRD_ARCH=\"{arch}\""),
            "-fobjc-arc",
            "-fblocks",
            "-Wall",
            "-Wextra",
            "-Werror",
            "-Wunguarded-availability",
            "-Werror=unguarded-availability-new",
            // New SDKs deprecate the synchronous 10.13-compatible AVAsset APIs.
            "-Wno-deprecated-declarations",
            "-O2",
        ]));
    run(Command::new("xcrun")
        .args(["ar", "crs"])
        .arg(out.join("liblrd_native.a"))
        .arg(out.join("client.o")));
    println!("cargo:rustc-link-search=native={}", out.display());
    println!("cargo:rustc-link-lib=static=lrd_native");
    for framework in [
        "AppKit",
        "Carbon",
        "AudioToolbox",
        "CoreAudio",
        "CoreGraphics",
        "AVFoundation",
        "VideoToolbox",
        "CoreMedia",
        "CoreVideo",
        "CoreImage",
        "Metal",
        "MetalKit",
        "QuartzCore",
    ] {
        println!("cargo:rustc-link-lib=framework={framework}");
    }
    println!("cargo:rustc-link-lib=objc");
}
