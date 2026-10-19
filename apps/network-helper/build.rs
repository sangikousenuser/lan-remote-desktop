fn main() {
    println!("cargo:rerun-if-changed=native/probe.c");
    println!("cargo:rerun-if-env-changed=SDKROOT");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }
    let arch = if std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("aarch64") {
        "arm64"
    } else {
        "x86_64"
    };
    let minimum = if arch == "arm64" { "11.0" } else { "10.13" };
    let out = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    assert!(std::process::Command::new("xcrun")
        .args([
            "clang",
            "-c",
            "native/probe.c",
            "-arch",
            arch,
            &format!("-mmacosx-version-min={minimum}"),
            "-Wall",
            "-Wextra",
            "-Werror",
            "-O2",
            "-o"
        ])
        .arg(out.join("probe.o"))
        .status()
        .unwrap()
        .success());
    assert!(std::process::Command::new("xcrun")
        .args(["ar", "crs"])
        .arg(out.join("liblrd_probe.a"))
        .arg(out.join("probe.o"))
        .status()
        .unwrap()
        .success());
    println!("cargo:rustc-link-search=native={}", out.display());
    println!("cargo:rustc-link-lib=static=lrd_probe");
}
