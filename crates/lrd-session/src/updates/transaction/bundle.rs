//! The live .app is replaced with one filesystem exchange. Journals and the
//! previous sealed bundle stay outside it, on the same filesystem.
use super::*;
#[cfg(test)]
mod tests;
use std::{
    ffi::{CString, OsString},
    os::{fd::AsRawFd, unix::fs::MetadataExt},
    path::PathBuf,
    process::Command,
};
pub(super) struct Bundle {
    parent: Dir,
    name: OsString,
    installation: PathBuf,
    control_path: PathBuf,
}
pub(super) fn control_path(installation: &Path) -> Result<PathBuf> {
    let canonical = installation.canonicalize()?;
    let hash = identity::fingerprint(canonical.as_os_str().as_encoded_bytes());
    Ok(canonical
        .parent()
        .ok_or("missing app parent")?
        .join(format!(".lrd-update-{hash}")))
}
fn owner(dir: &Dir) -> Result<()> {
    let metadata = dir.try_clone()?.into_std_file().metadata()?;
    if metadata.uid() != unsafe { libc::geteuid() } {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "updating an administrator-owned app requires an authorized installer",
        )
        .into());
    }
    Ok(())
}
pub(super) fn open(installation: &Path) -> Result<Transaction> {
    if std::fs::symlink_metadata(installation)?
        .file_type()
        .is_symlink()
    {
        return fail("app cannot be a symlink");
    }
    let installation = installation.canonicalize()?;
    let control_path = control_path(&installation)?;
    let name = installation
        .file_name()
        .ok_or("missing app name")?
        .to_owned();
    let parent = Dir::open_ambient_dir(
        installation.parent().ok_or("missing app parent")?,
        cap_std::ambient_authority(),
    )?;
    let root = parent.open_dir(&name)?;
    owner(&root)?;
    let control_name = control_path
        .file_name()
        .ok_or("missing update state name")?;
    if root.exists(".lrd-update") {
        let metadata = root.symlink_metadata(".lrd-update")?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return fail("legacy update state is not a directory");
        }
        if parent.exists(control_name) {
            return fail("two update journals exist; manual recovery required");
        }
        let legacy = root.open_dir(".lrd-update")?;
        owner(&legacy)?;
        drop(legacy);
        root.rename(".lrd-update", &parent, control_name)?;
        sync(&root)?;
        sync(&parent)?;
    }
    ensure_dirs(&parent, Path::new(control_name))?;
    let control = parent.open_dir(control_name)?;
    owner(&control)?;
    use cap_std::fs::PermissionsExt;
    control.set_permissions(".", cap_std::fs::Permissions::from_mode(0o700))?;
    sync(&parent)?;
    let mut transaction = Transaction::with_dirs(root, control)?;
    transaction.bundle = Some(Bundle {
        parent,
        name,
        installation,
        control_path,
    });
    Ok(transaction)
}
fn verify(path: &Path) -> Result<()> {
    // Intel unsigned bundles remain permitted by the product's distribution
    // policy. A supplied resource seal, including the arm64 seal, must verify.
    if !path.join("Contents/_CodeSignature/CodeResources").exists() {
        return Ok(());
    }
    let result = Command::new("/usr/bin/codesign")
        .args(["--verify", "--deep", "--strict"])
        .arg(path)
        .output()?;
    if !result.status.success() {
        return fail("app code-signing seal rejected");
    }
    Ok(())
}
fn copy_tree(
    source: &Dir,
    destination: &Dir,
    base: &Path,
    depth: u32,
    count: &mut usize,
    total: &mut u64,
) -> Result<()> {
    if depth > 32 {
        return fail("app directory depth limit");
    }
    use cap_std::fs::PermissionsExt;
    let directory = if base.as_os_str().is_empty() {
        Path::new(".")
    } else {
        base
    };
    for item in source.read_dir(directory)? {
        let item = item?;
        *count += 1;
        if *count > 4096 {
            return fail("app entry limit");
        }
        let relative = base.join(item.file_name());
        let metadata = source.symlink_metadata(&relative)?;
        if metadata.file_type().is_symlink() || metadata.permissions().mode() & 0o7000 != 0 {
            return fail("app symlink/special permissions rejected");
        }
        if metadata.is_dir() {
            ensure_dirs(destination, &relative)?;
            copy_tree(source, destination, &relative, depth + 1, count, total)?;
            destination.set_permissions(
                &relative,
                cap_std::fs::Permissions::from_mode(metadata.permissions().mode() & 0o777),
            )?;
        } else if metadata.is_file() {
            *total = total
                .checked_add(metadata.len())
                .ok_or("app size overflow")?;
            if *total > MAX_PACKAGE as u64 {
                return fail("app copy size limit");
            }
            let name = relative.to_str().ok_or("app paths must be UTF-8")?;
            let bytes = read(source, name, MAX_PACKAGE)?;
            atomic_write(destination, name, &bytes, is_executable(&metadata))?;
            destination.set_permissions(
                &relative,
                cap_std::fs::Permissions::from_mode(metadata.permissions().mode() & 0o777),
            )?;
        } else {
            return fail("app special file rejected");
        }
    }
    Ok(())
}
fn all_members(root: &Dir, state: &State, package: &VerifiedPackage, new: bool) -> Result<bool> {
    for (index, entry) in package.manifest().files.iter().enumerate() {
        let expected = if new {
            Some(entry.sha256.clone())
        } else {
            state.previous[index].as_ref().map(|p| p.sha256.clone())
        };
        if digest_if_exists(root, &entry.path)? != expected {
            return Ok(false);
        }
    }
    Ok(true)
}
fn sync_tree(dir: &Dir, depth: u32, count: &mut usize) -> Result<()> {
    if depth > 32 {
        return fail("candidate directory depth limit");
    }
    for item in dir.entries()? {
        let item = item?;
        *count += 1;
        if *count > 4096 {
            return fail("candidate entry limit");
        }
        let kind = item.file_type()?;
        if kind.is_symlink() {
            return fail("candidate symlink rejected");
        }
        if kind.is_dir() {
            sync_tree(&dir.open_dir(item.file_name())?, depth + 1, count)?;
        } else if kind.is_file() {
            dir.open(item.file_name())?.into_std().sync_all()?;
        } else {
            return fail("candidate special file rejected");
        }
    }
    sync(dir)
}
impl Bundle {
    pub(super) fn current(&self) -> Result<Dir> {
        if self
            .parent
            .symlink_metadata(&self.name)?
            .file_type()
            .is_symlink()
        {
            return fail("app became a symlink");
        }
        let root = self.parent.open_dir(&self.name)?;
        owner(&root)?;
        Ok(root)
    }
    pub(super) fn verify_current(&self) -> Result<()> {
        verify(&self.installation)
    }
    fn exchange(&self, work: &Dir) -> Result<()> {
        unsafe extern "C" {
            fn renameatx_np(
                from: i32,
                name: *const libc::c_char,
                to: i32,
                new: *const libc::c_char,
                flags: u32,
            ) -> i32;
        }
        let name = CString::new(self.name.as_encoded_bytes())?;
        let candidate = c"next.app";
        // RENAME_SWAP is available on 10.12+. There is no unsafe per-member
        // fallback if this filesystem does not support an atomic exchange.
        if unsafe {
            renameatx_np(
                self.parent.as_raw_fd(),
                name.as_ptr(),
                work.as_raw_fd(),
                candidate.as_ptr(),
                0x2,
            )
        } != 0
        {
            return Err(std::io::Error::last_os_error().into());
        }
        sync(&self.parent)?;
        sync(work)?;
        Ok(())
    }
    pub(super) fn apply(&self, work: &Dir, state: &State, package: &VerifiedPackage) -> Result<()> {
        let root = self.current()?;
        if !all_members(&root, state, package, false)? {
            return fail("app changed after preparation");
        }
        work.create_dir("next.app")?;
        let next = work.open_dir("next.app")?;
        copy_tree(&root, &next, Path::new(""), 0, &mut 0, &mut 0)?;
        let permissions = root.try_clone()?.into_std_file().metadata()?.permissions();
        let mode = std::os::unix::fs::PermissionsExt::mode(&permissions) & 0o777;
        use cap_std::fs::PermissionsExt;
        next.set_permissions(".", cap_std::fs::Permissions::from_mode(mode))?;
        for (index, entry) in package.manifest().files.iter().enumerate() {
            let name = format!("new/{index}");
            if identity::fingerprint(&read(work, &name, MAX_PACKAGE)?) != entry.sha256 {
                return fail("prepared bundle member changed");
            }
            ensure_dirs(
                &next,
                Path::new(&entry.path).parent().unwrap_or(Path::new("")),
            )?;
            work.rename(name, &next, &entry.path)?;
            sync_parent(&next, &entry.path)?;
        }
        if !all_members(&next, state, package, true)? {
            return fail("candidate bundle incomplete");
        }
        verify(&self.control_path.join(WORK).join("next.app"))?;
        self.verify_current()?;
        if !all_members(&self.current()?, state, package, false)? {
            return fail("app changed before exchange");
        }
        sync_tree(&next, 0, &mut 0)?;
        self.exchange(work)
    }
    pub(super) fn rollback(
        &self,
        work: &Dir,
        state: &State,
        package: &VerifiedPackage,
    ) -> Result<()> {
        let root = self.current()?;
        if all_members(&root, state, package, false)? {
            return Ok(());
        }
        if !all_members(&root, state, package, true)? {
            return fail("app changed independently; manual bundle recovery required");
        }
        let previous = work.open_dir("next.app")?;
        if !all_members(&previous, state, package, false)? {
            return fail("previous bundle changed; recovery rejected");
        }
        verify(&self.control_path.join(WORK).join("next.app"))?;
        self.exchange(work)?;
        self.verify_current()
    }
}
