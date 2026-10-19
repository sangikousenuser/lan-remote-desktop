use super::*;
use crate::updates::{
    tests::{fixture, policy, sign},
    Entry, Manifest, VerifiedManifest,
};
use ring::signature::KeyPair;
struct Sandbox(PathBuf);
impl Sandbox {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "lrd-sealed-update-{}",
            crate::random_u64().unwrap()
        ));
        identity::private_directory(&path).unwrap();
        Self(path)
    }
}
impl Drop for Sandbox {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}
fn command(command: &mut Command) {
    let result = command.output().unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
}
fn app(sandbox: &Sandbox, name: &str, version: &str) -> PathBuf {
    let root = sandbox.0.join(name);
    for path in ["Contents/MacOS", "Contents/Helpers", "Contents/Resources"] {
        std::fs::create_dir_all(root.join(path)).unwrap();
    }
    let source = sandbox.0.join("fixture.c");
    std::fs::write(&source, b"int main(void){return 0;}").unwrap();
    let executable = root.join("Contents/MacOS/lrd-macos-client");
    command(
        Command::new("/usr/bin/xcrun")
            .arg("clang")
            .arg(&source)
            .arg("-o")
            .arg(&executable),
    );
    let helper = root.join("Contents/Helpers/lrd-update-helper");
    std::fs::copy(&executable, &helper).unwrap();
    let info = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><plist version="1.0"><dict>
        <key>CFBundleIdentifier</key><string>local.lrd.update-fixture</string>
        <key>CFBundleExecutable</key><string>lrd-macos-client</string>
        <key>CFBundlePackageType</key><string>APPL</string>
        <key>CFBundleVersion</key><string>{version}</string>
        </dict></plist>"#
    );
    std::fs::write(root.join("Contents/Info.plist"), info).unwrap();
    std::fs::write(root.join("Contents/Resources/version.txt"), version).unwrap();
    command(
        Command::new("/usr/bin/codesign")
            .args(["--force", "--sign", "-"])
            .arg(helper),
    );
    command(
        Command::new("/usr/bin/codesign")
            .args(["--force", "--sign", "-"])
            .arg(&root),
    );
    verify(&root).unwrap();
    root
}
fn package(source: &Path) -> (VerifiedPackage, [u8; 32]) {
    let (key, _, _) = fixture();
    let public: [u8; 32] = key.public_key().as_ref().try_into().unwrap();
    fn files(root: &Path, path: &Path, paths: &mut Vec<PathBuf>) {
        for item in std::fs::read_dir(root.join(path)).unwrap() {
            let item = item.unwrap();
            let relative = path.join(item.file_name());
            if item.file_type().unwrap().is_dir() {
                files(root, &relative, paths);
            } else {
                paths.push(relative);
            }
        }
    }
    let mut paths = Vec::new();
    files(source, Path::new(""), &mut paths);
    paths.sort();
    let mut bytes = b"LRDUPD1\n".to_vec();
    let mut entries = Vec::new();
    for path in paths {
        let data = std::fs::read(source.join(&path)).unwrap();
        let executable = std::os::unix::fs::PermissionsExt::mode(
            &std::fs::metadata(source.join(&path)).unwrap().permissions(),
        ) & 0o111
            != 0;
        entries.push(Entry {
            path: path.to_str().unwrap().into(),
            size: data.len() as u64,
            sha256: identity::fingerprint(&data),
            executable,
        });
        bytes.extend(data);
    }
    let manifest = Manifest {
        schema: 1,
        version: "1.0.0".into(),
        target: "x86_64-apple-darwin".into(),
        minimum_os: "10.13.0".into(),
        protocol_major: 1,
        package_url: "https://example.invalid/fixture.lrdpkg".into(),
        package_size: bytes.len() as u64,
        package_sha256: identity::fingerprint(&bytes),
        files: entries,
    };
    let package = VerifiedManifest::verify(&sign(&key, &manifest), &policy(&public))
        .unwrap()
        .verify_package(bytes)
        .unwrap();
    (package, public)
}
#[test]
fn sealed_bundle_survives_journaling_exchange_and_interrupted_health_recovery() {
    let sandbox = Sandbox::new();
    let old = app(&sandbox, "Current.app", "0.9.0");
    let new = app(&sandbox, "Release.app", "1.0.0");
    let (package, public) = package(&new);
    let policy = policy(&public);
    let tx = Transaction::open_application(&old).unwrap();
    tx.prepare(&package).unwrap();
    assert!(!old.join(".lrd-update").exists());
    verify(&old).unwrap();
    tx.apply(&policy).unwrap();
    verify(&old).unwrap();
    assert_eq!(
        std::fs::read(old.join("Contents/Resources/version.txt")).unwrap(),
        b"1.0.0"
    );
    // A crash can happen between the atomic filesystem exchange and the state
    // write. Recovery must identify both complete bundles by authenticated hashes.
    let (work, mut state, _) = tx.load(&policy).unwrap();
    state.phase = Phase::Applying;
    save(&work, &state).unwrap();
    drop(work);
    drop(tx);
    let tx = Transaction::open_application(&old).unwrap();
    assert!(tx.recover(&policy).unwrap());
    verify(&old).unwrap();
    assert_eq!(
        std::fs::read(old.join("Contents/Resources/version.txt")).unwrap(),
        b"0.9.0"
    );
    // Reuse the reopened transaction: its original directory descriptor points
    // at the failed bundle after rollback, so preparation must resolve live root.
    tx.prepare(&package).unwrap();
    tx.apply(&policy).unwrap();
    tx.confirm_health(&policy).unwrap();
    verify(&old).unwrap();
    assert!(!tx.recovery_needed(&policy).unwrap());
    assert_eq!(
        std::fs::read(old.join("Contents/Resources/version.txt")).unwrap(),
        b"1.0.0"
    );
}
#[test]
fn changed_bundle_member_prevents_exchange_and_legacy_journal_is_moved_outside_seal() {
    let sandbox = Sandbox::new();
    let old = app(&sandbox, "Current.app", "0.9.0");
    let new = app(&sandbox, "Release.app", "1.0.0");
    let (package, public) = package(&new);
    let policy = policy(&public);
    std::fs::create_dir(old.join(".lrd-update")).unwrap();
    assert!(verify(&old).is_err());
    let tx = Transaction::open_application(&old).unwrap();
    verify(&old).unwrap();
    tx.prepare(&package).unwrap();
    std::fs::write(old.join("Contents/Resources/version.txt"), b"local change").unwrap();
    assert!(tx.apply(&policy).is_err());
    assert_eq!(
        std::fs::read(old.join("Contents/Resources/version.txt")).unwrap(),
        b"local change"
    );
    assert!(tx.recover(&policy).is_err());
}
