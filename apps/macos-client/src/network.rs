use lrd_session::{
    channel, fail,
    identity::{data_directory, Identity},
    network::{self, Nic},
    tls::{self, Tls},
    wire::{self, Hello, Input, InputPacket, Packet, VideoConfig},
    Result,
};
use std::{
    ffi::{c_char, CStr, CString},
    sync::{
        atomic::{AtomicU64, Ordering},
        Mutex, OnceLock,
    },
    time::{Duration, Instant},
};
unsafe extern "C" {
    fn lrd_native_network_event(generation: u64, kind: u32, bytes: *const u8, length: usize);
    fn lrd_native_approval(fingerprint: *const c_char, code: *const c_char, first: bool) -> bool;
}
struct Active {
    sender: channel::Sender,
    epoch: u64,
    sequence: u64,
    files: lrd_session::file_worker::Files,
}
static ACTIVE: OnceLock<Mutex<Option<Active>>> = OnceLock::new();
static GENERATION: AtomicU64 = AtomicU64::new(0);
fn active() -> &'static Mutex<Option<Active>> {
    ACTIVE.get_or_init(|| Mutex::new(None))
}
fn event(generation: u64, kind: u32, bytes: &[u8]) {
    if GENERATION.load(Ordering::Acquire) == generation {
        unsafe {
            lrd_native_network_event(generation, kind, bytes.as_ptr(), bytes.len());
        }
    }
}
fn status(generation: u64, message: &str) {
    event(generation, 1, message.as_bytes());
}
fn cstring(s: &str) -> CString {
    CString::new(s).unwrap_or_default()
}

#[no_mangle]
pub extern "C" fn lrd_interfaces_json() -> *mut c_char {
    let interfaces = network::interfaces();
    cstring(&serde_json::to_string(&interfaces).unwrap_or_else(|_| "[]".into())).into_raw()
}
#[no_mangle]
pub unsafe extern "C" fn lrd_free_string(string: *mut c_char) {
    if !string.is_null() {
        drop(unsafe { CString::from_raw(string) });
    }
}
#[no_mangle]
pub extern "C" fn lrd_discover(index: u32) {
    let generation = GENERATION.load(Ordering::Acquire);
    std::thread::spawn(move || {
        let result = (|| -> Result<()> {
            let nic = Nic::select(index)?;
            let found = network::discover(&nic, Duration::from_secs(3))?;
            event(generation, 6, &serde_json::to_vec(&found)?);
            Ok(())
        })();
        if let Err(e) = result {
            status(generation, &format!("探索できません: {e}"));
        }
    });
}
#[no_mangle]
pub unsafe extern "C" fn lrd_connect(index: u32, address: *const c_char) {
    if address.is_null() {
        return;
    }
    let Ok(address) = (unsafe { CStr::from_ptr(address) }).to_str() else {
        return;
    };
    let address = address.to_owned();
    let generation = GENERATION.fetch_add(1, Ordering::AcqRel) + 1;
    if let Ok(mut old) = active().lock() {
        if let Some(session) = old.take() {
            session.sender.close();
        }
    }
    std::thread::spawn(move || {
        let mut address = address;
        let mut resume: Option<Resume> = None;
        let mut outage: Option<Instant> = None;
        loop {
            if GENERATION.load(Ordering::Acquire) != generation {
                return;
            }
            let mut established = false;
            let result = connection(index, &address, generation, &mut resume, &mut established);
            if GENERATION.load(Ordering::Acquire) != generation {
                return;
            }
            event(generation, 7, b"");
            match result {
                Ok(()) => {
                    status(generation, "切断しました。");
                    return;
                }
                Err(error) => {
                    if error
                        .downcast_ref::<std::io::Error>()
                        .is_some_and(|e| e.kind() == std::io::ErrorKind::PermissionDenied)
                    {
                        resume = None;
                    }
                    if established || outage.is_none() {
                        outage = Some(Instant::now());
                    }
                    if resume.is_none()
                        || outage.is_some_and(|at| at.elapsed() > lrd_session::resume::GRACE)
                    {
                        status(generation, &error.to_string());
                        return;
                    }
                    status(
                        generation,
                        "Ethernet接続の復帰を待っています（最大120秒）。入力は停止しています。",
                    );
                    // Advertisements only suggest a new address. The next TLS
                    // handshake still requires the original certificate pin.
                    if let Some(previous) = resume.as_ref() {
                        if let Ok(nic) = Nic::select_mac(&previous.nic_mac) {
                            if let Ok(hosts) = network::discover(&nic, Duration::from_millis(700)) {
                                if let Some((endpoint, _)) = hosts.into_iter().find(|(_, host)| {
                                    host.fingerprint == previous.pin
                                        && host.user_session == previous.user_session
                                }) {
                                    address = endpoint.to_string();
                                }
                            }
                        }
                    }
                    std::thread::sleep(Duration::from_millis(300));
                }
            }
        }
    });
}
struct Resume {
    pin: String,
    token: String,
    user_session: u32,
    nic_mac: String,
}
fn connection(
    index: u32,
    address: &str,
    generation: u64,
    resume: &mut Option<Resume>,
    established: &mut bool,
) -> Result<()> {
    let nic = match resume.as_ref() {
        Some(previous) => Nic::select_mac(&previous.nic_mac)?,
        None => Nic::select(index)?,
    };
    let identity = Identity::load_or_create(&data_directory()?)?;
    status(generation, "Ethernet経由で暗号化接続しています…");
    let remote = address.parse::<std::net::SocketAddr>().or_else(|_| {
        address
            .parse::<std::net::Ipv4Addr>()
            .map(|a| std::net::SocketAddr::from((a, network::PORT)))
    })?;
    let mut tls = Tls::client(
        nic.connect(remote)?,
        &identity,
        resume.as_ref().map(|r| r.pin.clone()),
    )?;
    let hello = Packet::read(&mut tls)?;
    if hello.kind != wire::HELLO {
        return fail("Host HELLO required");
    }
    let hello: Hello = hello.parse()?;
    if hello.major != 1 || hello.role != "host" {
        return fail("unsupported Host protocol");
    }
    if resume
        .as_ref()
        .is_some_and(|r| r.user_session != hello.user_session)
    {
        *resume = None;
        return fail("Windowsユーザーセッションが変わりました。接続を承認し直してください。");
    }
    Packet::json(
        wire::HELLO,
        &Hello {
            major: 1,
            role: "client".into(),
            user_session: hello.user_session,
            nonce: lrd_session::random_u64()?,
            resume_token: resume.as_ref().map(|r| r.token.clone()),
        },
    )?
    .write(&mut tls)?;
    let ack = Packet::read(&mut tls)?;
    if ack.kind != wire::HELLO {
        return fail("resume negotiation required");
    }
    let ack: wire::ResumeAck = ack.parse()?;
    if let Some(error) = ack.error {
        *resume = None;
        return fail(match error {
            wire::HandshakeError::SessionBusy => {
                "このPCは別のMacが操作中、または再接続の猶予中です。"
            }
            wire::HandshakeError::ResumeExpired => {
                "再接続の承認が失効しました。「接続」から承認し直してください。"
            }
        });
    }
    if resume.is_some() && !ack.resumed {
        *resume = None;
        return fail("再接続の承認が失効しました。「接続」から承認し直してください。");
    }
    let peer = tls::approve(&mut tls, &identity, true, |info| {
        if GENERATION.load(Ordering::Acquire) != generation {
            return false;
        }
        if ack.resumed {
            return !info.first_pair;
        }
        unsafe {
            lrd_native_approval(
                cstring(&info.fingerprint).as_ptr(),
                cstring(&info.code).as_ptr(),
                info.first_pair,
            )
        }
    })?;
    if GENERATION.load(Ordering::Acquire) != generation {
        return fail("cancelled");
    }
    let config = Packet::read(&mut tls)?;
    if config.kind != wire::CONFIG {
        return fail("video configuration required");
    }
    let config: VideoConfig = config.parse()?;
    config.validate()?;
    *resume = Some(Resume {
        pin: peer.clone(),
        token: config.resume_token.clone(),
        user_session: hello.user_session,
        nic_mac: nic.mac.clone(),
    });
    *established = true;
    let (sender, receiver) = channel::start(tls)?;
    let files = lrd_session::file_worker::start(peer.clone(), sender.clone(), move |message| {
        event(generation, 5, message.as_bytes())
    })?;
    {
        let mut slot = active().lock().map_err(|_| "session lock failed")?;
        if GENERATION.load(Ordering::Acquire) != generation {
            sender.close();
            return Ok(());
        }
        *slot = Some(Active {
            sender: sender.clone(),
            epoch: config.input_epoch,
            sequence: 0,
            files: files.clone(),
        });
    }
    event(
        generation,
        2,
        &serde_json::to_vec(
            &serde_json::json!({"width":config.width,"height":config.height,"fps":config.fps,"monitor":config.monitor,"monitors":config.monitors}),
        )?,
    );
    let watch = sender.clone();
    std::thread::spawn(move || {
        while !watch.stopped() {
            if !nic.current() {
                watch.abort();
                break;
            }
            std::thread::sleep(Duration::from_millis(250));
        }
    });
    status(
        generation,
        "接続済み — Windowsの画面を操作できます。⌃⌥⌘Escで入力を解放します。",
    );
    let mut manual = false;
    let mut statistics_at = Instant::now();
    while let Ok(packet) = receiver.recv() {
        if GENERATION.load(Ordering::Acquire) != generation {
            sender.close();
            return Ok(());
        }
        if statistics_at.elapsed() > Duration::from_secs(1) {
            event(generation, 9, &serde_json::to_vec(&sender.statistics())?);
            statistics_at = Instant::now();
        }
        match packet.kind {
            wire::CONFIG => {
                let new: VideoConfig = packet.parse()?;
                new.validate()?;
                if new.session_id != config.session_id || new.resume_token != config.resume_token {
                    return fail("session configuration identity changed");
                }
                if let Ok(mut slot) = active().lock() {
                    if let Some(active) = slot.as_mut() {
                        active.epoch = new.input_epoch;
                        active.sequence = 0;
                    }
                }
                event(
                    generation,
                    2,
                    &serde_json::to_vec(
                        &serde_json::json!({"width":new.width,"height":new.height,"fps":new.fps,"monitor":new.monitor,"monitors":new.monitors}),
                    )?,
                );
            }
            wire::VIDEO => event(generation, 3, &packet.payload),
            wire::CLIPBOARD => {
                if lrd_session::clipboard::unpack(&packet.payload).is_ok() {
                    event(generation, 4, &packet.payload);
                }
            }
            wire::AUDIO => {
                if lrd_session::audio::validate(&packet.payload).is_ok() {
                    event(generation, 8, &packet.payload);
                }
            }
            wire::FILE => {
                if files.incoming.try_send(packet).is_err() {
                    sender.abort();
                    break;
                }
            }
            wire::CLOSE => {
                manual = true;
                break;
            }
            _ => {}
        }
    }
    sender.abort();
    if let Ok(mut slot) = active().lock() {
        if GENERATION.load(Ordering::Acquire) == generation {
            *slot = None;
        }
    }
    if manual {
        *resume = None;
        return Ok(());
    }
    fail("接続が切れました。入力を停止しました。")
}

#[no_mangle]
pub extern "C" fn lrd_disconnect() {
    GENERATION.fetch_add(1, Ordering::AcqRel);
    if let Ok(mut slot) = active().lock() {
        if let Some(session) = slot.take() {
            session.sender.close();
        }
    }
}
#[no_mangle]
pub unsafe extern "C" fn lrd_input_json(bytes: *const u8, length: usize) -> bool {
    if bytes.is_null() || length > 8192 {
        return false;
    }
    let Ok(input) =
        serde_json::from_slice::<Input>(unsafe { std::slice::from_raw_parts(bytes, length) })
    else {
        return false;
    };
    if let Ok(mut slot) = active().lock() {
        if let Some(session) = slot.as_mut() {
            session.sequence = session.sequence.saturating_add(1);
            let p = InputPacket {
                epoch: session.epoch,
                sequence: session.sequence,
                input,
            };
            if p.validate().is_err() {
                return false;
            }
            match Packet::json(wire::INPUT, &p).and_then(|p| session.sender.send(p)) {
                Ok(()) => return true,
                Err(_) => {
                    session.sender.abort();
                    return false;
                }
            }
        }
    }
    false
}

#[no_mangle]
pub unsafe extern "C" fn lrd_send_file(path: *const c_char) {
    if path.is_null() {
        return;
    }
    let Ok(path) = (unsafe { CStr::from_ptr(path) }).to_str() else {
        return;
    };
    if let Ok(slot) = active().lock() {
        if let Some(session) = slot.as_ref() {
            session.files.send(std::path::PathBuf::from(path));
        }
    }
}
#[no_mangle]
pub extern "C" fn lrd_cancel_files() {
    if let Ok(slot) = active().lock() {
        if let Some(session) = slot.as_ref() {
            let files = session.files.clone();
            std::thread::spawn(move || files.cancel());
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn lrd_clipboard_send(format: u8, bytes: *const u8, length: usize) -> bool {
    if bytes.is_null() || length > lrd_session::clipboard::MAX_BYTES {
        return false;
    }
    let data = unsafe { std::slice::from_raw_parts(bytes, length) };
    if let Ok(packet) = lrd_session::clipboard::packet(format, data) {
        if let Ok(slot) = active().lock() {
            if let Some(session) = slot.as_ref() {
                return session.sender.send(packet).is_ok();
            }
        }
    }
    false
}
#[no_mangle]
pub unsafe extern "C" fn lrd_control_json(bytes: *const u8, length: usize) -> bool {
    if bytes.is_null() || length > 4096 {
        return false;
    }
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(unsafe {
        std::slice::from_raw_parts(bytes, length)
    }) else {
        return false;
    };
    if let Ok(slot) = active().lock() {
        if let Some(session) = slot.as_ref() {
            if let Ok(packet) = Packet::json(wire::CONFIG, &value) {
                return session.sender.send(packet).is_ok();
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "requires the Windows MFT output artifact from this CI run"]
    fn windows_software_mft_frame_decodes_on_mac() {
        unsafe extern "C" {
            fn lrd_decode_annexb_probe(bytes: *const u8, length: usize) -> u64;
        }
        let path = std::path::PathBuf::from(
            std::env::var_os("LRD_EXTERNAL_H264_FIXTURE").expect("Windows fixture path missing"),
        );
        let metadata: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path.with_extension("json")).unwrap()).unwrap();
        assert!(std::fs::metadata(&path).unwrap().len() <= 8 * 1024 * 1024);
        let bytes = std::fs::read(path).unwrap();
        assert_eq!(
            metadata["sha256"].as_str().unwrap(),
            lrd_session::identity::fingerprint(&bytes)
        );
        assert_eq!(metadata["desktop_captured"], false);
        assert_eq!(
            unsafe { lrd_decode_annexb_probe(bytes.as_ptr(), bytes.len()) },
            1,
            "Windows MFT output was not decoded by VideoToolbox"
        );
    }
    #[test]
    fn tls_video_reaches_real_videotoolbox_decoder() {
        unsafe extern "C" {
            fn lrd_decode_annexb_probe(bytes: *const u8, length: usize) -> u64;
        }
        let root = std::env::temp_dir().join(format!("lrd-video-tls-{}", std::process::id()));
        let host = Identity::load_or_create(&root.join("host")).unwrap();
        let client = Identity::load_or_create(&root.join("client")).unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let pin = host.fingerprint();
        let server = std::thread::spawn(move || {
            let mut tls = Tls::server(listener.accept().unwrap().0, &host).unwrap();
            tls::approve(&mut tls, &host, false, |_| true).unwrap();
            Packet {
                kind: wire::VIDEO,
                payload: include_bytes!("../../../assets/test-frame.h264").to_vec(),
            }
            .write(&mut tls)
            .unwrap();
        });
        let mut tls = Tls::client(
            std::net::TcpStream::connect(address).unwrap(),
            &client,
            Some(pin),
        )
        .unwrap();
        tls::approve(&mut tls, &client, true, |_| true).unwrap();
        let packet = Packet::read(&mut tls).unwrap();
        assert_eq!(packet.kind, wire::VIDEO);
        assert_eq!(
            unsafe { lrd_decode_annexb_probe(packet.payload.as_ptr(), packet.payload.len()) },
            1
        );
        server.join().unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }
}
