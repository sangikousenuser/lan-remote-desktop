use crate::{fail, Result};
use rcgen::{CertificateParams, KeyPair};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};

pub fn fingerprint(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub struct Identity {
    pub certificate: CertificateDer<'static>,
    key: Vec<u8>,
    pub directory: PathBuf,
}
#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Peers {
    certificates: Vec<String>,
}

pub fn data_directory() -> Result<PathBuf> {
    #[cfg(windows)]
    let root = PathBuf::from(std::env::var_os("LOCALAPPDATA").ok_or("LOCALAPPDATA is missing")?);
    #[cfg(not(windows))]
    let root = PathBuf::from(std::env::var_os("HOME").ok_or("HOME is missing")?)
        .join("Library/Application Support");
    Ok(root.join("LAN Remote Desktop"))
}

/// Owner-only storage on Unix. Windows inherits the user's LocalAppData ACL.
pub fn private_directory(path: &Path) -> Result<()> {
    if path.exists() && fs::symlink_metadata(path)?.file_type().is_symlink() {
        return fail("data directory cannot be a symlink");
    }
    fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

pub fn write_private_new(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

pub(crate) fn read_private(path: &Path, maximum: u64) -> Result<Vec<u8>> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > maximum {
        return fail("invalid identity file");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return fail("identity file must be owner-only");
        }
    }
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(maximum + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > maximum {
        return fail("identity file exceeded size limit");
    }
    Ok(bytes)
}

impl Identity {
    pub fn load_or_create(directory: &Path) -> Result<Self> {
        private_directory(directory)?;
        let cert_path = directory.join("device.der");
        let key_path = directory.join("device.pk8");
        if !cert_path.exists() && !key_path.exists() {
            let key = KeyPair::generate()?;
            let params = CertificateParams::new(vec!["lanremoted.local".into()])?;
            let cert = params.self_signed(&key)?;
            write_private_new(&key_path, &key.serialize_der())?;
            write_private_new(&cert_path, cert.der().as_ref())?;
        }
        let certificate = CertificateDer::from(read_private(&cert_path, 16_384)?);
        let key = read_private(&key_path, 16_384)?;
        // Validate both files and their key match before making the listener available.
        rustls::sign::CertifiedKey::from_der(
            vec![certificate.clone()],
            PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.clone())),
            &rustls::crypto::ring::default_provider(),
        )?;
        Ok(Self {
            certificate,
            key,
            directory: directory.to_owned(),
        })
    }
    pub fn key(&self) -> PrivateKeyDer<'static> {
        PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(self.key.clone()))
    }
    pub fn fingerprint(&self) -> String {
        fingerprint(self.certificate.as_ref())
    }
    pub fn trusted(&self, certificate: &[u8]) -> Result<bool> {
        let path = self.directory.join("paired.json");
        if !path.exists() {
            return Ok(false);
        }
        let peers: Peers = serde_json::from_slice(&read_private(&path, 64 * 1024)?)?;
        Ok(peers.certificates.contains(&fingerprint(certificate)))
    }
    pub fn trust(&self, certificate: &[u8]) -> Result<()> {
        let path = self.directory.join("paired.json");
        let mut peers: Peers = if path.exists() {
            serde_json::from_slice(&read_private(&path, 64 * 1024)?)?
        } else {
            Peers::default()
        };
        let key = fingerprint(certificate);
        if peers.certificates.contains(&key) {
            return Ok(());
        }
        if peers.certificates.len() >= 32 {
            return fail("paired device limit reached; remove unused devices");
        }
        peers.certificates.push(key);
        let temporary = self
            .directory
            .join(format!("paired.{}.new", crate::random_u64()?));
        write_private_new(&temporary, &serde_json::to_vec(&peers)?)?;
        fs::rename(temporary, path)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn identity_is_stable_and_peer_pins_are_exact() {
        let root = std::env::temp_dir().join(format!("lrd-identity-{}", std::process::id()));
        private_directory(&root).unwrap();
        let first = Identity::load_or_create(&root).unwrap();
        let second = Identity::load_or_create(&root).unwrap();
        assert_eq!(first.fingerprint(), second.fingerprint());
        assert!(!first.trusted(b"unknown").unwrap());
        first.trust(second.certificate.as_ref()).unwrap();
        assert!(first.trusted(second.certificate.as_ref()).unwrap());
        assert!(!first.trusted(b"changed key").unwrap());
        fs::remove_dir_all(root).unwrap();
    }
}
