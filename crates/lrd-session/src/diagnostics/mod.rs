//! Typed numeric local logs. No field accepts text, paths, peer identifiers,
//! keyboard data, clipboard contents, frame pixels or file contents.
pub mod resources;
pub mod traffic;
use crate::{fail, identity, Result};
use serde::{Deserialize, Serialize};
use std::{
    fs::{File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc::{self, SyncSender},
        OnceLock,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Sample {
    pub unix_ms: u64,
    pub rtt_ms: Option<f64>,
    pub encode_ms: Option<f64>,
    pub capture_ms: Option<f64>,
    pub decode_ms: Option<f64>,
    pub render_ms: Option<f64>,
    pub render_gpu_ms: Option<f64>,
    pub capture_fps: Option<f64>,
    pub decode_fps: Option<f64>,
    pub render_fps: Option<f64>,
    pub transport_bitrate_bps: Option<u64>,
    pub encoder_target_bitrate_bps: Option<u64>,
    pub encoder_hardware: Option<bool>,
    pub decoder_hardware: Option<bool>,
    pub sent_bytes: Option<u64>,
    pub received_bytes: Option<u64>,
    pub decode_errors: Option<u64>,
    pub dropped_frames: Option<u64>,
    pub queue_stalls: Option<u64>,
    pub reconnects: Option<u64>,
    pub file_send_bytes_per_second: Option<u64>,
    pub file_receive_bytes_per_second: Option<u64>,
    pub cpu_percent: Option<f64>,
    pub resident_bytes: Option<u64>,
    pub peak_resident_bytes: Option<u64>,
}
impl Sample {
    pub fn validate(&self) -> Result<()> {
        for value in [
            self.rtt_ms,
            self.encode_ms,
            self.capture_ms,
            self.decode_ms,
            self.render_ms,
            self.render_gpu_ms,
            self.capture_fps,
            self.decode_fps,
            self.render_fps,
            self.cpu_percent,
        ]
        .into_iter()
        .flatten()
        {
            if !value.is_finite() || value < 0. {
                return fail("invalid numeric diagnostic sample");
            }
        }
        Ok(())
    }
}
#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub max_bytes: u64,
    pub retention_days: u32,
    pub segment_bytes: u64,
}
impl Default for Policy {
    fn default() -> Self {
        Self {
            max_bytes: 100 * 1024 * 1024,
            retention_days: 7,
            segment_bytes: 1024 * 1024,
        }
    }
}
impl Policy {
    fn validate(&self) -> Result<()> {
        if !(32768..=1024 * 1024 * 1024).contains(&self.max_bytes)
            || !(1..=31).contains(&self.retention_days)
            || self.segment_bytes < 4096
            || self.segment_bytes > self.max_bytes
            || self.segment_bytes > 16 * 1024 * 1024
            || self.max_bytes.div_ceil(self.segment_bytes) > 1024
        {
            return fail("diagnostic log policy limits");
        }
        Ok(())
    }
}
struct Store {
    root: PathBuf,
    policy: Policy,
    current: Option<(PathBuf, File, u64)>,
}
fn clock_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}
fn timestamp(name: &str) -> Option<u64> {
    let base = name.strip_prefix("metrics-")?.strip_suffix(".jsonl")?;
    let (time, nonce) = base.split_once('-')?;
    if nonce.len() != 16 || !nonce.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    time.parse().ok()
}
impl Store {
    fn open(root: &Path, policy: Policy) -> Result<Self> {
        policy.validate()?;
        identity::private_directory(root)?;
        let store = Self {
            root: root.into(),
            policy,
            current: None,
        };
        store.prune(clock_ms(), 0)?;
        Ok(store)
    }
    fn prune(&self, now: u64, incoming: u64) -> Result<()> {
        let mut files = Vec::new();
        let mut total = 0u64;
        for (index, entry) in std::fs::read_dir(&self.root)?.enumerate() {
            if index >= 2048 {
                return fail("diagnostic directory entry limit");
            }
            let entry = entry?;
            let name = entry.file_name();
            let Some(at) = timestamp(&name.to_string_lossy()) else {
                continue;
            };
            if !entry.file_type()?.is_file() || entry.file_type()?.is_symlink() {
                continue;
            }
            let metadata = entry.metadata()?;
            let path = entry.path();
            let current = self.current.as_ref().is_some_and(|(p, _, _)| p == &path);
            if !current && now.saturating_sub(at) > self.policy.retention_days as u64 * 86400000 {
                std::fs::remove_file(path)?;
                continue;
            }
            total = total.saturating_add(metadata.len());
            files.push((at, path, metadata.len(), current));
        }
        files.sort_by_key(|f| f.0);
        for (_, path, size, current) in files {
            if total.saturating_add(incoming) <= self.policy.max_bytes {
                break;
            }
            if current {
                continue;
            }
            std::fs::remove_file(path)?;
            total = total.saturating_sub(size);
        }
        if total.saturating_add(incoming) > self.policy.max_bytes {
            return fail("diagnostic log quota");
        }
        Ok(())
    }
    fn append(&mut self, sample: &Sample) -> Result<()> {
        sample.validate()?;
        let mut bytes = serde_json::to_vec(sample)?;
        bytes.push(b'\n');
        if bytes.len() > 4096 {
            return fail("diagnostic record limit");
        }
        let now = clock_ms();
        if self.current.as_ref().is_some_and(|(_, _, n)| {
            n.saturating_add(bytes.len() as u64) > self.policy.segment_bytes
        }) || self.current.as_ref().is_some_and(|(path, _, _)| {
            timestamp(&path.file_name().unwrap_or_default().to_string_lossy()).is_some_and(|at| {
                now.saturating_sub(at) > self.policy.retention_days as u64 * 86400000
            })
        }) {
            if let Some((_, file, _)) = self.current.take() {
                file.sync_all()?;
            }
        }
        self.prune(now, bytes.len() as u64)?;
        if self.current.is_none() {
            let path = self
                .root
                .join(format!("metrics-{now}-{:016x}.jsonl", crate::random_u64()?));
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            self.current = Some((path.clone(), options.open(path)?, 0));
        }
        if let Some((_, file, size)) = self.current.as_mut() {
            file.write_all(&bytes)?;
            file.flush()?;
            *size += bytes.len() as u64;
        }
        Ok(())
    }
}
static LOST: AtomicU64 = AtomicU64::new(0);
static LATEST: OnceLock<std::sync::Mutex<Sample>> = OnceLock::new();
pub fn latest() -> Option<Sample> {
    LATEST.get().and_then(|s| s.lock().ok().map(|s| s.clone()))
}
pub fn dropped_samples() -> u64 {
    LOST.load(Ordering::Relaxed)
}
/// Export only parsed numeric records, never arbitrary files in the log folder.
pub fn export(destination: &Path) -> Result<u64> {
    let directory = identity::data_directory()?;
    export_from(&directory.join("logs"), destination, Some(&directory))
}
fn export_from(root: &Path, destination: &Path, private_root: Option<&Path>) -> Result<u64> {
    let parent = destination
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let parent = std::fs::canonicalize(parent)?;
    if let Some(private) = private_root.filter(|p| p.exists()) {
        if parent.starts_with(std::fs::canonicalize(private)?) {
            return fail("choose a destination outside private app data");
        }
    }
    let temporary = parent.join(format!(".lrd-export-{:016x}.tmp", crate::random_u64()?));
    let result = (|| -> Result<u64> {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        file.write_all(b"{\"schema\":1,\"numeric_diagnostics_only\":true,\"samples\":[")?;
        let mut paths = Vec::new();
        if root.exists() {
            identity::private_directory(root)?;
            for (index, entry) in std::fs::read_dir(root)?.enumerate() {
                if index >= 2048 {
                    return fail("diagnostic directory entry limit");
                }
                let entry = entry?;
                let name = entry.file_name();
                if let Some(at) = timestamp(&name.to_string_lossy()) {
                    if entry.file_type()?.is_file() && !entry.file_type()?.is_symlink() {
                        paths.push((at, entry.path()));
                    }
                }
            }
        }
        paths.sort_by_key(|(at, _)| std::cmp::Reverse(*at));
        let mut written = 0;
        let mut skipped = 0u64;
        let mut total = 0usize;
        let mut truncated = false;
        for (_, path) in paths {
            let bytes = match identity::read_private(&path, 16 * 1024 * 1024) {
                Ok(bytes) => bytes,
                Err(_) => {
                    skipped += 1;
                    continue;
                }
            };
            total = total.saturating_add(bytes.len());
            if total > 100 * 1024 * 1024 {
                truncated = true;
                break;
            }
            for line in bytes.split(|b| *b == b'\n').filter(|line| !line.is_empty()) {
                if line.len() > 4096 {
                    skipped += 1;
                    continue;
                }
                let Ok(sample) = serde_json::from_slice::<Sample>(line) else {
                    skipped += 1;
                    continue;
                };
                if sample.validate().is_err() {
                    skipped += 1;
                    continue;
                }
                if written > 0 {
                    file.write_all(b",")?;
                }
                serde_json::to_writer(&mut file, &sample)?;
                written += 1;
            }
        }
        write!(
            file,
            "],\"skipped_records\":{skipped},\"truncated\":{truncated}}}"
        )?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temporary, destination)?;
        Ok(written)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(temporary);
    }
    result
}
pub fn record(mut sample: Sample) {
    if sample.validate().is_err() {
        LOST.fetch_add(1, Ordering::Relaxed);
        return;
    }
    static LOGGER: OnceLock<SyncSender<Sample>> = OnceLock::new();
    let logger = LOGGER.get_or_init(|| {
        let (tx, rx) = mpsc::sync_channel::<Sample>(32);
        std::thread::spawn(move || {
            let mut store = None;
            let mut meter = resources::Meter::default();
            let mut retry = std::time::Instant::now();
            while let Ok(mut sample) = rx.recv() {
                sample.unix_ms = clock_ms();
                let resource = meter.sample();
                sample.cpu_percent = resource.cpu_percent;
                sample.resident_bytes = resource.resident_bytes;
                sample.peak_resident_bytes = resource.peak_resident_bytes;
                if let Ok(mut latest) = LATEST.get_or_init(Default::default).lock() {
                    *latest = sample.clone();
                }
                if store.is_none() && std::time::Instant::now() >= retry {
                    store = (|| -> Result<Store> {
                        let directory = identity::data_directory()?;
                        let path = directory.join("log-policy.json");
                        let policy = if path.exists() {
                            serde_json::from_slice(&identity::read_private(&path, 4096)?)?
                        } else {
                            Policy::default()
                        };
                        Store::open(&directory.join("logs"), policy)
                    })()
                    .ok();
                    retry = std::time::Instant::now() + Duration::from_secs(60);
                }
                if store
                    .as_mut()
                    .is_none_or(|store| store.append(&sample).is_err())
                {
                    LOST.fetch_add(1, Ordering::Relaxed);
                    store = None;
                }
            }
        });
        tx
    });
    sample.unix_ms = 0;
    if logger.try_send(sample).is_err() {
        LOST.fetch_add(1, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rotation_retention_and_total_quota_preserve_unknown_files() {
        let root = std::env::temp_dir().join(format!("lrd-logs-{}", crate::random_u64().unwrap()));
        identity::private_directory(&root).unwrap();
        std::fs::write(root.join("keep.txt"), b"untouched").unwrap();
        std::fs::write(root.join("metrics-1-0000000000000001.jsonl"), b"expired").unwrap();
        let policy = Policy {
            max_bytes: 32768,
            retention_days: 7,
            segment_bytes: 4096,
        };
        let mut store = Store::open(&root, policy).unwrap();
        assert!(!root.join("metrics-1-0000000000000001.jsonl").exists());
        for _ in 0..300 {
            store
                .append(&Sample {
                    unix_ms: clock_ms(),
                    rtt_ms: Some(2.),
                    ..Default::default()
                })
                .unwrap();
        }
        drop(store);
        let files: Vec<_> = std::fs::read_dir(&root)
            .unwrap()
            .map(|e| e.unwrap())
            .filter(|e| timestamp(&e.file_name().to_string_lossy()).is_some())
            .collect();
        assert!(files.len() > 1);
        assert!(
            files
                .iter()
                .map(|e| e.metadata().unwrap().len())
                .sum::<u64>()
                <= policy.max_bytes
        );
        for file in files {
            let bytes = std::fs::read(file.path()).unwrap();
            assert!(bytes.ends_with(b"\n"));
            for line in bytes.split(|b| *b == b'\n').filter(|line| !line.is_empty()) {
                serde_json::from_slice::<serde_json::Value>(line).unwrap();
            }
        }
        assert_eq!(std::fs::read(root.join("keep.txt")).unwrap(), b"untouched");
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn non_finite_samples_and_invalid_limits_are_rejected() {
        assert!(Sample {
            rtt_ms: Some(f64::NAN),
            ..Default::default()
        }
        .validate()
        .is_err());
        assert!(Sample {
            encode_ms: Some(-1.),
            ..Default::default()
        }
        .validate()
        .is_err());
        assert!(Policy {
            max_bytes: 1024,
            ..Default::default()
        }
        .validate()
        .is_err());
    }
    #[test]
    fn export_excludes_unknown_files_text_and_incomplete_records() {
        let root =
            std::env::temp_dir().join(format!("lrd-export-{}", crate::random_u64().unwrap()));
        identity::private_directory(&root).unwrap();
        let logs = root.join("logs");
        identity::private_directory(&logs).unwrap();
        let sample = serde_json::to_vec(&Sample {
            unix_ms: 1,
            rtt_ms: Some(2.),
            ..Default::default()
        })
        .unwrap();
        let mut bytes = sample;
        bytes.extend(b"\n{\"keyboard\":\"do not export\"}\n{truncated\n");
        identity::write_private_new(&logs.join("metrics-1-0000000000000001.jsonl"), &bytes)
            .unwrap();
        std::fs::write(logs.join("other.txt"), b"unrelated private data").unwrap();
        let destination = root.join("export.json");
        assert_eq!(export_from(&logs, &destination, None).unwrap(), 1);
        let exported = std::fs::read(destination).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&exported).unwrap();
        assert_eq!(value["skipped_records"], 2);
        assert_eq!(value["samples"].as_array().unwrap().len(), 1);
        assert!(!String::from_utf8(exported).unwrap().contains("private"));
        assert!(export_from(&logs, &root.join("paired.json"), Some(&root)).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
}
