//! Stable update authentication and private staging. The installer/apply helper
//! must consume only VerifiedPackage; hashes alone never authenticate a release.
use crate::{fail, identity, Result};
use ring::signature::{UnparsedPublicKey, ED25519};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};
pub mod download;
pub mod health;
pub mod service;
pub mod transaction;
pub const MAX_PACKAGE: usize = 512 * 1024 * 1024;
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Envelope {
    pub manifest: String,
    pub signature: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub schema: u32,
    pub version: String,
    pub target: String,
    pub minimum_os: String,
    pub protocol_major: u32,
    pub package_url: String,
    pub package_size: u64,
    pub package_sha256: String,
    pub files: Vec<Entry>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub path: String,
    pub size: u64,
    pub sha256: String,
    pub executable: bool,
}
pub struct Policy<'a> {
    pub installed: &'a str,
    pub highest_staged: Option<&'a str>,
    pub target: &'a str,
    pub os: &'a str,
    pub public_key: &'a [u8; 32],
}
pub struct VerifiedManifest {
    manifest: Manifest,
    envelope: Vec<u8>,
}
pub struct VerifiedPackage {
    metadata: VerifiedManifest,
    bytes: Vec<u8>,
}
fn hex_bytes(value: &str, length: usize) -> Result<Vec<u8>> {
    if value.len() != length * 2 || !value.bytes().all(|b| b.is_ascii_hexdigit()) {
        return fail("invalid update signature/digest encoding");
    }
    (0..length)
        .map(|i| u8::from_str_radix(&value[i * 2..i * 2 + 2], 16).map_err(|e| e.into()))
        .collect()
}
impl VerifiedManifest {
    pub fn verify(bytes: &[u8], policy: &Policy<'_>) -> Result<Self> {
        if bytes.len() > 128 * 1024 {
            return fail("update metadata limit");
        }
        let envelope: Envelope = serde_json::from_slice(bytes)?;
        if envelope.manifest.len() > 65536 {
            return fail("signed manifest limit");
        }
        UnparsedPublicKey::new(&ED25519, policy.public_key)
            .verify(
                envelope.manifest.as_bytes(),
                &hex_bytes(&envelope.signature, 64)?,
            )
            .map_err(|_| std::io::Error::other("update signature rejected"))?;
        let manifest: Manifest = serde_json::from_str(&envelope.manifest)?;
        let version = semver::Version::parse(&manifest.version)?;
        if !version.pre.is_empty()
            || !version.build.is_empty()
            || version <= semver::Version::parse(policy.installed)?
            || policy
                .highest_staged
                .is_some_and(|s| semver::Version::parse(s).map_or(true, |floor| version < floor))
        {
            return fail("update version/channel/rollback rejected");
        }
        if manifest.schema != 1
            || manifest.protocol_major != 1
            || manifest.target != policy.target
            || semver::Version::parse(&manifest.minimum_os)? > semver::Version::parse(policy.os)?
        {
            return fail("incompatible update target/OS/protocol");
        }
        download::validate_url(&manifest.package_url)?;
        if manifest.files.is_empty()
            || manifest.files.len() > 256
            || manifest.package_size > MAX_PACKAGE as u64
        {
            return fail("update file/size limit");
        }
        let mut paths = HashSet::new();
        let mut total = 8u64;
        for file in &manifest.files {
            crate::transfer::validate_path(&file.path)?;
            if file.path.len() > 2048
                || file
                    .path
                    .split('/')
                    .any(|part| part.eq_ignore_ascii_case(".lrd-update"))
                || !paths.insert(file.path.to_lowercase())
            {
                return fail("duplicate/oversized update path");
            }
            hex_bytes(&file.sha256, 32)?;
            total = total.checked_add(file.size).ok_or("update size overflow")?;
        }
        for path in &paths {
            let mut parent = path.as_str();
            while let Some((head, _)) = parent.rsplit_once('/') {
                if paths.contains(head) {
                    return fail("update file/directory path conflict");
                }
                parent = head;
            }
        }
        hex_bytes(&manifest.package_sha256, 32)?;
        if total != manifest.package_size {
            return fail("update package size mismatch");
        }
        Ok(Self {
            manifest,
            envelope: bytes.to_vec(),
        })
    }
    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }
    pub fn verify_package(self, bytes: Vec<u8>) -> Result<VerifiedPackage> {
        if bytes.len() > MAX_PACKAGE
            || bytes.len() as u64 != self.manifest.package_size
            || !bytes.starts_with(b"LRDUPD1\n")
            || identity::fingerprint(&bytes) != self.manifest.package_sha256
        {
            return fail("update package integrity rejected");
        }
        let mut offset = 8;
        for file in &self.manifest.files {
            let end = offset + file.size as usize;
            if identity::fingerprint(&bytes[offset..end]) != file.sha256 {
                return fail("update member integrity rejected");
            }
            offset = end;
        }
        Ok(VerifiedPackage {
            metadata: self,
            bytes,
        })
    }
}
impl VerifiedPackage {
    pub fn manifest(&self) -> &Manifest {
        &self.metadata.manifest
    }
    /// Store the exact authenticated package, without modifying a running app.
    pub fn stage(self, parent: &Path) -> Result<PathBuf> {
        identity::private_directory(parent)?;
        let root = parent.join(format!(
            "staged-{}-{:016x}",
            self.manifest().version,
            crate::random_u64()?
        ));
        std::fs::create_dir(&root)?;
        identity::private_directory(&root)?;
        identity::write_private_new(&root.join("manifest.json"), &self.metadata.envelope)?;
        identity::write_private_new(&root.join("package.lrdpkg"), &self.bytes)?;
        // The ready marker is last; interrupted staging is never an apply candidate.
        identity::write_private_new(&root.join("UPDATE_STAGED"), b"1\n")?;
        Ok(root)
    }
}
/// Re-authenticate persisted stages; a ready filename is never sufficient.
pub fn newest_staged(
    parent: &Path,
    policy: &Policy<'_>,
) -> Result<Option<(PathBuf, VerifiedPackage)>> {
    if !parent.exists() {
        return Ok(None);
    }
    identity::private_directory(parent)?;
    let mut best: Option<(PathBuf, VerifiedPackage)> = None;
    for entry in std::fs::read_dir(parent)?.take(32) {
        let entry = entry?;
        if !entry.file_name().to_string_lossy().starts_with("staged-")
            || !entry.file_type()?.is_dir()
            || entry.file_type()?.is_symlink()
        {
            continue;
        }
        let root = entry.path();
        let attempt = (|| -> Result<VerifiedPackage> {
            if identity::read_private(&root.join("UPDATE_STAGED"), 8)? != b"1\n" {
                return fail("invalid ready marker");
            }
            VerifiedManifest::verify(
                &identity::read_private(&root.join("manifest.json"), 128 * 1024)?,
                policy,
            )?
            .verify_package(identity::read_private(
                &root.join("package.lrdpkg"),
                MAX_PACKAGE as u64,
            )?)
        })();
        if let Ok(package) = attempt {
            if best.as_ref().is_none_or(|(_, old)| {
                semver::Version::parse(&package.manifest().version).unwrap()
                    > semver::Version::parse(&old.manifest().version).unwrap()
            }) {
                best = Some((root, package));
            }
        }
    }
    Ok(best)
}
#[cfg(test)]
mod tests {
    use super::*;
    use ring::{
        rand::SystemRandom,
        signature::{Ed25519KeyPair, KeyPair},
    };
    pub(super) fn fixture() -> (Ed25519KeyPair, Manifest, Vec<u8>) {
        let key = Ed25519KeyPair::from_pkcs8(
            Ed25519KeyPair::generate_pkcs8(&SystemRandom::new())
                .unwrap()
                .as_ref(),
        )
        .unwrap();
        let bytes = b"LRDUPD1\nexecutable".to_vec();
        let m = Manifest {
            schema: 1,
            version: "1.0.0".into(),
            target: "x86_64-apple-darwin".into(),
            minimum_os: "10.13.0".into(),
            protocol_major: 1,
            package_url: "https://example.invalid/releases/client.lrdpkg".into(),
            package_size: bytes.len() as u64,
            package_sha256: identity::fingerprint(&bytes),
            files: vec![Entry {
                path: "Contents/MacOS/lrd-macos-client".into(),
                size: 10,
                sha256: identity::fingerprint(b"executable"),
                executable: true,
            }],
        };
        (key, m, bytes)
    }
    pub(super) fn sign(key: &Ed25519KeyPair, m: &Manifest) -> Vec<u8> {
        let manifest = serde_json::to_string(m).unwrap();
        let signature = identity::hex(key.sign(manifest.as_bytes()).as_ref());
        serde_json::to_vec(&Envelope {
            manifest,
            signature,
        })
        .unwrap()
    }
    pub(super) fn policy(key: &[u8; 32]) -> Policy<'_> {
        Policy {
            installed: "0.9.0-alpha.1",
            highest_staged: None,
            target: "x86_64-apple-darwin",
            os: "10.13.6",
            public_key: key,
        }
    }
    #[test]
    fn signature_arch_version_os_and_bytes_are_independently_checked() {
        let (key, mut m, bytes) = fixture();
        let public: [u8; 32] = key.public_key().as_ref().try_into().unwrap();
        let p = policy(&public);
        let signed = sign(&key, &m);
        let verified = VerifiedManifest::verify(&signed, &p).unwrap();
        assert!(verified.verify_package(bytes.clone()).is_ok());
        let mut forged = signed.clone();
        let pos = forged.iter().position(|b| *b == b'1').unwrap();
        forged[pos] = b'2';
        assert!(VerifiedManifest::verify(&forged, &p).is_err());
        let mut wrong = bytes.clone();
        wrong[8] ^= 1;
        assert!(VerifiedManifest::verify(&signed, &p)
            .unwrap()
            .verify_package(wrong)
            .is_err());
        for n in 0..bytes.len() {
            assert!(VerifiedManifest::verify(&signed, &p)
                .unwrap()
                .verify_package(bytes[..n].to_vec())
                .is_err());
        }
        m.target = "aarch64-apple-darwin".into();
        assert!(VerifiedManifest::verify(&sign(&key, &m), &p).is_err());
        m.target = p.target.into();
        m.minimum_os = "11.0.0".into();
        assert!(VerifiedManifest::verify(&sign(&key, &m), &p).is_err());
        m.minimum_os = "10.13.0".into();
        m.version = "0.8.0".into();
        assert!(VerifiedManifest::verify(&sign(&key, &m), &p).is_err());
        m.version = "2.0.0-beta.1".into();
        assert!(VerifiedManifest::verify(&sign(&key, &m), &p).is_err());
    }
    #[test]
    fn signed_unsafe_paths_and_wrong_keys_are_rejected() {
        let (key, mut m, _) = fixture();
        let public: [u8; 32] = key.public_key().as_ref().try_into().unwrap();
        m.files[0].path = "../escape".into();
        assert!(VerifiedManifest::verify(&sign(&key, &m), &policy(&public)).is_err());
        assert!(VerifiedManifest::verify(&sign(&key, &m), &policy(&[0; 32])).is_err());
    }
    #[test]
    fn staging_is_private_and_ready_is_written_last() {
        let (key, m, bytes) = fixture();
        let public: [u8; 32] = key.public_key().as_ref().try_into().unwrap();
        let parent =
            std::env::temp_dir().join(format!("lrd-updates-{}", crate::random_u64().unwrap()));
        let dir = VerifiedManifest::verify(&sign(&key, &m), &policy(&public))
            .unwrap()
            .verify_package(bytes.clone())
            .unwrap()
            .stage(&parent)
            .unwrap();
        assert!(dir.join("UPDATE_STAGED").is_file());
        assert_eq!(std::fs::read(dir.join("package.lrdpkg")).unwrap(), bytes);
        std::fs::remove_dir_all(parent).unwrap();
    }
}
