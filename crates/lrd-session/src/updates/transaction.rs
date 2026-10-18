//! Restartable replacement engine. The caller must stop the old app first and
//! run this with the installation owner's privileges. Only authenticated package
//! members are touched; device keys, settings and received files live elsewhere.
use super::{Policy, VerifiedManifest, VerifiedPackage, MAX_PACKAGE};
use crate::{fail, identity, Result};
use cap_std::fs::{Dir, OpenOptions};
use serde::{Deserialize, Serialize};
use std::{
    fs::File,
    io::{Read, Write},
    path::Path,
};

const WORK: &str = "transaction";
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Phase {
    Prepared,
    Applying,
    AwaitingHealth,
    Healthy,
    RolledBack,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Previous {
    sha256: String,
    size: u64,
    executable: bool,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct State {
    schema: u32,
    id: u64,
    version: String,
    phase: Phase,
    previous: Vec<Option<Previous>>,
}
#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct History {
    highest: Option<String>,
    failures: std::collections::BTreeMap<String, u32>,
    last_failed_transaction: Option<u64>,
}

pub struct Transaction {
    root: Dir,
    control: Dir,
    // OS lock is released even on crash; no stale PID/lock-file deletion race.
    _lock: File,
}
impl Transaction {
    pub fn open(installation: &Path) -> Result<Self> {
        if std::fs::symlink_metadata(installation)?
            .file_type()
            .is_symlink()
        {
            return fail("installation cannot be a symlink");
        }
        let root = Dir::open_ambient_dir(installation, cap_std::ambient_authority())?;
        ensure_dirs(&root, Path::new(".lrd-update"))?;
        let control = root.open_dir(".lrd-update")?;
        #[cfg(unix)]
        {
            use cap_std::fs::PermissionsExt;
            root.set_permissions(".lrd-update", cap_std::fs::Permissions::from_mode(0o700))?;
        }
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true);
        reject_special(&control, "lock")?;
        let lock = control.open_with("lock", &options)?.into_std();
        lock.try_lock()?;
        Ok(Self {
            root,
            control,
            _lock: lock,
        })
    }
    /// Copy all old members and authenticate all new bytes before replacement.
    /// A failed preparation leaves the installed application untouched.
    pub fn prepare(&self, package: &VerifiedPackage) -> Result<()> {
        let mut history = self.history()?;
        let version = &package.manifest().version;
        if history.failures.get(version).copied().unwrap_or(0) >= 2 {
            return fail("update blocked after repeated startup failures");
        }
        if let Some(floor) = &history.highest {
            if semver::Version::parse(version)? < semver::Version::parse(floor)? {
                return fail("update below persistent rollback floor");
            }
        }
        if self.control.exists(WORK) {
            let work = self.control.open_dir(WORK)?;
            // No state means preparation was interrupted before any app change.
            if work.exists("state.json") {
                let state: State = serde_json::from_slice(&read(&work, "state.json", 128 * 1024)?)?;
                if !matches!(state.phase, Phase::Healthy | Phase::RolledBack) {
                    return fail("recover the pending update before preparing another");
                }
            }
            drop(work);
            self.control.remove_dir_all(WORK)?;
        }
        self.control.create_dir(WORK)?;
        let work = self.control.open_dir(WORK)?;
        work.create_dir("backup")?;
        work.create_dir("new")?;
        atomic_write(&work, "manifest.json", &package.metadata.envelope, false)?;
        atomic_write(&work, "package.lrdpkg", &package.bytes, false)?;
        let mut previous = Vec::new();
        let mut old_total = 0u64;
        let mut offset = 8;
        for (i, entry) in package.manifest().files.iter().enumerate() {
            safe_parent(&self.root, &entry.path)?;
            let old = match self.root.symlink_metadata(&entry.path) {
                Ok(m) if m.is_file() && !m.file_type().is_symlink() => {
                    old_total = old_total
                        .checked_add(m.len())
                        .ok_or("backup size overflow")?;
                    if old_total > MAX_PACKAGE as u64 {
                        return fail("update backup size limit");
                    }
                    let bytes = read(&self.root, &entry.path, MAX_PACKAGE)?;
                    let executable = is_executable(&m);
                    atomic_write(&work, &format!("backup/{i}"), &bytes, executable)?;
                    Some(Previous {
                        sha256: identity::fingerprint(&bytes),
                        size: bytes.len() as u64,
                        executable,
                    })
                }
                Ok(_) => return fail("update destination is not a regular file"),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
                Err(e) => return Err(e.into()),
            };
            previous.push(old);
            let end = offset + entry.size as usize;
            atomic_write(
                &work,
                &format!("new/{i}"),
                &package.bytes[offset..end],
                entry.executable,
            )?;
            offset = end;
        }
        let state = State {
            schema: 1,
            id: crate::random_u64()?,
            version: version.clone(),
            phase: Phase::Prepared,
            previous,
        };
        save(&work, &state)?;
        history.highest = Some(version.clone());
        atomic_write(
            &self.control,
            "history.json",
            &serde_json::to_vec(&history)?,
            false,
        )?;
        Ok(())
    }
    /// The helper re-verifies the persisted signature and package, even if the
    /// downloading process already verified them before requesting elevation.
    fn load(&self, policy: &Policy<'_>) -> Result<(Dir, State, VerifiedPackage)> {
        let work = self.control.open_dir(WORK)?;
        let bytes = read(&work, "manifest.json", 128 * 1024)?;
        // Recovery also runs from the new binary, whose installed version equals
        // the pending one. Version advancement is checked in prepare/history.
        let authentication = Policy {
            installed: "0.0.0",
            highest_staged: None,
            ..*policy
        };
        let manifest = VerifiedManifest::verify(&bytes, &authentication)?;
        let package = manifest.verify_package(read(&work, "package.lrdpkg", MAX_PACKAGE)?)?;
        let state: State = serde_json::from_slice(&read(&work, "state.json", 128 * 1024)?)?;
        if state.schema != 1
            || state.version != package.manifest().version
            || state.previous.len() != package.manifest().files.len()
        {
            return fail("invalid update transaction");
        }
        Ok((work, state, package))
    }
    pub fn apply(&self, policy: &Policy<'_>) -> Result<()> {
        let (work, mut state, package) = self.load(policy)?;
        if state.phase != Phase::Prepared {
            return fail("update is not prepared");
        }
        state.phase = Phase::Applying;
        save(&work, &state)?;
        let result = (|| -> Result<()> {
            for (i, entry) in package.manifest().files.iter().enumerate() {
                safe_parent(&self.root, &entry.path)?;
                let current = digest_if_exists(&self.root, &entry.path)?;
                if current != state.previous[i].as_ref().map(|p| p.sha256.clone()) {
                    return fail("installed application changed after update preparation");
                }
                let new = format!("new/{i}");
                if identity::fingerprint(&read(&work, &new, MAX_PACKAGE)?) != entry.sha256 {
                    return fail("prepared member changed");
                }
                ensure_dirs(
                    &self.root,
                    Path::new(&entry.path).parent().unwrap_or(Path::new("")),
                )?;
                work.rename(new, &self.root, &entry.path)?;
                sync_parent(&self.root, &entry.path)?;
            }
            state.phase = Phase::AwaitingHealth;
            save(&work, &state)
        })();
        if result.is_err() {
            self.rollback(policy)?;
        }
        result
    }
    /// Called only after the helper observes successful startup, not merely
    /// after CreateProcess/exec returns. Backups remain available for recovery.
    pub fn confirm_health(&self, policy: &Policy<'_>) -> Result<()> {
        let (work, mut state, package) = self.load(policy)?;
        if state.phase != Phase::AwaitingHealth {
            return fail("update has no pending health check");
        }
        for entry in &package.manifest().files {
            if digest_if_exists(&self.root, &entry.path)? != Some(entry.sha256.clone()) {
                return fail("updated application changed before health confirmation");
            }
        }
        state.phase = Phase::Healthy;
        save(&work, &state)
    }
    pub fn recover(&self, policy: &Policy<'_>) -> Result<bool> {
        if !self.recovery_needed(policy)? {
            return Ok(false);
        }
        self.rollback(policy)?;
        Ok(true)
    }
    pub fn recovery_needed(&self, policy: &Policy<'_>) -> Result<bool> {
        if !self.control.exists(WORK) {
            return Ok(false);
        }
        let work = self.control.open_dir(WORK)?;
        if !work.exists("state.json") {
            return Ok(false);
        }
        drop(work);
        let (_, state, _) = self.load(policy)?;
        if matches!(state.phase, Phase::Healthy | Phase::RolledBack) {
            return Ok(false);
        }
        Ok(true)
    }
    pub fn rollback(&self, policy: &Policy<'_>) -> Result<()> {
        let (work, mut state, package) = self.load(policy)?;
        if state.phase == Phase::RolledBack {
            return Ok(());
        }
        if state.phase == Phase::Healthy {
            return fail("healthy update cannot be automatically reverted");
        }
        // Validate the entire recovery plan before writing any old member.
        for (i, entry) in package.manifest().files.iter().enumerate() {
            safe_parent(&self.root, &entry.path)?;
            if let Some(old) = &state.previous[i] {
                let bytes = read(&work, &format!("backup/{i}"), MAX_PACKAGE)?;
                if bytes.len() as u64 != old.size || identity::fingerprint(&bytes) != old.sha256 {
                    return fail("backup integrity rejected; manual recovery needed");
                }
            }
            let current = digest_if_exists(&self.root, &entry.path)?;
            if current != Some(entry.sha256.clone())
                && current != state.previous[i].as_ref().map(|p| p.sha256.clone())
            {
                return fail("application changed independently; manual recovery needed");
            }
        }
        for (i, entry) in package.manifest().files.iter().enumerate() {
            if let Some(old) = &state.previous[i] {
                let bytes = read(&work, &format!("backup/{i}"), MAX_PACKAGE)?;
                atomic_write(&self.root, &entry.path, &bytes, old.executable)?;
            } else if self.root.exists(&entry.path) {
                self.root.remove_file(&entry.path)?;
            }
        }
        // Count once. Re-running an interrupted rollback does not unblock a bad
        // version or overwrite the old executable with the failed one.
        if state.phase == Phase::AwaitingHealth || state.phase == Phase::Applying {
            let mut history = self.history()?;
            if history.last_failed_transaction != Some(state.id) {
                let count = history.failures.entry(state.version.clone()).or_default();
                *count = count.saturating_add(1);
                history.last_failed_transaction = Some(state.id);
            }
            if history.failures.len() > 32 {
                return fail("update failure history limit");
            }
            atomic_write(
                &self.control,
                "history.json",
                &serde_json::to_vec(&history)?,
                false,
            )?;
        }
        state.phase = Phase::RolledBack;
        save(&work, &state)
    }
    fn history(&self) -> Result<History> {
        if !self.control.exists("history.json") {
            return Ok(History::default());
        }
        Ok(serde_json::from_slice(&read(
            &self.control,
            "history.json",
            16 * 1024,
        )?)?)
    }
}
fn save(work: &Dir, state: &State) -> Result<()> {
    atomic_write(work, "state.json", &serde_json::to_vec(state)?, false)
}
fn digest_if_exists(dir: &Dir, path: &str) -> Result<Option<String>> {
    reject_special(dir, path)?;
    if !dir.exists(path) {
        return Ok(None);
    }
    Ok(Some(identity::fingerprint(&read(dir, path, MAX_PACKAGE)?)))
}
fn reject_special(dir: &Dir, path: impl AsRef<Path>) -> Result<()> {
    match dir.symlink_metadata(path) {
        Ok(m) if !m.is_file() || m.file_type().is_symlink() => {
            fail("update symlink/special file rejected")
        }
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}
fn safe_parent(dir: &Dir, path: &str) -> Result<()> {
    let mut parent = std::path::PathBuf::new();
    let mut parts = Path::new(path).components().peekable();
    while let Some(part) = parts.next() {
        if parts.peek().is_none() {
            break;
        }
        parent.push(part);
        match dir.symlink_metadata(&parent) {
            Ok(m) if !m.is_dir() || m.file_type().is_symlink() => {
                return fail("update parent symlink rejected")
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => break,
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}
fn ensure_dirs(dir: &Dir, path: &Path) -> Result<()> {
    let mut parent = std::path::PathBuf::new();
    for part in path.components() {
        parent.push(part);
        match dir.symlink_metadata(&parent) {
            Ok(m) if m.is_dir() && !m.file_type().is_symlink() => {}
            Ok(_) => return fail("update directory symlink rejected"),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => dir.create_dir(&parent)?,
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}
fn read(dir: &Dir, path: &str, maximum: usize) -> Result<Vec<u8>> {
    safe_parent(dir, path)?;
    reject_special(dir, path)?;
    let mut bytes = Vec::new();
    dir.open(path)?
        .take(maximum as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > maximum {
        return fail("update local file size limit");
    }
    Ok(bytes)
}
fn atomic_write(dir: &Dir, path: &str, bytes: &[u8], executable: bool) -> Result<()> {
    safe_parent(dir, path)?;
    reject_special(dir, path)?;
    let temporary = format!("{path}.{:016x}.new", crate::random_u64()?);
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    let mut file = dir.open_with(&temporary, &options)?;
    #[cfg(unix)]
    {
        use cap_std::fs::PermissionsExt;
        file.set_permissions(cap_std::fs::Permissions::from_mode(if executable {
            0o755
        } else {
            0o644
        }))?;
    }
    #[cfg(not(unix))]
    let _ = executable;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    dir.rename(&temporary, dir, path)?;
    sync_parent(dir, path)
}
fn is_executable(metadata: &cap_std::fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use cap_std::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        false
    }
}
fn sync(dir: &Dir) -> Result<()> {
    // Unix fsync of directory metadata makes renamed journals durable. Windows
    // file sync_all is used; a directory handle cannot be FlushFileBuffers'd.
    #[cfg(unix)]
    dir.open(".")?.into_std().sync_all()?;
    #[cfg(not(unix))]
    let _ = dir;
    Ok(())
}
fn sync_parent(dir: &Dir, path: &str) -> Result<()> {
    match Path::new(path)
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
    {
        Some(parent) => sync(&dir.open_dir(parent)?),
        None => sync(dir),
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{fixture, policy, sign};
    use super::*;
    use ring::signature::KeyPair;
    use std::path::PathBuf;
    struct Root(PathBuf);
    impl Root {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "lrd-transaction-{:016x}",
                crate::random_u64().unwrap()
            ));
            std::fs::create_dir_all(root.join("Contents/MacOS")).unwrap();
            std::fs::write(root.join("Contents/MacOS/lrd-macos-client"), b"old binary").unwrap();
            std::fs::write(root.join("unrelated-settings.json"), b"keep me").unwrap();
            Self(root)
        }
    }
    impl Drop for Root {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }
    fn verified(
        key: &ring::signature::Ed25519KeyPair,
        manifest: &super::super::Manifest,
        bytes: Vec<u8>,
    ) -> VerifiedPackage {
        let public: [u8; 32] = key.public_key().as_ref().try_into().unwrap();
        VerifiedManifest::verify(&sign(key, manifest), &policy(&public))
            .unwrap()
            .verify_package(bytes)
            .unwrap()
    }
    #[test]
    fn healthy_update_keeps_backup_and_unrelated_data() {
        let root = Root::new();
        let (key, m, bytes) = fixture();
        let public: [u8; 32] = key.public_key().as_ref().try_into().unwrap();
        let p = policy(&public);
        let tx = Transaction::open(&root.0).unwrap();
        assert!(Transaction::open(&root.0).is_err());
        tx.prepare(&verified(&key, &m, bytes)).unwrap();
        assert!(tx.confirm_health(&p).is_err());
        tx.apply(&p).unwrap();
        assert_eq!(
            std::fs::read(root.0.join(&m.files[0].path)).unwrap(),
            b"executable"
        );
        tx.confirm_health(&p).unwrap();
        assert!(!tx.recover(&p).unwrap());
        assert!(tx.rollback(&p).is_err());
        assert_eq!(
            std::fs::read(root.0.join("unrelated-settings.json")).unwrap(),
            b"keep me"
        );
        assert_eq!(
            read(&tx.control.open_dir(WORK).unwrap(), "backup/0", 128).unwrap(),
            b"old binary"
        );
    }
    #[test]
    fn failed_startup_restores_old_files_and_blocks_repeated_bad_version() {
        let root = Root::new();
        let (key, mut m, mut bytes) = fixture();
        let public: [u8; 32] = key.public_key().as_ref().try_into().unwrap();
        let p = policy(&public);
        bytes.extend(b"new resource");
        m.files.push(super::super::Entry {
            path: "Contents/Resources/new.txt".into(),
            size: 12,
            sha256: identity::fingerprint(b"new resource"),
            executable: false,
        });
        m.package_size = bytes.len() as u64;
        m.package_sha256 = identity::fingerprint(&bytes);
        let package = verified(&key, &m, bytes);
        for _ in 0..2 {
            let tx = Transaction::open(&root.0).unwrap();
            tx.prepare(&package).unwrap();
            tx.apply(&p).unwrap();
            drop(tx); // Simulate helper/app dying before health confirmation.
            let tx = Transaction::open(&root.0).unwrap();
            assert!(tx.recover(&p).unwrap());
            assert!(!tx.recover(&p).unwrap());
            assert_eq!(
                std::fs::read(root.0.join(&m.files[0].path)).unwrap(),
                b"old binary"
            );
            assert!(!root.0.join("Contents/Resources/new.txt").exists());
        }
        let tx = Transaction::open(&root.0).unwrap();
        assert!(tx.prepare(&package).is_err());
        assert_eq!(tx.history().unwrap().failures["1.0.0"], 2);
    }
    #[test]
    fn interrupted_member_replacement_is_recovered_after_reopening() {
        let root = Root::new();
        let (key, m, bytes) = fixture();
        let public: [u8; 32] = key.public_key().as_ref().try_into().unwrap();
        let p = policy(&public);
        let tx = Transaction::open(&root.0).unwrap();
        tx.prepare(&verified(&key, &m, bytes)).unwrap();
        let (work, mut state, _) = tx.load(&p).unwrap();
        state.phase = Phase::Applying;
        save(&work, &state).unwrap();
        work.rename("new/0", &tx.root, &m.files[0].path).unwrap();
        drop(work);
        drop(tx);
        let tx = Transaction::open(&root.0).unwrap();
        assert!(tx.recover(&p).unwrap());
        assert_eq!(
            std::fs::read(root.0.join(&m.files[0].path)).unwrap(),
            b"old binary"
        );
    }
    #[test]
    fn corrupt_backup_or_signature_is_never_applied_or_restored() {
        let root = Root::new();
        let (key, m, bytes) = fixture();
        let public: [u8; 32] = key.public_key().as_ref().try_into().unwrap();
        let p = policy(&public);
        let tx = Transaction::open(&root.0).unwrap();
        tx.prepare(&verified(&key, &m, bytes)).unwrap();
        tx.apply(&p).unwrap();
        std::fs::write(root.0.join(".lrd-update/transaction/backup/0"), b"corrupt").unwrap();
        assert!(tx.rollback(&p).is_err());
        assert_eq!(
            std::fs::read(root.0.join(&m.files[0].path)).unwrap(),
            b"executable"
        );
        std::fs::write(root.0.join(".lrd-update/transaction/manifest.json"), b"{}").unwrap();
        assert!(tx.confirm_health(&p).is_err());
        assert!(tx.recover(&p).is_err());
    }
    #[test]
    fn rollback_floor_persists_across_process_lifetimes() {
        let root = Root::new();
        let (key, mut m, bytes) = fixture();
        let public: [u8; 32] = key.public_key().as_ref().try_into().unwrap();
        let p = policy(&public);
        let tx = Transaction::open(&root.0).unwrap();
        m.version = "2.0.0".into();
        tx.prepare(&verified(&key, &m, bytes.clone())).unwrap();
        tx.apply(&p).unwrap();
        tx.rollback(&p).unwrap();
        drop(tx);
        m.version = "1.0.0".into();
        let tx = Transaction::open(&root.0).unwrap();
        assert!(tx.prepare(&verified(&key, &m, bytes)).is_err());
    }
    #[cfg(unix)]
    #[test]
    fn symlinked_destination_parent_is_rejected_before_touching_outside() {
        let root = Root::new();
        let other = Root::new();
        std::fs::remove_dir_all(root.0.join("Contents")).unwrap();
        std::os::unix::fs::symlink(other.0.join("Contents"), root.0.join("Contents")).unwrap();
        let (key, m, bytes) = fixture();
        let tx = Transaction::open(&root.0).unwrap();
        assert!(tx.prepare(&verified(&key, &m, bytes)).is_err());
        assert_eq!(
            std::fs::read(other.0.join(&m.files[0].path)).unwrap(),
            b"old binary"
        );
    }
}
