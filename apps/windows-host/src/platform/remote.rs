use super::{input::Injector, stream_encoder::StreamEncoder, Capture, Result, Runtime};
use lrd_session::{
    channel,
    identity::{data_directory, Identity},
    network::{self, Advertisement, Nic},
    tls::{self, Tls},
    wire::{self, Hello, InputPacket, Packet, VideoConfig},
};
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
use windows::{
    core::{w, HSTRING},
    Win32::{
        Foundation::CloseHandle,
        Graphics::Dxgi::{CreateDXGIFactory1, IDXGIFactory1},
        System::{
            RemoteDesktop::ProcessIdToSessionId,
            Threading::{CreateMutexW, GetCurrentProcessId, ReleaseMutex, WaitForSingleObject},
        },
        UI::WindowsAndMessaging::{MessageBoxW, IDYES, MB_ICONQUESTION, MB_TOPMOST, MB_YESNO},
    },
};

// A dedicated owner thread keeps Win32 mutex ownership valid when the lease
// moves between a live session and the reconnect reservation.
struct Controller {
    release: std::sync::mpsc::Sender<()>,
    display: Option<super::display::Lease>,
}
impl Controller {
    fn acquire() -> Option<Self> {
        let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(1);
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || unsafe {
            let Ok(handle) =
                CreateMutexW(None, false, w!("Global\\LANRemoteDesktop.Controller.v1"))
            else {
                let _ = ready_tx.send(false);
                return;
            };
            let wait = WaitForSingleObject(handle, 100).0;
            if wait != 0 && wait != 0x80 {
                let _ = CloseHandle(handle);
                let _ = ready_tx.send(false);
                return;
            }
            if ready_tx.send(true).is_ok() {
                let _ = release_rx.recv();
            }
            let _ = ReleaseMutex(handle);
            let _ = CloseHandle(handle);
        });
        ready_rx.recv().ok().filter(|ok| *ok).map(|_| Self {
            release: release_tx,
            display: None,
        })
    }
}
impl Drop for Controller {
    fn drop(&mut self) {
        if let Some(lease) = self.display.take() {
            if let Err(e) = lease.restore() {
                eprintln!("{e}");
            }
        }
        let _ = self.release.send(());
    }
}
static RESERVATIONS: std::sync::OnceLock<std::sync::Mutex<lrd_session::resume::Book<Controller>>> =
    std::sync::OnceLock::new();
fn reservations() -> &'static std::sync::Mutex<lrd_session::resume::Book<Controller>> {
    RESERVATIONS.get_or_init(|| std::sync::Mutex::new(Default::default()))
}
struct SessionLease {
    control: Option<Controller>,
    peer: String,
    token: String,
    previous_deadline: Option<Instant>,
    live: bool,
    manual: Arc<AtomicBool>,
}
impl Drop for SessionLease {
    fn drop(&mut self) {
        if self.manual.load(Ordering::Acquire) {
            return;
        }
        let deadline = if self.live {
            Some(Instant::now() + lrd_session::resume::GRACE)
        } else {
            self.previous_deadline
        };
        if let Some(deadline) = deadline {
            if deadline >= Instant::now() {
                if let (Some(control), Ok(mut book)) = (self.control.take(), reservations().lock())
                {
                    book.reserve_until(self.peer.clone(), self.token.clone(), control, deadline);
                }
            }
        }
    }
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct DisplayRequest {
    monitor: u32,
    #[serde(default)]
    mode: Option<wire::DisplayMode>,
}
static ACTIVE_FILES: std::sync::OnceLock<
    std::sync::Mutex<Option<lrd_session::file_worker::Files>>,
> = std::sync::OnceLock::new();
fn active_files() -> &'static std::sync::Mutex<Option<lrd_session::file_worker::Files>> {
    ACTIVE_FILES.get_or_init(|| std::sync::Mutex::new(None))
}

static ACCEPTING: AtomicBool = AtomicBool::new(true);
static SHUTDOWN: AtomicBool = AtomicBool::new(false);
type ActiveSession = Option<(channel::Sender, Arc<AtomicBool>)>;
static ACTIVE_SESSION: std::sync::OnceLock<std::sync::Mutex<ActiveSession>> =
    std::sync::OnceLock::new();
fn session() -> &'static std::sync::Mutex<ActiveSession> {
    ACTIVE_SESSION.get_or_init(|| std::sync::Mutex::new(None))
}
pub fn accepting() -> bool {
    ACCEPTING.load(Ordering::Acquire)
}
pub fn toggle_accepting() {
    ACCEPTING.fetch_xor(true, Ordering::AcqRel);
    if !accepting() {
        disconnect();
    }
}
pub fn disconnect() {
    if let Ok(slot) = session().lock() {
        if let Some((sender, manual)) = slot.as_ref() {
            manual.store(true, Ordering::Release);
            sender.close();
        }
    }
    if let Ok(mut book) = reservations().lock() {
        *book = Default::default();
    }
}
pub fn shutdown() {
    SHUTDOWN.store(true, Ordering::Release);
    ACCEPTING.store(false, Ordering::Release);
    disconnect();
}
pub fn send_file(path: std::path::PathBuf) -> Result<()> {
    let slot = active_files().lock().map_err(|_| "file lock")?;
    let files = slot.as_ref().ok_or("Mac is not connected")?;
    files.send(path);
    Ok(())
}
pub fn cancel_files() {
    if let Ok(slot) = active_files().lock() {
        if let Some(files) = slot.as_ref() {
            files.cancel();
        }
    }
}
pub fn run(args: &[String]) -> Result<()> {
    if args.iter().any(|v| v == "--interfaces") {
        return run_once(args);
    }
    loop {
        if SHUTDOWN.load(Ordering::Acquire) {
            return Ok(());
        }
        let result = run_once(args);
        if !args.iter().any(|a| a == "--auto") {
            return result;
        }
        if let Err(error) = result {
            eprintln!("Ethernet待機: {error}");
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}
static SELECTED_MAC: std::sync::OnceLock<String> = std::sync::OnceLock::new();
fn run_once(args: &[String]) -> Result<()> {
    let nics = network::interfaces();
    if args.iter().any(|a| a == "--interfaces") {
        for nic in nics {
            println!(
                "{}  {}  {}/{}  {}",
                nic.index, nic.label, nic.address, nic.prefix, nic.mac
            );
        }
        return Ok(());
    }
    let mut index = None;
    let mut adapter = 0;
    let mut monitor = 0;
    let mut fps = 60;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--serve" | "--auto" => {}
            "--interface" | "--adapter" | "--monitor" | "--fps" => {
                let key = &args[i];
                i += 1;
                let value = args
                    .get(i)
                    .ok_or("option requires a value")?
                    .parse::<u32>()?;
                match key.as_str() {
                    "--interface" => index = Some(value),
                    "--adapter" => adapter = value,
                    "--monitor" => monitor = value,
                    _ => fps = value,
                }
            }
            other => return Err(format!("unknown option: {other}").into()),
        }
        i += 1;
    }
    if !(1..=60).contains(&fps) {
        return Err("fps must be 1..60".into());
    }
    let saved = lrd_session::identity::data_directory()?.join("host-ethernet.json");
    if SELECTED_MAC.get().is_none() && index.is_none() && args.iter().any(|a| a == "--auto") {
        if let Ok(bytes) = std::fs::read(&saved) {
            if bytes.len() < 4096 {
                if let Ok(mac) = serde_json::from_slice::<String>(&bytes) {
                    let _ = SELECTED_MAC.set(mac);
                }
            }
        }
    }
    let nic = if let Some(mac) = SELECTED_MAC.get() {
        nics.iter()
            .find(|n| &n.mac == mac)
            .cloned()
            .ok_or("selected Ethernet unavailable")?
    } else if let Some(index) = index {
        Nic::select(index)?
    } else if nics.len() == 1 {
        nics[0].clone()
    } else {
        return Err("select an Ethernet adapter using --interfaces and --interface INDEX".into());
    };
    let identity = Arc::new(Identity::load_or_create(&data_directory()?)?);
    if SELECTED_MAC.set(nic.mac.clone()).is_ok() {
        let tmp = saved.with_extension(format!("{}.tmp", lrd_session::random_u64()?));
        lrd_session::identity::write_private_new(&tmp, &serde_json::to_vec(&nic.mac)?)?;
        std::fs::rename(tmp, saved)?;
    }
    let mut user_session = 0;
    unsafe {
        ProcessIdToSessionId(GetCurrentProcessId(), &mut user_session)?;
    }
    if let Some(_lease) = Controller::acquire() {
        super::display::recover()?;
    }
    let listener = nic.listen(network::PORT)?;
    listener.set_nonblocking(true)?;
    super::tray::start(format!(
        "{} — {}:{} / Windows session {}",
        nic.label,
        nic.address,
        network::PORT,
        user_session
    ));
    let stop = Arc::new(AtomicBool::new(false));
    let _discovery = network::advertise(
        nic.clone(),
        Advertisement {
            major: 1,
            name: format!("Windows Host / session {user_session}"),
            fingerprint: identity.fingerprint(),
            port: network::PORT,
            user_session,
            nonce: 0,
        },
        stop.clone(),
    )?;
    struct StopAdvertisement(Arc<AtomicBool>);
    impl Drop for StopAdvertisement {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Release);
        }
    }
    let _stop_advertisement = StopAdvertisement(stop);
    println!(
        "Ethernet {}: {}:{} / user session {}",
        nic.label,
        nic.address,
        network::PORT,
        user_session
    );
    println!("Macアプリで探索して接続してください。初回は両方の画面に同じ確認コードが出ます。");
    static BACKGROUND: std::sync::Once = std::sync::Once::new();
    BACKGROUND.call_once(|| {
        std::thread::spawn(|| loop {
            std::thread::sleep(Duration::from_secs(1));
            if let Ok(mut book) = reservations().lock() {
                book.expire(Instant::now());
            }
        });
        std::thread::spawn(|| {
            use std::io::BufRead;
            println!("Macへ送信: send ファイル/フォルダーパス / 転送取消: cancel");
            for line in std::io::stdin()
                .lock()
                .lines()
                .map_while(std::result::Result::ok)
            {
                if let Ok(slot) = active_files().lock() {
                    if let Some(files) = slot.as_ref() {
                        if let Some(path) = line.strip_prefix("send ") {
                            files.send(std::path::PathBuf::from(path.trim().trim_matches('"')));
                        } else if line.trim() == "cancel" {
                            files.cancel();
                        }
                    } else {
                        println!("Macが接続されていません");
                    }
                }
            }
        });
    });
    let pending = Arc::new(AtomicBool::new(false));
    while !SHUTDOWN.load(Ordering::Acquire) && nic.current() {
        let socket = match nic.accept(&listener) {
            Ok(socket) => socket,
            Err(_) => {
                std::thread::sleep(Duration::from_millis(50));
                continue;
            }
        };
        if !accepting() || pending.swap(true, Ordering::AcqRel) {
            drop(socket);
            continue;
        }
        let identity = identity.clone();
        let nic = nic.clone();
        let pending = pending.clone();
        std::thread::spawn(move || {
            struct Clear(Arc<AtomicBool>);
            impl Drop for Clear {
                fn drop(&mut self) {
                    self.0.store(false, Ordering::Release);
                }
            }
            let _clear = Clear(pending);
            if let Err(error) = serve(socket, &identity, nic, user_session, adapter, monitor, fps) {
                eprintln!("接続終了: {error}");
            }
        });
    }
    if SHUTDOWN.load(Ordering::Acquire) {
        disconnect();
    } else if let Ok(slot) = session().lock() {
        if let Some((sender, _)) = slot.as_ref() {
            sender.abort();
        }
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    while pending.load(Ordering::Acquire) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    if SHUTDOWN.load(Ordering::Acquire) {
        disconnect();
    }
    Ok(())
}
fn serve(
    socket: std::net::TcpStream,
    identity: &Identity,
    nic: Nic,
    user_session: u32,
    adapter: u32,
    monitor: u32,
    fps: u32,
) -> Result<()> {
    let mut tls = Tls::server(socket, identity)?;

    Packet::json(
        wire::HELLO,
        &Hello {
            major: 1,
            role: "host".into(),
            user_session,
            nonce: lrd_session::random_u64()?,
            resume_token: None,
        },
    )?
    .write(&mut tls)?;
    let hello = Packet::read(&mut tls)?;
    if hello.kind != wire::HELLO {
        return Err("HELLO required".into());
    }
    let hello: Hello = hello.parse()?;
    if hello.major != 1
        || hello.role != "client"
        || (hello.user_session != 0 && hello.user_session != user_session)
    {
        return Err("protocol/Windows session mismatch".into());
    }
    let peer_certificate = tls.peer_certificate()?;
    let peer_pin = lrd_session::identity::fingerprint(&peer_certificate);
    let (control, resumed, previous_deadline) = {
        let mut book = reservations()
            .lock()
            .map_err(|_| "controller lock failed")?;
        if let Some(token) = hello.resume_token.as_deref() {
            if !identity.trusted(&peer_certificate)? {
                return Err("resume identity is not paired".into());
            }
            if let Some((lease, deadline)) =
                book.claim_with_deadline(&peer_pin, token, Instant::now())
            {
                (lease, true, Some(deadline))
            } else {
                return Err("resume authorization expired or changed".into());
            }
        } else {
            if book.reserved(Instant::now()) {
                return Err("SESSION_BUSY: previous controller reconnect grace".into());
            }
            (
                Controller::acquire().ok_or("SESSION_BUSY: another Mac controls this PC")?,
                false,
                None,
            )
        }
    };
    let manual = Arc::new(AtomicBool::new(false));
    let mut lease = SessionLease {
        control: Some(control),
        peer: peer_pin,
        token: hello.resume_token.unwrap_or_default(),
        previous_deadline,
        live: false,
        manual: manual.clone(),
    };
    Packet::json(wire::HELLO, &wire::ResumeAck { resumed })?.write(&mut tls)?;
    let peer = tls::approve(&mut tls, identity, false, |info| {
        let message=HSTRING::from(format!("Macの確認コードと一致しますか？\n\n{}\n\n端末: {}\n\n一致を確認した場合だけ「はい」を選択してください。",info.code,info.fingerprint));
        unsafe {
            MessageBoxW(
                None,
                &message,
                w!("LAN Remote Desktop — 初回ペアリング"),
                MB_YESNO | MB_ICONQUESTION | MB_TOPMOST,
            ) == IDYES
        }
    })?;
    if !resumed {
        super::display::recover()?;
    }
    let _runtime = Runtime::initialize()?;
    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1()? };
    let mut capture = Capture::new(&factory, adapter, monitor)?;
    let mut encoder = StreamEncoder::new(capture.width, capture.height, fps)?;
    let mut config = VideoConfig {
        width: capture.width,
        height: capture.height,
        fps,
        monitor: (adapter << 16) | monitor,
        monitors: super::display::monitors(&factory)?,
        input_epoch: lrd_session::random_u64()?,
        session_id: lrd_session::random_u64()?,
        resume_token: if resumed {
            lease.token.clone()
        } else {
            lrd_session::resume::token()?
        },
    };
    Packet::json(wire::CONFIG, &config)?.write(&mut tls)?;
    lease.live = true;
    lease.token = config.resume_token.clone();
    let (sender, events) = channel::start(tls)?;
    if let Ok(mut slot) = session().lock() {
        *slot = Some((sender.clone(), manual.clone()));
    }
    struct ClearSession;
    impl Drop for ClearSession {
        fn drop(&mut self) {
            if let Ok(mut slot) = session().lock() {
                *slot = None;
            }
            if let Ok(mut slot) = active_files().lock() {
                *slot = None;
            }
        }
    }
    let _clear_session = ClearSession;
    struct Cleanup {
        sender: channel::Sender,
        worker: Option<std::thread::JoinHandle<()>>,
    }
    impl Drop for Cleanup {
        fn drop(&mut self) {
            self.sender.abort();
            if let Some(worker) = self.worker.take() {
                let _ = worker.join();
            }
        }
    }
    let mut cleanup = Cleanup {
        sender: sender.clone(),
        worker: None,
    };
    let watcher = sender.clone();
    std::thread::spawn(move || {
        while !watcher.stopped() {
            if !nic.current() {
                watcher.abort();
                break;
            }
            std::thread::sleep(Duration::from_millis(250));
        }
    });
    let files = lrd_session::file_worker::start(peer.clone(), sender.clone(), |message| {
        super::tray::transfer_status(message.clone());
        eprintln!("{message}");
    })?;
    if let Ok(mut slot) = active_files().lock() {
        *slot = Some(files.clone());
    }
    let input_manual = manual.clone();
    let clipboard_enabled = Arc::new(AtomicBool::new(false));
    let clipboard = super::clipboard::start(sender.clone(), clipboard_enabled.clone());
    let audio_mode = Arc::new(std::sync::atomic::AtomicU8::new(0));
    super::audio::start(sender.clone(), audio_mode.clone());
    let input_sender = sender.clone();
    let rect = capture.rect;
    let input_state = Arc::new(std::sync::Mutex::new((config.input_epoch, rect)));
    let input_state_worker = input_state.clone();
    let (display_tx, display_rx) = std::sync::mpsc::sync_channel(2);
    cleanup.worker = Some(std::thread::spawn(move || {
        let mut injector = Injector::new(rect);
        let mut last = 0;
        let mut input_epoch = 0;
        let mut last_input = Instant::now();
        loop {
            if last_input.elapsed() > Duration::from_secs(2) {
                injector.release();
            }
            let packet = match events.recv_timeout(Duration::from_millis(250)) {
                Ok(packet) => packet,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                Err(_) => break,
            };
            if packet.kind == wire::CLOSE {
                input_manual.store(true, Ordering::Release);
                break;
            }
            if packet.kind == wire::CLIPBOARD {
                if lrd_session::clipboard::unpack(&packet.payload).is_ok() {
                    let _ = clipboard.try_send(packet.payload);
                }
                continue;
            }
            if packet.kind == wire::CONFIG {
                if let Ok(value) = packet.parse::<serde_json::Value>() {
                    if let Some(display) = value.get("display") {
                        if let Ok(request) =
                            serde_json::from_value::<DisplayRequest>(display.clone())
                        {
                            injector.release();
                            let _ = display_tx.try_send(request);
                        }
                    }
                    if value.get("keyframe").and_then(|v| v.as_bool()) == Some(true) {
                        let _ = display_tx.try_send(DisplayRequest {
                            monitor: u32::MAX,
                            mode: None,
                        });
                    }
                    if let Some(enabled) = value.get("clipboard").and_then(|v| v.as_bool()) {
                        clipboard_enabled.store(enabled, Ordering::Release);
                    }
                    if let Some(mode) = value.get("audio").and_then(|v| v.as_u64()) {
                        audio_mode.store(if mode == 0 { 0 } else { 1 }, Ordering::Release);
                    }
                }
                continue;
            }
            if packet.kind == wire::FILE {
                if files.incoming.try_send(packet).is_err() {
                    input_sender.abort();
                    break;
                }
                continue;
            }
            if packet.kind == wire::INPUT {
                let result = (|| -> Result<()> {
                    let p: InputPacket = packet.parse()?;
                    p.validate()?;
                    let state = input_state_worker
                        .lock()
                        .map_err(|_| "input state unavailable")?;
                    if input_epoch != state.0 {
                        injector.set_rect(state.1);
                        last = 0;
                        input_epoch = state.0;
                    }
                    if p.epoch != input_epoch || p.sequence <= last {
                        return Ok(());
                    }
                    last = p.sequence;
                    last_input = Instant::now();
                    injector.apply(p.input)
                })();
                if result.is_err() {
                    input_sender.abort();
                    break;
                }
            }
        }
        injector.release();
        input_sender.abort();
    }));
    println!("接続承認済み — 画面転送中");
    let mut quality =
        lrd_session::quality::Controller::new(fps, capture.width as u64 * capture.height as u64);
    let mut quality_at = Instant::now();
    let mut encode_average = 0.0;
    let mut previous_stalls = 0;
    encoder.set_bitrate(quality.target().bitrate);
    let mut cached_frame = None;
    let mut last_video = Instant::now() - Duration::from_secs(2);
    while !sender.stopped() {
        let start = Instant::now();
        if quality_at.elapsed() > Duration::from_secs(2) {
            let stats = sender.statistics();
            if let Some(target) = quality.observe(
                encode_average,
                stats.rtt_ms,
                stats.queue_full.saturating_sub(previous_stalls),
            ) {
                encoder.set_bitrate(target.bitrate);
                eprintln!(
                    "Adaptive quality: {} fps / {} bps / encode {:.1} ms / RTT {:.1} ms",
                    target.fps, target.bitrate, encode_average, stats.rtt_ms
                );
            }
            previous_stalls = stats.queue_full;
            quality_at = Instant::now();
        }
        if let Ok(request) = display_rx.try_recv() {
            if request.monitor == u32::MAX {
                encoder.request_keyframe();
                last_video = Instant::now() - Duration::from_secs(2);
            } else if config.monitors.iter().any(|m| {
                m.id == request.monitor
                    && request
                        .mode
                        .as_ref()
                        .is_none_or(|v| v.valid() && m.modes.contains(v))
            }) {
                input_state.lock().map_err(|_| "input state unavailable")?.0 = 0;
                let a = request.monitor >> 16;
                let m = request.monitor & 0xffff;
                if let Some(display) = lease.control.as_mut().and_then(|c| c.display.take()) {
                    display.restore()?;
                }
                drop(capture);
                if let Some(mode) = request.mode {
                    match super::display::Lease::change(&factory, a, m, &mode) {
                        Ok(display) => lease.control.as_mut().unwrap().display = Some(display),
                        Err(e) => eprintln!("表示モード変更を拒否: {e}"),
                    }
                }
                capture = Capture::new(&factory, a, m)?;
                encoder = StreamEncoder::new(capture.width, capture.height, fps)?;
                cached_frame = None;
                config.width = capture.width;
                config.height = capture.height;
                config.monitor = request.monitor;
                config.monitors = super::display::monitors(&factory)?;
                config.input_epoch = lrd_session::random_u64()?;
                *input_state.lock().map_err(|_| "input state unavailable")? =
                    (config.input_epoch, capture.rect);
                sender.send_bulk(Packet::json(wire::CONFIG, &config)?)?;
            }
        }
        let changed = match capture.frame() {
            Ok(Some(frame)) => {
                cached_frame = Some(frame);
                true
            }
            Ok(None) => false,
            Err(error) => {
                eprintln!("Capture interrupted; reinitializing on reconnect: {error}");
                break;
            }
        };
        if let Some(frame) = cached_frame
            .as_ref()
            .filter(|_| changed || last_video.elapsed() > Duration::from_secs(1))
        {
            let encode_at = Instant::now();
            let encoded = encoder.encode(frame)?;
            encode_average = encode_average * 0.9 + encode_at.elapsed().as_secs_f64() * 100.0;
            for bytes in encoded {
                last_video = Instant::now();
                if sender
                    .send_bulk(Packet {
                        kind: wire::VIDEO,
                        payload: bytes,
                    })
                    .is_err()
                {
                    break;
                }
            }
        }
        if let Some(wait) =
            Duration::from_secs_f64(1.0 / quality.target().fps as f64).checked_sub(start.elapsed())
        {
            std::thread::sleep(wait);
        }
    }
    sender.abort();
    drop(cleanup);
    if let Ok(mut slot) = active_files().lock() {
        *slot = None;
    }
    Ok(())
}
