//! App startup bridge. Updates are staged while running, then applied by the
//! separate helper before the next session begins. No session is terminated.
use super::{download, health, newest_staged, transaction::Transaction, Policy};
use crate::{fail, identity, Result};
use std::{
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};

pub struct Platform {
    pub target: &'static str,
    pub os: String,
    pub key: [u8; 32],
}
impl Platform {
    pub fn current() -> Result<Self> {
        let key: [u8; 32] = super::hex_bytes(
            include_str!("../../../../packaging/update-public-key.hex").trim(),
            32,
        )?
        .try_into()
        .map_err(|_| std::io::Error::other("invalid embedded release key"))?;
        #[cfg(target_os = "macos")]
        {
            let output = Command::new("/usr/bin/sw_vers")
                .arg("-productVersion")
                .output()?;
            if !output.status.success() {
                return fail("OS version unavailable");
            }
            let mut os = String::from_utf8(output.stdout)?.trim().to_string();
            if os.matches('.').count() == 1 {
                os.push_str(".0");
            }
            semver::Version::parse(&os)?;
            let target = if cfg!(target_arch = "aarch64") {
                "aarch64-apple-darwin"
            } else {
                "x86_64-apple-darwin"
            };
            Ok(Self { target, os, key })
        }
        #[cfg(windows)]
        {
            #[repr(C)]
            struct Version {
                size: u32,
                major: u32,
                minor: u32,
                build: u32,
                platform: u32,
                description: [u16; 128],
            }
            #[link(name = "ntdll")]
            unsafe extern "system" {
                fn RtlGetVersion(version: *mut Version) -> i32;
            }
            let mut version = Version {
                size: std::mem::size_of::<Version>() as u32,
                major: 0,
                minor: 0,
                build: 0,
                platform: 0,
                description: [0; 128],
            };
            if unsafe { RtlGetVersion(&mut version) } != 0 {
                return fail("OS version unavailable");
            }
            Ok(Self {
                target: "x86_64-pc-windows-msvc",
                os: format!("{}.{}.{}", version.major, version.minor, version.build),
                key,
            })
        }
        #[cfg(not(any(windows, target_os = "macos")))]
        {
            let _ = key;
            fail("updates require Windows or macOS")
        }
    }
    pub fn policy<'a>(&'a self, installed: &'a str) -> Policy<'a> {
        Policy {
            installed,
            highest_staged: None,
            target: self.target,
            os: &self.os,
            public_key: &self.key,
        }
    }
}
pub fn stage_directory() -> Result<PathBuf> {
    Ok(identity::data_directory()?.join("updates"))
}

pub fn installation(executable: &Path, helper: bool) -> Result<PathBuf> {
    #[cfg(windows)]
    {
        let _ = helper;
        Ok(executable
            .parent()
            .ok_or("missing executable directory")?
            .into())
    }
    #[cfg(target_os = "macos")]
    {
        let folder = executable.parent().ok_or("missing executable directory")?;
        if folder.file_name().and_then(|v| v.to_str())
            != Some(if helper { "Helpers" } else { "MacOS" })
        {
            return fail("updates require a packaged .app");
        }
        let contents = folder.parent().ok_or("missing Contents")?;
        if contents.file_name().and_then(|v| v.to_str()) != Some("Contents") {
            return fail("invalid app bundle");
        }
        Ok(contents.parent().ok_or("missing app bundle")?.into())
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = (executable, helper);
        fail("unsupported installation")
    }
}
pub fn helper_path(root: &Path) -> PathBuf {
    if cfg!(windows) {
        let versioned = root.join(format!(
            "lrd-update-helper-{}.exe",
            env!("CARGO_PKG_VERSION")
        ));
        if versioned.is_file() {
            return versioned;
        }
        root.join("lrd-update-helper.exe")
    } else {
        root.join("Contents/Helpers/lrd-update-helper")
    }
}
pub fn app_path(root: &Path) -> PathBuf {
    if cfg!(windows) {
        root.join("lrd-windows-host.exe")
    } else {
        root.join("Contents/MacOS/lrd-macos-client")
    }
}
fn child(executable: &Path) -> Command {
    let mut command = Command::new(executable);
    command
        .env_remove("LRD_UPDATE_HEALTH_ADDRESS")
        .env_remove("LRD_UPDATE_HEALTH_TOKEN")
        .env_remove("LRD_UPDATE_HEALTH_VERSION");
    command
}
/// True means the helper owns restart. Diagnostic tools and LaunchAgent must
/// skip this entry point; the GUI/Host calls it before initializing resources.
pub fn bootstrap(installed: &str) -> Result<bool> {
    let executable = std::env::current_exe()?;
    let Ok(root) = installation(&executable, false) else {
        return Ok(false);
    };
    let helper = helper_path(&root);
    if !helper.is_file() {
        return Ok(false);
    }
    static LIFETIME: std::sync::OnceLock<std::fs::File> = std::sync::OnceLock::new();
    if LIFETIME.get().is_none() {
        let Some(lease) = runtime_lease(&root)? else {
            return Ok(true);
        };
        let _ = LIFETIME.set(lease);
    }
    if std::env::var_os("LRD_UPDATE_HEALTH_ADDRESS").is_some()
        || std::env::var_os("LRD_UPDATE_SKIP_ONCE").is_some()
    {
        return Ok(false);
    }
    let platform = Platform::current()?;
    let pending = root.join(".lrd-update/transaction/state.json").exists();
    if !pending && newest_staged(&stage_directory()?, &platform.policy(installed))?.is_none() {
        return Ok(false);
    }
    // Healthy history also has a state file, so the helper checks its phase.
    // Do not hand off every launch when no recovery or staged update is needed.
    if pending && newest_staged(&stage_directory()?, &platform.policy(installed))?.is_none() {
        match Transaction::open(&root)
            .and_then(|tx| tx.recovery_needed(&platform.policy(installed)))
        {
            Ok(false) => return Ok(false),
            Ok(true) => {}
            Err(_) => return Ok(false),
        }
    }
    let mut command = child(&helper);
    command.args([
        "--restart",
        "--installed",
        installed,
        "--wait-pid",
        &std::process::id().to_string(),
    ]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    command.spawn()?;
    Ok(true)
}
/// Once per process, after UI readiness. Network errors defer updates and never
/// interfere with an offline Ethernet session.
pub fn ready(installed: &'static str) {
    let _ = health::notify_ready(installed);
    static STARTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if STARTED.swap(true, std::sync::atomic::Ordering::AcqRel) {
        return;
    }
    std::thread::spawn(move || {
        let _ = (|| -> Result<()> {
            let platform = Platform::current()?;
            let root = stage_directory()?;
            let mut floor = None;
            if let Some((_, package)) = newest_staged(&root, &platform.policy(installed))? {
                floor = Some(package.manifest().version.clone());
            }
            let mut policy = platform.policy(installed);
            policy.highest_staged = floor.as_deref();
            download::check_and_stage(&download::SystemHttps, &policy, &root)?;
            Ok(())
        })();
    });
}
pub fn restart_helper(installed: &str, wait_pid: u32) -> Result<()> {
    semver::Version::parse(installed)?;
    let platform = Platform::current()?;
    let root = installation(&std::env::current_exe()?, true)?;
    let executable = app_path(&root);
    wait_for_parent(wait_pid)?;
    let result = (|| -> Result<bool> {
        let lease = runtime_lease(&root)?.ok_or("application is still active; update deferred")?;
        let tx = Transaction::open(&root)?;
        let policy = platform.policy(installed);
        tx.recover(&policy)?;
        let Some((stage, package)) = newest_staged(&stage_directory()?, &policy)? else {
            return Ok(false);
        };
        // Windows runs a versioned helper, so the next version may install a
        // different helper without touching the executable currently in use.
        // Darwin permits atomic rename of a running image; the bundle's helper
        // must be updated together with its code-signing resource seal.
        let current_helper = std::env::current_exe()?
            .file_name()
            .ok_or("missing helper name")?
            .to_string_lossy()
            .to_lowercase();
        if cfg!(windows)
            && package.manifest().files.iter().any(|entry| {
                entry.path.eq_ignore_ascii_case(&current_helper)
                    || entry.path.eq_ignore_ascii_case("lrd-update-helper.exe")
            })
        {
            return fail("running bootstrap helper cannot be replaced");
        }
        if !package.manifest().files.iter().any(|entry| {
            entry.path
                == if cfg!(windows) {
                    "lrd-windows-host.exe"
                } else {
                    "Contents/MacOS/lrd-macos-client"
                }
        }) {
            return fail("update package has no application entry point");
        }
        let version = package.manifest().version.clone();
        let health = health::Health::new(&version)?;
        let mut command = child(&executable);
        health.configure(&mut command)?;
        tx.prepare(&package)?;
        tx.apply(&policy)?;
        drop(lease);
        let mut running = match command.spawn() {
            Ok(child) => child,
            Err(e) => {
                tx.rollback(&policy)?;
                return Err(e.into());
            }
        };
        match health.observe(
            &mut running,
            Duration::from_secs(30),
            Duration::from_secs(2),
        ) {
            Ok(true) => {
                if let Err(error) = tx.confirm_health(&policy) {
                    let _ = running.kill();
                    running.wait()?;
                    let _lease =
                        runtime_lease(&root)?.ok_or("another app is active; rollback deferred")?;
                    tx.rollback(&policy)?;
                    return Err(error);
                }
                // Only a verified stage under this user's update directory.
                let _ = std::fs::remove_dir_all(stage);
                #[cfg(target_os = "macos")]
                {
                    // The headless watcher is a separate process. Load its new
                    // signed-package image too; a disabled agent is not enabled.
                    let domain = format!("gui/{}/local.lanremotedesktop.agent", unsafe {
                        libc::getuid()
                    });
                    let _ = Command::new("/bin/launchctl")
                        .args(["kickstart", "-k", &domain])
                        .stdout(std::process::Stdio::null())
                        .stderr(std::process::Stdio::null())
                        .status();
                }
                Ok(true)
            }
            _ => {
                let _ = running.kill();
                running.wait()?;
                let _lease =
                    runtime_lease(&root)?.ok_or("another app is active; rollback deferred")?;
                tx.rollback(&policy)?;
                fail("new version failed startup; previous version restored")
            }
        }
    })();
    match result {
        Ok(true) => Ok(()),
        outcome => {
            if let Err(ref error) = outcome {
                eprintln!("Update deferred: {error}");
            }
            // If replacement/recovery itself failed, do not start a partially
            // replaced application. An unwritable installation was never changed.
            let lease = runtime_lease(&root)?.ok_or("application is active; restart deferred")?;
            match Transaction::open(&root) {
                Ok(tx) => {
                    tx.recover(&platform.policy(installed))?;
                }
                Err(error)
                    if error
                        .downcast_ref::<std::io::Error>()
                        .is_some_and(|e| e.kind() == std::io::ErrorKind::PermissionDenied) => {}
                Err(error) => return Err(error),
            }
            drop(lease);
            child(&executable)
                .env("LRD_UPDATE_SKIP_ONCE", "1")
                .spawn()?;
            outcome.map(|_| ())
        }
    }
}
fn runtime_lease(root: &Path) -> Result<Option<std::fs::File>> {
    let data = identity::data_directory()?;
    runtime_lease_at(&data, root)
}
fn runtime_lease_at(data: &Path, root: &Path) -> Result<Option<std::fs::File>> {
    identity::private_directory(data)?;
    let canonical = std::fs::canonicalize(root)?;
    let mut normalized = canonical.as_os_str().to_string_lossy().into_owned();
    if cfg!(windows) {
        normalized.make_ascii_lowercase();
    }
    let name = identity::fingerprint(normalized.as_bytes());
    let path = data.join(format!("runtime-{name}.lock"));
    if let Ok(metadata) = std::fs::symlink_metadata(&path) {
        if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() != 0 {
            return fail("invalid application lease file");
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if metadata.permissions().mode() & 0o077 != 0 {
                return fail("application lease must be private");
            }
        }
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(path)?;
    match file.try_lock() {
        Ok(()) => Ok(Some(file)),
        Err(std::fs::TryLockError::WouldBlock) => Ok(None),
        Err(error) => Err(error.into()),
    }
}
fn wait_for_parent(pid: u32) -> Result<()> {
    if pid == 0 || pid == std::process::id() {
        return fail("invalid update parent");
    }
    let deadline = Instant::now() + Duration::from_secs(30);
    #[cfg(windows)]
    {
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut std::ffi::c_void;
            fn WaitForSingleObject(handle: *mut std::ffi::c_void, milliseconds: u32) -> u32;
            fn CloseHandle(handle: *mut std::ffi::c_void) -> i32;
        }
        let handle = unsafe { OpenProcess(0x00100000, 0, pid) };
        if handle.is_null() {
            if std::io::Error::last_os_error().raw_os_error() == Some(87) {
                return Ok(());
            }
            return fail("cannot verify old process exit");
        }
        let result = unsafe { WaitForSingleObject(handle, 30000) };
        unsafe { CloseHandle(handle) };
        let _ = deadline;
        if result != 0 {
            return fail("old application is still running");
        }
    }
    #[cfg(unix)]
    {
        unsafe extern "C" {
            fn kill(pid: i32, signal: i32) -> i32;
        }
        if pid > i32::MAX as u32 {
            return fail("invalid update parent");
        }
        loop {
            if unsafe { kill(pid as i32, 0) } != 0 {
                if std::io::Error::last_os_error().raw_os_error() == Some(3) {
                    break;
                }
                return fail("cannot verify old process exit");
            }
            if Instant::now() >= deadline {
                return fail("old application is still running");
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn an_active_app_blocks_replacement_and_exit_releases_the_lease() {
        let root =
            std::env::temp_dir().join(format!("lrd-active-{:016x}", crate::random_u64().unwrap()));
        let app = root.join("app");
        std::fs::create_dir_all(&app).unwrap();
        let data = root.join("data");
        let first = runtime_lease_at(&data, &app).unwrap().unwrap();
        assert!(runtime_lease_at(&data, &app.join(".")).unwrap().is_none());
        let other_data = data.clone();
        let other_app = app.clone();
        assert!(
            std::thread::spawn(move || runtime_lease_at(&other_data, &other_app)
                .unwrap()
                .is_none())
            .join()
            .unwrap()
        );
        drop(first);
        let next = runtime_lease_at(&data, &app).unwrap().unwrap();
        drop(next);
        std::fs::remove_dir_all(root).unwrap();
    }
}
