//! Resumable file engine. All receiver paths are resolved through a directory
//! capability; remote names never become ambient filesystem paths.
use crate::{
    fail,
    identity::{fingerprint, hex},
    wire::{self, Packet},
    Result,
};
use cap_std::fs::{Dir, OpenOptions};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

pub const CHUNK: usize = 1024 * 1024;
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FileEntry {
    pub path: String,
    pub size: u64,
    pub sha256: String,
    pub chunks: Vec<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub id: String,
    pub directories: Vec<String>,
    pub files: Vec<FileEntry>,
}
fn valid_hash(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|c| c.is_ascii_hexdigit())
}
pub fn validate_path(path: &str) -> Result<()> {
    if path.is_empty()
        || path.len() > 2048
        || path.contains('\\')
        || path.contains(':')
        || path.starts_with('/')
        || path.chars().any(char::is_control)
    {
        return fail("unsafe transfer path");
    }
    for part in path.split('/') {
        let upper = part
            .split('.')
            .next()
            .unwrap_or("")
            .trim_end_matches(' ')
            .to_ascii_uppercase();
        if part.is_empty()
            || part == "."
            || part == ".."
            || part.ends_with(['.', ' '])
            || part.eq_ignore_ascii_case(".lrd-partials")
            || upper.rsplit_once('~').is_some_and(|(head, tail)| {
                !head.is_empty()
                    && upper.len() <= 8
                    && !tail.is_empty()
                    && tail.bytes().all(|b| b.is_ascii_digit())
            })
            || matches!(
                upper.as_str(),
                "COM¹" | "COM²" | "COM³" | "LPT¹" | "LPT²" | "LPT³"
            )
            || matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL" | "CLOCK$")
            || ((upper.starts_with("COM") || upper.starts_with("LPT"))
                && upper.len() == 4
                && matches!(upper.as_bytes()[3], b'1'..=b'9'))
        {
            return fail("unsafe or reserved transfer path");
        }
    }
    Ok(())
}
impl Manifest {
    pub fn validate(&self) -> Result<()> {
        if self.id.len() != 32
            || !self.id.bytes().all(|b| b.is_ascii_hexdigit())
            || self.files.len() + self.directories.len() > 10000
        {
            return fail("manifest limits");
        }
        let mut names = HashSet::new();
        let mut chunks = 0;
        for path in &self.directories {
            validate_path(path)?;
            if !names.insert(path.to_lowercase()) {
                return fail("duplicate transfer path");
            }
        }
        for file in &self.files {
            validate_path(&file.path)?;
            if !names.insert(file.path.to_lowercase())
                || !valid_hash(&file.sha256)
                || file.size > 1024 * 1024 * 1024 * 1024
                || file.chunks.len() as u64 != file.size.div_ceil(CHUNK as u64)
                || file.chunks.iter().any(|h| !valid_hash(h))
            {
                return fail("invalid file manifest");
            }
            chunks += file.chunks.len();
            if chunks > 65536 {
                return fail("chunk manifest limit");
            }
        }
        Ok(())
    }
}

pub struct Source {
    pub manifest: Manifest,
    paths: Vec<PathBuf>,
    modified: Vec<Option<std::time::SystemTime>>,
    cancelled: std::sync::atomic::AtomicBool,
    sending: std::sync::atomic::AtomicU64,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SavedSource {
    manifest: Manifest,
    paths: Vec<PathBuf>,
    modified: Vec<Option<std::time::SystemTime>>,
}
impl Source {
    fn verify_unchanged(&self) -> Result<()> {
        for (i, path) in self.paths.iter().enumerate() {
            let meta = std::fs::symlink_metadata(path)?;
            if !meta.is_file()
                || meta.file_type().is_symlink()
                || meta.len() != self.manifest.files[i].size
                || meta.modified().ok() != self.modified[i]
            {
                return fail("転送元が変更されました。新しい転送を確認してください");
            }
        }
        Ok(())
    }

    pub fn build(path: &Path) -> Result<Self> {
        let mut source = Self {
            manifest: Manifest {
                id: format!("{:016x}{:016x}", crate::random_u64()?, crate::random_u64()?),
                directories: Vec::new(),
                files: Vec::new(),
            },
            paths: Vec::new(),
            modified: Vec::new(),
            cancelled: std::sync::atomic::AtomicBool::new(false),
            sending: std::sync::atomic::AtomicU64::new(0),
        };
        let name = path
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or("source must have a UTF-8 filename")?
            .to_owned();
        source.visit(path, &name, 0)?;
        source.manifest.validate()?;
        Ok(source)
    }
    fn visit(&mut self, path: &Path, relative: &str, depth: usize) -> Result<()> {
        if depth > 64 || self.paths.len() + self.manifest.directories.len() > 10000 {
            return fail("source directory limits");
        }
        validate_path(relative)?;
        let metadata = std::fs::symlink_metadata(path)?;
        if metadata.file_type().is_symlink() {
            return fail("symlinks are not transferred");
        }
        if metadata.is_dir() {
            self.manifest.directories.push(relative.into());
            let mut entries = std::fs::read_dir(path)?.collect::<std::io::Result<Vec<_>>>()?;
            entries.sort_by_key(|e| e.file_name());
            for entry in entries {
                let name = entry
                    .file_name()
                    .into_string()
                    .map_err(|_| "non-UTF-8 filename")?;
                self.visit(&entry.path(), &format!("{relative}/{name}"), depth + 1)?;
            }
        } else if metadata.is_file() {
            let mut f = std::fs::File::open(path)?;
            let mut total = Sha256::new();
            let mut hashes = Vec::new();
            let mut b = vec![0; CHUNK];
            let mut size = 0;
            loop {
                let n = read_chunk(&mut f, &mut b)?;
                if n == 0 {
                    break;
                }
                total.update(&b[..n]);
                hashes.push(fingerprint(&b[..n]));
                size += n as u64;
                if hashes.len() > 65536 {
                    return fail("source file too large");
                }
            }
            if size != metadata.len() {
                return fail("source changed while building manifest");
            }
            self.paths.push(path.to_owned());
            self.modified.push(metadata.modified().ok());
            self.manifest.files.push(FileEntry {
                path: relative.into(),
                size,
                sha256: hex(&total.finalize()),
                chunks: hashes,
            });
        } else {
            return fail("special files are not transferred");
        }
        Ok(())
    }
    pub fn chunk(&self, file: usize, chunk: usize) -> Result<Vec<u8>> {
        if self.cancelled.load(std::sync::atomic::Ordering::Acquire) {
            return fail("cancelled");
        }
        let entry = self
            .manifest
            .files
            .get(file)
            .ok_or("file index out of bounds")?;
        let expected = entry.chunks.get(chunk).ok_or("chunk index out of bounds")?;
        let path = &self.paths[file];
        let meta = std::fs::symlink_metadata(path)?;
        if !meta.is_file()
            || meta.file_type().is_symlink()
            || meta.len() != entry.size
            || meta.modified().ok() != self.modified[file]
        {
            return fail("source changed; confirm a new transfer");
        }
        let mut f = std::fs::File::open(path)?;
        f.seek(SeekFrom::Start(chunk as u64 * CHUNK as u64))?;
        let length = ((entry.size - chunk as u64 * CHUNK as u64).min(CHUNK as u64)) as usize;
        let mut b = vec![0; length];
        f.read_exact(&mut b)?;
        if fingerprint(&b) != *expected {
            return fail("source changed; confirm a new transfer");
        }
        Ok(b)
    }
}
fn read_chunk(reader: &mut impl Read, bytes: &mut [u8]) -> std::io::Result<usize> {
    let mut n = 0;
    while n < bytes.len() {
        let m = reader.read(&mut bytes[n..])?;
        if m == 0 {
            break;
        }
        n += m;
    }
    Ok(n)
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct State {
    peer: String,
    manifest: Manifest,
    verified: Vec<Vec<bool>>,
    finished: Vec<bool>,
}
pub struct Receiver {
    root: Dir,
    parts: Dir,
    state: State,
}
impl Receiver {
    pub fn open(root: &Path, peer: &str, manifest: Manifest) -> Result<Self> {
        manifest.validate()?;
        if !valid_hash(peer) {
            return fail("authenticated peer fingerprint required");
        }
        std::fs::create_dir_all(root)?;
        if std::fs::symlink_metadata(root)?.file_type().is_symlink() {
            return fail("receive root cannot be a symlink");
        }
        let root = Dir::open_ambient_dir(root, cap_std::ambient_authority())?;
        safe_directories(&root, ".lrd-partials", true)?;
        let folder = format!(".lrd-partials/{}", manifest.id);
        safe_directories(&root, &folder, true)?;
        let parts = root.open_dir(&folder)?;
        let state = if parts.exists("state.json") {
            if parts
                .symlink_metadata("state.json")?
                .file_type()
                .is_symlink()
            {
                return fail("invalid transfer state");
            }
            let b = parts.read("state.json")?;
            if b.len() > 8 * 1024 * 1024 {
                return fail("transfer state size limit");
            }
            let state: State = serde_json::from_slice(&b)?;
            if state.peer != peer
                || state.manifest != manifest
                || state.verified.len() != manifest.files.len()
                || state.finished.len() != manifest.files.len()
            {
                return fail("resume peer/manifest mismatch");
            }
            state
        } else {
            State {
                peer: peer.into(),
                verified: manifest
                    .files
                    .iter()
                    .map(|f| vec![false; f.chunks.len()])
                    .collect(),
                finished: vec![false; manifest.files.len()],
                manifest,
            }
        };
        let mut receiver = Self { root, parts, state };
        receiver.recheck()?;
        for dir in &receiver.state.manifest.directories {
            safe_directories(&receiver.root, dir, false)?;
        }
        receiver.save()?;
        Ok(receiver)
    }
    fn recheck(&mut self) -> Result<()> {
        for i in 0..self.state.manifest.files.len() {
            let entry = &self.state.manifest.files[i];
            if self.state.verified[i].len() != entry.chunks.len() {
                return fail("corrupt resume bitmap");
            }
            if self.state.finished[i] {
                if self
                    .root
                    .symlink_metadata(&entry.path)
                    .is_ok_and(|m| m.file_type().is_symlink())
                {
                    return fail("destination symlink rejected");
                }
                if let Ok(mut f) = self.root.open(&entry.path) {
                    if hash_reader(&mut f)? == entry.sha256 {
                        continue;
                    }
                }
                return fail("completed destination changed; confirm a new transfer");
            }
            let name = format!("{i}.part");
            if self.parts.exists(&name)
                && self.parts.symlink_metadata(&name)?.file_type().is_symlink()
            {
                return fail("partial symlink rejected");
            }
            let mut opts = OpenOptions::new();
            opts.read(true).write(true).create(true);
            let mut f = self.parts.open_with(&name, &opts)?;
            for c in 0..entry.chunks.len() {
                if self.state.verified[i][c] {
                    let length =
                        ((entry.size - c as u64 * CHUNK as u64).min(CHUNK as u64)) as usize;
                    let mut b = vec![0; length];
                    f.seek(SeekFrom::Start(c as u64 * CHUNK as u64))?;
                    if f.read_exact(&mut b).is_err() || fingerprint(&b) != entry.chunks[c] {
                        self.state.verified[i][c] = false;
                    }
                }
            }
        }
        Ok(())
    }
    pub fn missing(&self) -> Vec<(usize, usize)> {
        self.state
            .verified
            .iter()
            .enumerate()
            .flat_map(|(i, chunks)| {
                chunks.iter().enumerate().filter_map(move |(c, done)| {
                    (!done && !self.state.finished[i]).then_some((i, c))
                })
            })
            .collect()
    }
    pub fn receive(&mut self, file: usize, chunk: usize, bytes: &[u8]) -> Result<()> {
        let entry = self
            .state
            .manifest
            .files
            .get(file)
            .ok_or("invalid file index")?;
        let expected = entry.chunks.get(chunk).ok_or("invalid chunk index")?;
        let length = ((entry.size - chunk as u64 * CHUNK as u64).min(CHUNK as u64)) as usize;
        if bytes.len() != length || fingerprint(bytes) != *expected {
            return fail("chunk integrity failure");
        }
        if self.state.finished[file] {
            return Ok(());
        }
        let mut options = OpenOptions::new();
        options.write(true);
        let mut f = self.parts.open_with(format!("{file}.part"), &options)?;
        f.seek(SeekFrom::Start(chunk as u64 * CHUNK as u64))?;
        f.write_all(bytes)?;
        f.sync_data()?;
        self.state.verified[file][chunk] = true;
        self.save()?;
        Ok(())
    }
    pub fn finish(&mut self) -> Result<()> {
        if !self.missing().is_empty() {
            return fail("transfer is incomplete");
        }
        for i in 0..self.state.manifest.files.len() {
            if self.state.finished[i] {
                continue;
            }
            let entry = &self.state.manifest.files[i];
            let name = format!("{i}.part");
            let mut f = self.parts.open(&name)?;
            if f.metadata()?.len() != entry.size || hash_reader(&mut f)? != entry.sha256 {
                return fail("final SHA-256 mismatch");
            }
            drop(f);
            if let Some(parent) = Path::new(&entry.path).parent() {
                if !parent.as_os_str().is_empty() {
                    safe_directories(&self.root, parent.to_str().ok_or("invalid parent")?, false)?;
                }
            }
            // hard_link is atomic and fails if any destination already exists.
            if self.root.exists(&entry.path) {
                if self
                    .root
                    .symlink_metadata(&entry.path)?
                    .file_type()
                    .is_symlink()
                {
                    return fail("destination symlink rejected");
                }
                let mut existing = self.root.open(&entry.path)?;
                if hash_reader(&mut existing)? != entry.sha256 {
                    return fail("destination already exists with different contents");
                }
            } else {
                self.parts.hard_link(&name, &self.root, &entry.path)?;
            }
            self.state.finished[i] = true;
            self.save()?;
            self.parts.remove_file(name)?;
        }
        Ok(())
    }
    fn save(&self) -> Result<()> {
        let bytes = serde_json::to_vec(&self.state)?;
        if self.parts.exists("state.new") {
            if self
                .parts
                .symlink_metadata("state.new")?
                .file_type()
                .is_symlink()
            {
                return fail("state symlink rejected");
            }
            self.parts.remove_file("state.new")?;
        }
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        let mut file = self.parts.open_with("state.new", &options)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file);
        self.parts.rename("state.new", &self.parts, "state.json")?;
        Ok(())
    }
}
fn safe_directories(root: &Dir, path: &str, internal: bool) -> Result<()> {
    if !internal {
        validate_path(path)?;
    }
    let mut current = PathBuf::new();
    for part in path.split('/') {
        current.push(part);
        match root.symlink_metadata(&current) {
            Ok(m) if m.file_type().is_symlink() || !m.is_dir() => {
                return fail("destination symlink/special file rejected")
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => root.create_dir(&current)?,
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}
fn hash_reader(reader: &mut impl Read) -> Result<String> {
    let mut h = Sha256::new();
    let mut bytes = vec![0; CHUNK];
    loop {
        let n = reader.read(&mut bytes)?;
        if n == 0 {
            break;
        }
        h.update(&bytes[..n]);
    }
    Ok(hex(&h.finalize()))
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "op", deny_unknown_fields)]
pub enum Operation {
    Begin {
        manifest: Manifest,
    },
    Need {
        id: String,
        missing: Vec<(usize, usize)>,
    },
    Chunk {
        id: String,
        file: usize,
        chunk: usize,
    },
    Finish {
        id: String,
    },
    Done {
        id: String,
    },
    Cancel {
        id: String,
    },
    Error {
        id: String,
        message: String,
    },
}
pub fn packet(op: &Operation, bytes: &[u8]) -> Result<Packet> {
    let header = serde_json::to_vec(op)?;
    if header.len() > 4 * 1024 * 1024 || bytes.len() > CHUNK {
        return fail("file packet limit");
    }
    let mut payload = (header.len() as u32).to_be_bytes().to_vec();
    payload.extend(header);
    payload.extend(bytes);
    Ok(Packet {
        kind: wire::FILE,
        payload,
    })
}
pub fn unpack(packet: &Packet) -> Result<(Operation, &[u8])> {
    if packet.payload.len() < 4 {
        return fail("truncated file packet");
    }
    let n = u32::from_be_bytes(packet.payload[..4].try_into()?) as usize;
    if n > 4 * 1024 * 1024 || n > packet.payload.len() - 4 {
        return fail("invalid file header length");
    }
    let op: Operation = serde_json::from_slice(&packet.payload[4..4 + n])?;
    let body = &packet.payload[4 + n..];
    if body.len() > CHUNK {
        return fail("file chunk size limit");
    }
    Ok((op, body))
}

pub struct Service {
    root: PathBuf,
    peer: String,
    receivers: HashMap<String, Receiver>,
    sources: HashMap<String, std::sync::Arc<Source>>,
    journal: Option<PathBuf>,
}
impl Service {
    pub fn new(root: PathBuf, peer: String) -> Self {
        Self {
            root,
            peer,
            receivers: HashMap::new(),
            sources: HashMap::new(),
            journal: None,
        }
    }
    pub fn persistent(mut self, directory: &Path) -> Result<Self> {
        crate::identity::private_directory(directory)?;
        for entry in std::fs::read_dir(directory)?.take(32) {
            if self.sources.len() >= 4 {
                break;
            }
            let entry = entry?;
            if entry.path().extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            if let Ok(bytes) = crate::identity::read_private(&entry.path(), 8 * 1024 * 1024) {
                if let Ok(saved) = serde_json::from_slice::<SavedSource>(&bytes) {
                    if saved.manifest.validate().is_ok()
                        && saved.paths.len() == saved.manifest.files.len()
                        && saved.modified.len() == saved.paths.len()
                    {
                        let source = Source {
                            manifest: saved.manifest,
                            paths: saved.paths,
                            modified: saved.modified,
                            cancelled: std::sync::atomic::AtomicBool::new(false),
                            sending: std::sync::atomic::AtomicU64::new(0),
                        };
                        self.sources
                            .insert(source.manifest.id.clone(), std::sync::Arc::new(source));
                    }
                }
            }
        }
        self.journal = Some(directory.to_owned());
        Ok(self)
    }
    fn remove_journal(&self, id: &str) {
        if let Some(dir) = &self.journal {
            if id.len() == 32 && id.bytes().all(|c| c.is_ascii_hexdigit()) {
                let _ = std::fs::remove_file(dir.join(format!("{id}.json")));
            }
        }
    }
    pub fn offer(&mut self, path: &Path) -> Result<Packet> {
        self.offer_source(Source::build(path)?)
    }
    pub fn offer_source(&mut self, source: Source) -> Result<Packet> {
        source.manifest.validate()?;

        if self.sources.len() >= 4 {
            return fail("outgoing transfer limit");
        }
        let p = packet(
            &Operation::Begin {
                manifest: source.manifest.clone(),
            },
            &[],
        )?;
        if let Some(directory) = &self.journal {
            let saved = SavedSource {
                manifest: source.manifest.clone(),
                paths: source.paths.clone(),
                modified: source.modified.clone(),
            };
            let bytes = serde_json::to_vec(&saved)?;
            if bytes.len() > 8 * 1024 * 1024 {
                return fail("source journal size limit");
            }
            crate::identity::write_private_new(
                &directory.join(format!("{}.json", source.manifest.id)),
                &bytes,
            )?;
        }
        self.sources
            .insert(source.manifest.id.clone(), std::sync::Arc::new(source));
        Ok(p)
    }
    pub fn resume_offers(&self) -> Result<Vec<Packet>> {
        self.sources
            .values()
            .map(|s| {
                s.verify_unchanged()?;
                packet(
                    &Operation::Begin {
                        manifest: s.manifest.clone(),
                    },
                    &[],
                )
            })
            .collect()
    }
    pub fn cancel_all(&mut self) -> Result<Vec<Packet>> {
        let ids = self
            .sources
            .keys()
            .chain(self.receivers.keys())
            .cloned()
            .collect::<HashSet<_>>();
        for id in &ids {
            self.remove_journal(id);
        }
        for source in self.sources.values() {
            source
                .cancelled
                .store(true, std::sync::atomic::Ordering::Release);
        }
        self.sources.clear();
        self.receivers.clear();
        ids.into_iter()
            .map(|id| packet(&Operation::Cancel { id }, &[]))
            .collect()
    }
    pub fn handle(&mut self, incoming: Packet, send: &crate::channel::Sender) -> Result<String> {
        let (op, bytes) = unpack(&incoming)?;
        match op {
            Operation::Begin { manifest } => {
                if self.receivers.len() >= 4 {
                    return fail("incoming transfer limit");
                }
                let id = manifest.id.clone();
                let receiver = Receiver::open(&self.root, &self.peer, manifest)?;
                let need = packet(
                    &Operation::Need {
                        id: id.clone(),
                        missing: receiver.missing(),
                    },
                    &[],
                )?;
                self.receivers.insert(id, receiver);
                send.send_bulk(need)?;
                Ok("受信を準備しました".into())
            }
            Operation::Need { id, missing } => {
                let source = self
                    .sources
                    .get(&id)
                    .ok_or("unknown source transfer")?
                    .clone();
                if missing.len() > 65536 {
                    return fail("resume request limit");
                }
                let identity = send.identity();
                if source
                    .sending
                    .swap(identity, std::sync::atomic::Ordering::AcqRel)
                    == identity
                {
                    return Ok("Transfer already sending".into());
                }
                let send = send.clone();
                std::thread::spawn(move || {
                    struct Job(std::sync::Arc<Source>, u64);
                    impl Drop for Job {
                        fn drop(&mut self) {
                            let _ = self.0.sending.compare_exchange(
                                self.1,
                                0,
                                std::sync::atomic::Ordering::AcqRel,
                                std::sync::atomic::Ordering::Acquire,
                            );
                        }
                    }
                    let _job = Job(source.clone(), identity);
                    let result = (|| -> Result<()> {
                        source.verify_unchanged()?;
                        for (file, chunk) in missing {
                            if send.stopped()
                                || source.sending.load(std::sync::atomic::Ordering::Acquire)
                                    != identity
                            {
                                return fail("session ended");
                            }
                            send.send_bulk(packet(
                                &Operation::Chunk {
                                    id: id.clone(),
                                    file,
                                    chunk,
                                },
                                &source.chunk(file, chunk)?,
                            )?)?;
                        }
                        send.send_bulk(packet(&Operation::Finish { id: id.clone() }, &[])?)?;
                        Ok(())
                    })();
                    if result.is_err()
                        && !source.cancelled.load(std::sync::atomic::Ordering::Acquire)
                    {
                        if let Ok(p)=packet(&Operation::Error{id,message:"転送元の変更または通信停止を検出しました。再送には確認が必要です".into()},&[]){let _=send.send_bulk(p);}
                    }
                });
                Ok("転送データを送信しています".into())
            }
            Operation::Chunk { id, file, chunk } => {
                let receiver = self
                    .receivers
                    .get_mut(&id)
                    .ok_or("unknown receiving transfer")?;
                receiver.receive(file, chunk, bytes)?;
                Ok(format!(
                    "受信中 — 残り {} チャンク",
                    receiver.missing().len()
                ))
            }
            Operation::Finish { id } => {
                self.receivers
                    .get_mut(&id)
                    .ok_or("unknown transfer")?
                    .finish()?;
                self.receivers.remove(&id);
                send.send_bulk(packet(&Operation::Done { id }, &[])?)?;
                Ok("SHA-256検証済み。受信フォルダーに保存しました".into())
            }
            Operation::Done { id } => {
                self.remove_journal(&id);
                self.sources.remove(&id);
                Ok("転送完了 — 相手側のSHA-256検証に成功".into())
            }
            Operation::Cancel { id } => {
                self.remove_journal(&id);
                if let Some(source) = self.sources.remove(&id) {
                    source
                        .cancelled
                        .store(true, std::sync::atomic::Ordering::Release);
                }
                self.receivers.remove(&id);
                Ok("転送をキャンセルしました。検証済みチャンクは再開用に保持します".into())
            }
            Operation::Error { message, .. } => Ok(format!(
                "転送エラー: {}",
                message.chars().take(256).collect::<String>()
            )),
        }
    }
}
pub fn downloads() -> Result<PathBuf> {
    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .ok_or("user home missing")?;
    Ok(PathBuf::from(home).join("Downloads/LAN Remote Desktop"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn refuses_cross_platform_unsafe_paths() {
        for path in [
            "../secret",
            "/etc/passwd",
            "C:/secret",
            "ok/../evil",
            "a\\b",
            "file:ads",
            "CON.txt",
            "a/PRN",
            "a/COM1",
            "a/ends.",
            "a/space ",
            ".lrd-partials/x",
            ".LRD-PARTIALS/x",
            "LRD-PA~1/x",
            "CON .txt",
            "COM¹.txt",
        ] {
            assert!(validate_path(path).is_err(), "{path}");
        }
        assert!(validate_path("folder/日本語.txt").is_ok());
    }
    #[test]
    fn resumes_verified_chunks_and_checks_peer_and_corruption() {
        let root = std::env::temp_dir().join(format!("lrd-transfer-{}", std::process::id()));
        let input = root.join("in");
        let output = root.join("out");
        std::fs::create_dir_all(&input).unwrap();
        let path = input.join("data.bin");
        std::fs::write(&path, vec![42; CHUNK + 37]).unwrap();
        let source = Source::build(&path).unwrap();
        let peer = fingerprint(b"peer");
        let manifest = source.manifest.clone();
        let mut r = Receiver::open(&output, &peer, manifest.clone()).unwrap();
        r.receive(0, 0, &source.chunk(0, 0).unwrap()).unwrap();
        drop(r);
        assert!(Receiver::open(&output, &fingerprint(b"other"), manifest.clone()).is_err());
        let mut r = Receiver::open(&output, &peer, manifest.clone()).unwrap();
        assert_eq!(r.missing(), [(0, 1)]);
        assert!(r.receive(0, 1, b"corrupt").is_err());
        r.receive(0, 1, &source.chunk(0, 1).unwrap()).unwrap();
        r.finish().unwrap();
        assert_eq!(
            std::fs::read(output.join("data.bin")).unwrap(),
            std::fs::read(path).unwrap()
        );
        // Windows keeps an open capability directory protected from deletion.
        drop(r);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[cfg(unix)]
    #[test]
    fn directory_symlink_never_escapes_capability() {
        let root = std::env::temp_dir().join(format!("lrd-symlink-{}", std::process::id()));
        std::fs::create_dir_all(root.join("out")).unwrap();
        std::fs::create_dir_all(root.join("secret")).unwrap();
        std::os::unix::fs::symlink(root.join("secret"), root.join("out/link")).unwrap();
        let manifest = Manifest {
            id: "a".repeat(32),
            directories: vec!["link".into()],
            files: vec![],
        };
        assert!(Receiver::open(&root.join("out"), &fingerprint(b"peer"), manifest).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[cfg(test)]
mod durability_tests {
    use super::*;
    #[test]
    fn outgoing_manifest_survives_restart_and_source_changes_stop_resume() {
        let root = std::env::temp_dir().join(format!("lrd-journal-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("source.bin");
        std::fs::write(&path, b"original").unwrap();
        let peer = fingerprint(b"peer");
        let mut service = Service::new(root.join("out"), peer.clone())
            .persistent(&root.join("journal"))
            .unwrap();
        service.offer(&path).unwrap();
        drop(service);
        let service = Service::new(root.join("out"), peer)
            .persistent(&root.join("journal"))
            .unwrap();
        assert_eq!(service.resume_offers().unwrap().len(), 1);
        std::fs::write(path, b"changed!").unwrap();
        assert!(service.resume_offers().is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    #[ignore = "writes and verifies a 1 GiB transfer; run explicitly"]
    fn large_file_over_one_gib_resumes_and_matches_sha256() {
        let root = std::env::temp_dir().join(format!("lrd-large-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("large.bin");
        let file = std::fs::File::create(&path).unwrap();
        file.set_len(1024 * 1024 * 1024 + 123).unwrap();
        drop(file);
        let source = Source::build(&path).unwrap();
        let manifest = source.manifest.clone();
        let peer = fingerprint(b"same authenticated device");
        let mut receiver = Receiver::open(&root.join("out"), &peer, manifest.clone()).unwrap();
        for chunk in 0..512 {
            receiver
                .receive(0, chunk, &source.chunk(0, chunk).unwrap())
                .unwrap();
        }
        drop(receiver);
        let mut receiver = Receiver::open(&root.join("out"), &peer, manifest).unwrap();
        assert_eq!(receiver.missing().len(), 513);
        for (_, chunk) in receiver.missing() {
            receiver
                .receive(0, chunk, &source.chunk(0, chunk).unwrap())
                .unwrap();
        }
        receiver.finish().unwrap();
        assert_eq!(
            std::fs::metadata(root.join("out/large.bin")).unwrap().len(),
            1024 * 1024 * 1024 + 123
        );
        drop(receiver);
        std::fs::remove_dir_all(root).unwrap();
    }
}
