//! HTTPS only, bounded downloads via the OS-supplied curl. The update connection
//! uses the normal Internet route; it never goes through the Ethernet session.
use super::{Policy, VerifiedManifest, MAX_PACKAGE};
use crate::{fail, Result};
use std::{
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

pub const REPOSITORY: &str = "https://github.com/sangikousenuser/lan-remote-desktop";
pub const MAX_METADATA: usize = 128 * 1024;

/// Exclude credentials, control characters and ambiguous URL authorities before
/// passing a URL as a single non-shell argument. TLS certificates stay verified.
pub fn validate_url(url: &str) -> Result<()> {
    if !url.starts_with("https://")
        || url.len() > 2048
        || !url.is_ascii()
        || url.bytes().any(|b| b <= b' ' || b == 127 || b == b'\\')
    {
        return fail("HTTPS update endpoint required");
    }
    let authority = url[8..].split(['/', '?', '#']).next().unwrap_or("");
    if authority.is_empty()
        || authority.starts_with('.')
        || authority.ends_with('.')
        || !authority
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.')
        || !authority.contains('.')
        || url.contains('#')
    {
        return fail("invalid update HTTPS authority");
    }
    Ok(())
}

pub trait Fetcher {
    fn fetch(&self, url: &str, maximum: usize) -> Result<Vec<u8>>;
}
pub struct SystemHttps;
impl Fetcher for SystemHttps {
    fn fetch(&self, url: &str, maximum: usize) -> Result<Vec<u8>> {
        validate_url(url)?;
        if maximum == 0 || maximum > MAX_PACKAGE {
            return fail("download size limit");
        }
        #[cfg(windows)]
        let executable = PathBuf::from(std::env::var_os("SystemRoot").ok_or("SystemRoot missing")?)
            .join("System32/curl.exe");
        #[cfg(not(windows))]
        let executable = PathBuf::from("/usr/bin/curl");
        let mut command = Command::new(executable);
        // -q must be first: a per-user .curlrc must not disable verification or
        // change the output path. Redirects may only stay on HTTPS (GitHub CDN).
        command
            .args([
                "-q",
                "--fail",
                "--silent",
                "--show-error",
                "--location",
                "--proto",
                "=https",
                "--proto-redir",
                "=https",
                "--max-redirs",
                "5",
                "--connect-timeout",
                "10",
                "--max-time",
                "120",
                "--tlsv1.2",
                "--max-filesize",
                &maximum.to_string(),
                "--url",
                url,
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x08000000); // CREATE_NO_WINDOW
        }
        let mut child = command.spawn()?;
        let result = read_bounded(child.stdout.take().ok_or("download pipe missing")?, maximum);
        if result.is_err() {
            let _ = child.kill();
        }
        let status = child.wait()?;
        if !status.success() {
            return fail("update unavailable; HTTPS download deferred");
        }
        result
    }
}
fn read_bounded(reader: impl Read, maximum: usize) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader.take(maximum as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > maximum {
        return fail("download exceeded limit");
    }
    Ok(bytes)
}

/// Metadata is authenticated before its package URL is used. An interrupted
/// download produces no ready marker and cannot replace a running application.
pub fn check_and_stage(
    fetcher: &impl Fetcher,
    policy: &Policy<'_>,
    parent: &Path,
) -> Result<PathBuf> {
    if !matches!(
        policy.target,
        "x86_64-pc-windows-msvc" | "x86_64-apple-darwin" | "aarch64-apple-darwin"
    ) {
        return fail("unsupported update platform");
    }
    let url = format!(
        "{REPOSITORY}/releases/latest/download/manifest-{}.json",
        policy.target
    );
    let envelope = fetcher.fetch(&url, MAX_METADATA)?;
    let floor_path = parent.join("highest-staged.json");
    let floor = if floor_path.exists() {
        let value: String =
            serde_json::from_slice(&crate::identity::read_private(&floor_path, 1024)?)?;
        semver::Version::parse(&value)?;
        Some(value)
    } else {
        None
    };
    let highest = [floor.as_deref(), policy.highest_staged]
        .into_iter()
        .flatten()
        .map(|s| semver::Version::parse(s).map(|v| (v, s)))
        .collect::<std::result::Result<Vec<_>, _>>()?
        .into_iter()
        .max_by(|a, b| a.0.cmp(&b.0))
        .map(|(_, s)| s);
    let effective = Policy {
        highest_staged: highest,
        ..*policy
    };
    let manifest = VerifiedManifest::verify(&envelope, &effective)?;
    crate::identity::private_directory(parent)?;
    if let Some((path, existing)) = super::newest_staged(parent, &effective)? {
        if existing.manifest().version == manifest.manifest().version
            && existing.manifest().package_sha256 == manifest.manifest().package_sha256
        {
            return Ok(path);
        }
    }
    let version = manifest.manifest().version.clone();
    let bytes = fetcher.fetch(
        &manifest.manifest().package_url,
        manifest.manifest().package_size as usize,
    )?;
    let staged = manifest.verify_package(bytes)?.stage(parent)?;
    let temporary = parent.join(format!("highest-{:016x}.new", crate::random_u64()?));
    crate::identity::write_private_new(&temporary, &serde_json::to_vec(&version)?)?;
    std::fs::rename(temporary, floor_path)?;
    Ok(staged)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounds_and_https_authorities_are_enforced() {
        for url in [
            "http://github.com/a",
            "https://github.com@evil.test/a",
            "https://github.com:443/a",
            "https://github.com\\@evil.test/a",
            "https://github.com\n/a",
            "https://github.com/#a",
            "https://--config/x",
            "https:///a",
            "https://github.com /a",
        ] {
            assert!(validate_url(url).is_err(), "{url}");
        }
        validate_url(&format!(
            "{REPOSITORY}/releases/latest/download/manifest.json"
        ))
        .unwrap();
        assert_eq!(read_bounded(&b"123"[..], 3).unwrap(), b"123");
        assert!(read_bounded(&b"1234"[..], 3).is_err());
    }
    #[test]
    fn invalid_signature_never_triggers_a_package_request() {
        struct Forged(std::cell::Cell<usize>);
        impl Fetcher for Forged {
            fn fetch(&self, _: &str, maximum: usize) -> Result<Vec<u8>> {
                self.0.set(self.0.get() + 1);
                assert_eq!(maximum, MAX_METADATA);
                Ok(br#"{"manifest":"{}","signature":"00"}"#.to_vec())
            }
        }
        let fetcher = Forged(std::cell::Cell::new(0));
        let p = Policy {
            installed: "0.9.0-alpha.1",
            highest_staged: None,
            target: "x86_64-apple-darwin",
            os: "10.13.6",
            public_key: &[0; 32],
        };
        assert!(check_and_stage(&fetcher, &p, Path::new("must-not-exist")).is_err());
        assert_eq!(fetcher.0.get(), 1);
        assert!(!Path::new("must-not-exist").exists());
    }
    #[test]
    fn signed_download_is_staged_once_and_persistent_floor_rejects_older_metadata() {
        use ring::signature::KeyPair;
        let (key, mut manifest, bytes) = super::super::tests::fixture();
        let public: [u8; 32] = key.public_key().as_ref().try_into().unwrap();
        let policy = super::super::tests::policy(&public);
        struct Remote {
            metadata: std::cell::RefCell<Vec<u8>>,
            package: std::cell::RefCell<Vec<u8>>,
            calls: std::cell::Cell<usize>,
        }
        impl Fetcher for Remote {
            fn fetch(&self, url: &str, maximum: usize) -> Result<Vec<u8>> {
                self.calls.set(self.calls.get() + 1);
                let bytes = if url.ends_with(".json") {
                    self.metadata.borrow().clone()
                } else {
                    self.package.borrow().clone()
                };
                if bytes.len() > maximum {
                    return fail("fake remote bound");
                }
                Ok(bytes)
            }
        }
        let remote = Remote {
            metadata: std::cell::RefCell::new(super::super::tests::sign(&key, &manifest)),
            package: std::cell::RefCell::new(bytes),
            calls: std::cell::Cell::new(0),
        };
        let root = std::env::temp_dir().join(format!(
            "lrd-download-{:016x}",
            crate::random_u64().unwrap()
        ));
        let first = check_and_stage(&remote, &policy, &root).unwrap();
        assert!(first.join("UPDATE_STAGED").is_file());
        assert_eq!(check_and_stage(&remote, &policy, &root).unwrap(), first);
        assert_eq!(remote.calls.get(), 3); // second check does not download the package
        manifest.version = "0.9.1".into();
        *remote.metadata.borrow_mut() = super::super::tests::sign(&key, &manifest);
        assert!(check_and_stage(&remote, &policy, &root).is_err());
        assert_eq!(remote.calls.get(), 4);
        manifest.version = "2.0.0".into();
        *remote.metadata.borrow_mut() = super::super::tests::sign(&key, &manifest);
        remote.package.borrow_mut()[8] ^= 1;
        assert!(check_and_stage(&remote, &policy, &root).is_err());
        let floor: String = serde_json::from_slice(
            &crate::identity::read_private(&root.join("highest-staged.json"), 1024).unwrap(),
        )
        .unwrap();
        assert_eq!(floor, "1.0.0");
        assert_eq!(
            std::fs::read_dir(&root)
                .unwrap()
                .filter(|e| e.as_ref().unwrap().file_type().unwrap().is_dir())
                .count(),
            1
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
