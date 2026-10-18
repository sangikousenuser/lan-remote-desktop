use crate::{
    fail,
    tls::Tls,
    wire::{self, Packet},
    Result,
};
use std::{
    io::ErrorKind,
    net::{Shutdown, TcpStream},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, SyncSender, TrySendError},
        Arc,
    },
    time::{Duration, Instant},
};

#[derive(Default)]
struct Counters {
    sent: AtomicU64,
    received: AtomicU64,
    rtt_us: AtomicU64,
    queue_full: AtomicU64,
}
#[derive(Clone, Copy, serde::Serialize)]
pub struct Statistics {
    pub sent_bytes: u64,
    pub received_bytes: u64,
    pub rtt_ms: f64,
    pub queue_full: u64,
}
impl Counters {
    fn snapshot(&self) -> Statistics {
        Statistics {
            sent_bytes: self.sent.load(Ordering::Relaxed),
            received_bytes: self.received.load(Ordering::Relaxed),
            rtt_ms: self.rtt_us.load(Ordering::Relaxed) as f64 / 1000.0,
            queue_full: self.queue_full.load(Ordering::Relaxed),
        }
    }
}
#[derive(Clone)]
pub struct Sender {
    priority: SyncSender<Packet>,
    bulk: SyncSender<Packet>,
    socket: Arc<TcpStream>,
    stopped: Arc<AtomicBool>,
    counters: Arc<Counters>,
    closing: Arc<AtomicBool>,
    identity: u64,
}
impl Sender {
    pub fn identity(&self) -> u64 {
        self.identity
    }
    pub fn statistics(&self) -> Statistics {
        self.counters.snapshot()
    }
    pub fn send(&self, packet: Packet) -> Result<()> {
        let channel = if matches!(packet.kind, wire::VIDEO | wire::FILE | wire::CLIPBOARD) {
            &self.bulk
        } else {
            &self.priority
        };
        channel.try_send(packet).map_err(|_| {
            self.counters.queue_full.fetch_add(1, Ordering::Relaxed);
            std::io::Error::other("outgoing queue is full or closed")
        })?;
        Ok(())
    }
    pub fn close(&self) {
        self.closing.store(true, Ordering::Release);
    }
    pub fn abort(&self) {
        self.stopped.store(true, Ordering::Release);
        let _ = self.socket.shutdown(Shutdown::Both);
    }
    pub fn stopped(&self) -> bool {
        self.stopped.load(Ordering::Acquire) || self.closing.load(Ordering::Acquire)
    }
    pub fn send_bulk(&self, packet: Packet) -> Result<()> {
        self.bulk
            .send(packet)
            .map_err(|_| std::io::Error::other("session closed"))?;
        Ok(())
    }
}

/// One TLS owner handles both directions. Queue/memory limits prevent a stalled
/// renderer or file consumer from building an unbounded heap of video frames.
pub fn start(mut tls: Tls) -> Result<(Sender, Receiver<Packet>)> {
    tls.nonblocking()?;
    let socket = Arc::new(tls.socket().try_clone()?);
    let stopped = Arc::new(AtomicBool::new(false));
    let closing = Arc::new(AtomicBool::new(false));
    let (priority_tx, priority) = mpsc::sync_channel::<Packet>(64);
    let (bulk_tx, bulk) = mpsc::sync_channel::<Packet>(2);
    let (events, incoming) = mpsc::sync_channel(8);
    let counters = Arc::new(Counters::default());
    let sender = Sender {
        identity: crate::random_u64()?,
        priority: priority_tx,
        bulk: bulk_tx,
        socket: socket.clone(),
        stopped: stopped.clone(),
        counters: counters.clone(),
        closing: closing.clone(),
    };
    std::thread::spawn(move || {
        let result = (|| -> Result<()> {
            let mut pending = Vec::new();
            let mut buffer = [0u8; 65536];
            let mut outgoing: Option<(Vec<u8>, usize, bool)> = None;
            let mut replies = std::collections::VecDeque::new();
            let mut last_read = Instant::now();
            let mut last_ping = Instant::now();
            let mut ping_sequence = 0u64;
            let mut finishing = false;
            let mut priority_burst = 0u8;
            loop {
                if stopped.load(Ordering::Acquire) {
                    return Ok(());
                }
                let mut progress = false;
                // Never block a full-duplex connection while writing a large file
                // packet. Rustls buffers at most 64 KiB and both directions advance.
                if !finishing && outgoing.is_none() {
                    let packet = if closing.load(Ordering::Acquire) {
                        Some(Packet {
                            kind: wire::CLOSE,
                            payload: Vec::new(),
                        })
                    } else if let Some(p) = replies.pop_front() {
                        Some(p)
                    } else if priority_burst >= 8 {
                        priority_burst = 0;
                        bulk.try_recv().ok().or_else(|| priority.try_recv().ok())
                    } else if let Ok(p) = priority.try_recv() {
                        priority_burst += 1;
                        Some(p)
                    } else if let Ok(p) = bulk.try_recv() {
                        priority_burst = 0;
                        Some(p)
                    } else if last_ping.elapsed() > Duration::from_secs(1) {
                        ping_sequence = ping_sequence.wrapping_add(1);
                        let mut payload = vec![0];
                        payload.extend(ping_sequence.to_be_bytes());
                        last_ping = Instant::now();
                        Some(Packet {
                            kind: wire::PING,
                            payload,
                        })
                    } else {
                        None
                    };
                    if let Some(packet) = packet {
                        let close = packet.kind == wire::CLOSE;
                        let mut bytes = Vec::with_capacity(packet.payload.len() + 6);
                        packet.write(&mut bytes)?;
                        outgoing = Some((bytes, 0, close));
                    }
                }
                if let Some((bytes, offset, close)) = outgoing.as_mut() {
                    match tls.write_plain(&bytes[*offset..bytes.len().min(*offset + 32768)]) {
                        Ok(n) => {
                            *offset += n;
                            progress |= n > 0;
                            counters.sent.fetch_add(n as u64, Ordering::Relaxed);
                        }
                        Err(e) if e.kind() == ErrorKind::WouldBlock => {}
                        Err(e) => return Err(e.into()),
                    }
                    if *offset == bytes.len() {
                        finishing = *close;
                        outgoing = None;
                    }
                }
                if tls.wants_write() {
                    match tls.flush_available() {
                        Ok(n) => progress |= n > 0,
                        Err(e)
                            if matches!(
                                e.kind(),
                                ErrorKind::WouldBlock | ErrorKind::Interrupted
                            ) => {}
                        Err(e) => return Err(e.into()),
                    }
                }
                if finishing && !tls.wants_write() {
                    return Ok(());
                }
                // A full application queue applies backpressure, preserving bytes.
                let mut consumer_ready = true;
                for _ in 0..32 {
                    if pending.len() < 6 {
                        break;
                    }
                    let n = u32::from_be_bytes(pending[2..6].try_into()?) as usize;
                    if n > wire::MAX_PACKET {
                        return fail("incoming frame exceeds limit");
                    }
                    if pending.len() < 6 + n {
                        break;
                    }
                    let packet = Packet::read(&mut &pending[..6 + n])?;
                    if packet.kind == wire::PING {
                        if packet.payload.len() != 9 {
                            return fail("invalid heartbeat");
                        }
                        if packet.payload[0] == 0 {
                            if replies.len() >= 8 {
                                return fail("heartbeat rate limit");
                            }
                            let mut reply = packet;
                            reply.payload[0] = 1;
                            replies.push_back(reply);
                        } else if packet.payload[0] == 1
                            && u64::from_be_bytes(packet.payload[1..9].try_into()?) == ping_sequence
                        {
                            counters.rtt_us.store(
                                last_ping.elapsed().as_micros().min(u64::MAX as u128) as u64,
                                Ordering::Relaxed,
                            );
                        } else if packet.payload[0] != 1 {
                            return fail("invalid heartbeat type");
                        }
                    } else {
                        let close = packet.kind == wire::CLOSE;
                        match events.try_send(packet) {
                            Ok(()) => {
                                if close {
                                    return Ok(());
                                }
                            }
                            Err(TrySendError::Full(_)) => {
                                consumer_ready = false;
                                break;
                            }
                            Err(TrySendError::Disconnected(_)) => return Ok(()),
                        }
                    }
                    pending.drain(..6 + n);
                    progress = true;
                }
                if consumer_ready && pending.len() < wire::MAX_PACKET + 6 {
                    match tls.read_available(&mut buffer) {
                        Ok(0) => return fail("peer disconnected"),
                        Ok(n) => {
                            pending.extend_from_slice(&buffer[..n]);
                            last_read = Instant::now();
                            progress = true;
                            counters.received.fetch_add(n as u64, Ordering::Relaxed);
                        }
                        Err(e)
                            if matches!(
                                e.kind(),
                                ErrorKind::WouldBlock | ErrorKind::Interrupted
                            ) => {}
                        Err(e) => return Err(e.into()),
                    }
                }
                if pending.len() > wire::MAX_PACKET + 65542 {
                    return fail("incoming buffer limit");
                }
                if last_read.elapsed() > Duration::from_secs(5) {
                    return fail("heartbeat timeout");
                }
                if !progress {
                    std::thread::sleep(Duration::from_millis(1));
                }
            }
        })();
        let _ = result;
        stopped.store(true, Ordering::Release);
        let _ = socket.shutdown(Shutdown::Both);
    });
    Ok((sender, incoming))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{identity::Identity, tls};
    use std::{fs, net::TcpListener};
    #[test]
    fn encrypted_pairing_and_bidirectional_stream() {
        let root = std::env::temp_dir().join(format!("lrd-tls-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let host = Identity::load_or_create(&root.join("host")).unwrap();
        let client = Identity::load_or_create(&root.join("client")).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let host_pin = host.fingerprint();
        let (codes_tx, codes_rx) = mpsc::channel();
        let server = std::thread::spawn(move || {
            let mut tls = Tls::server(listener.accept().unwrap().0, &host).unwrap();
            tls::approve(&mut tls, &host, false, |info| {
                codes_tx.send(info.code.clone()).unwrap();
                true
            })
            .unwrap();
            let (sender, receiver) = start(tls).unwrap();
            let packet = receiver.recv_timeout(Duration::from_secs(3)).unwrap();
            assert_eq!(packet.parse::<u64>().unwrap(), 42);
            sender
                .send(Packet::json(wire::CONFIG, &99).unwrap())
                .unwrap();
            std::thread::sleep(Duration::from_millis(100));
            sender.close();
        });
        let mut tls = Tls::client(
            TcpStream::connect(address).unwrap(),
            &client,
            Some(host_pin),
        )
        .unwrap();
        let own = tls.pairing_code().unwrap();
        tls::approve(&mut tls, &client, true, |_| {
            assert_eq!(codes_rx.recv_timeout(Duration::from_secs(3)).unwrap(), own);
            true
        })
        .unwrap();
        let (sender, receiver) = start(tls).unwrap();
        sender
            .send(Packet::json(wire::CONFIG, &42).unwrap())
            .unwrap();
        assert_eq!(
            receiver
                .recv_timeout(Duration::from_secs(3))
                .unwrap()
                .parse::<u64>()
                .unwrap(),
            99
        );
        sender.close();
        server.join().unwrap();
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn large_full_duplex_packets_survive_small_socket_buffers() {
        let root = std::env::temp_dir().join(format!("lrd-duplex-{}", std::process::id()));
        let host = Identity::load_or_create(&root.join("host")).unwrap();
        let client = Identity::load_or_create(&root.join("client")).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let pin = host.fingerprint();
        fn endpoint(mut tls: Tls, identity: &Identity, client: bool) {
            tls::approve(&mut tls, identity, client, |_| true).unwrap();
            let socket = socket2::SockRef::from(tls.socket());
            socket.set_send_buffer_size(8192).unwrap();
            socket.set_recv_buffer_size(8192).unwrap();
            let (sender, receiver) = start(tls).unwrap();
            let producer = sender.clone();
            let worker = std::thread::spawn(move || {
                for i in 0..8u8 {
                    producer
                        .send_bulk(Packet {
                            kind: wire::FILE,
                            payload: vec![i; 256 * 1024],
                        })
                        .unwrap();
                }
            });
            for i in 0..8u8 {
                let p = receiver.recv_timeout(Duration::from_secs(15)).unwrap();
                assert_eq!(p.kind, wire::FILE);
                assert_eq!(p.payload, vec![i; 256 * 1024]);
            }
            worker.join().unwrap();
            sender
                .send(Packet::json(wire::CONFIG, &42).unwrap())
                .unwrap();
            assert_eq!(
                receiver
                    .recv_timeout(Duration::from_secs(5))
                    .unwrap()
                    .parse::<u64>()
                    .unwrap(),
                42
            );
            std::thread::sleep(Duration::from_millis(50));
            sender.close();
        }
        let server = std::thread::spawn(move || {
            endpoint(
                Tls::server(listener.accept().unwrap().0, &host).unwrap(),
                &host,
                false,
            )
        });
        endpoint(
            Tls::client(TcpStream::connect(address).unwrap(), &client, Some(pin)).unwrap(),
            &client,
            true,
        );
        server.join().unwrap();
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn rejected_pairing_does_not_persist_either_certificate() {
        let root = std::env::temp_dir().join(format!("lrd-deny-{}", std::process::id()));
        let host = Identity::load_or_create(&root.join("host")).unwrap();
        let client = Identity::load_or_create(&root.join("client")).unwrap();
        let client_cert = client.certificate.clone();
        let host_cert = host.certificate.clone();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let mut tls = Tls::server(listener.accept().unwrap().0, &host).unwrap();
            assert!(tls::approve(&mut tls, &host, false, |_| false).is_err());
            assert!(!host.trusted(&client_cert).unwrap());
        });
        let mut tls = Tls::client(TcpStream::connect(address).unwrap(), &client, None).unwrap();
        assert!(tls::approve(&mut tls, &client, true, |_| true).is_err());
        assert!(!client.trusted(&host_cert).unwrap());
        server.join().unwrap();
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn changed_pinned_certificate_is_rejected() {
        let root = std::env::temp_dir().join(format!("lrd-pin-{}", std::process::id()));
        let host = Identity::load_or_create(&root.join("host")).unwrap();
        let client = Identity::load_or_create(&root.join("client")).unwrap();
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let a = l.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            assert!(Tls::server(l.accept().unwrap().0, &host).is_err());
        });
        assert!(Tls::client(
            TcpStream::connect(a).unwrap(),
            &client,
            Some("wrong fingerprint".into())
        )
        .is_err());
        server.join().unwrap();
        fs::remove_dir_all(root).unwrap();
    }
}
