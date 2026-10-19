//! Native application adapter. Call only after HELLO, resume and bilateral
//! approval on the bootstrap TLS connection. No transport fallback is implicit.
use crate::{hybrid, quic, Channel, Scope};
use lrd_protocol::media::{self, Kind, Reassembler};
use lrd_session::{
    channel::{self, external::Driver},
    fail,
    identity::Identity,
    tls::Tls,
    wire::{self, Packet, TransportMode, VideoConfig},
    Result,
};
use std::{
    collections::VecDeque,
    net::SocketAddr,
    sync::{mpsc, Arc},
    time::{Duration, Instant},
};
use tokio::sync::mpsc as queue;
#[cfg(test)]
mod tests;

const FRAGMENT: usize = crate::crypto::MAX_PAYLOAD - media::HEADER_BYTES;
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Upgrade {
    transport: TransportMode,
    port: u16,
}

/// The peer must already have approved this logical session. UDP endpoints are
/// exchanged inside that exact TLS channel, never taken from discovery.
#[allow(clippy::too_many_arguments)]
pub fn start(
    mut tls: Tls,
    scope: Scope,
    identity: Arc<Identity>,
    client: bool,
    mode: TransportMode,
    peer: String,
    epoch: u64,
) -> Result<(channel::Sender, mpsc::Receiver<Packet>)> {
    if mode == TransportMode::Tcp {
        return channel::start(tls);
    }
    if epoch == 0
        || !identity.trusted(&tls.peer_certificate()?)?
        || lrd_session::identity::fingerprint(&tls.peer_certificate()?) != peer
    {
        return fail("transport upgrade requires the approved paired peer");
    }
    let remote_ip = tls.socket().peer_addr()?.ip();
    let udp = match &scope {
        Scope::Ethernet(nic) => nic.udp(if client {
            0
        } else {
            tls.socket()
                .local_addr()?
                .port()
                .checked_add(2)
                .ok_or("media port overflow")?
        })?,
        Scope::LoopbackDiagnostics => scope.udp()?,
    };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    let transport = if mode == TransportMode::Hybrid {
        Channel::Hybrid(Box::new(
            runtime.block_on(hybrid::Hybrid::establish(tls, udp, scope, peer, client))?,
        ))
    } else {
        // Bind before advertising the port; another local process cannot claim
        // the endpoint between authenticated negotiation and QUIC startup.
        let listener =
            if !client {
                Some(runtime.block_on(async {
                    quic::Listener::bind_socket(scope.clone(), &identity, udp)
                })?)
            } else {
                drop(udp);
                None
            };
        Packet::json(
            wire::CONFIG,
            &Upgrade {
                transport: mode,
                port: listener
                    .as_ref()
                    .map(|l| l.local_address().map(|a| a.port()))
                    .transpose()?
                    .unwrap_or(0),
            },
        )?
        .write(&mut tls)?;
        let packet = Packet::read(&mut tls)?;
        if packet.kind != wire::CONFIG {
            return fail("authenticated transport upgrade required");
        }
        let remote: Upgrade = packet.parse()?;
        if remote.transport != mode || (client && remote.port == 0) || (!client && remote.port != 0)
        {
            return fail("transport upgrade mismatch");
        }
        let ask: hybrid::Ask = Arc::new(move |info| !info.first_pair && info.fingerprint == peer);
        let mut binding = tls.export_media_keys()?;
        let proof = lrd_session::identity::fingerprint(&binding);
        binding.fill(0);
        let connection = runtime.block_on(async {
            tokio::time::timeout(Duration::from_secs(12), async {
                if let Some(listener) = listener {
                    listener.accept(&identity, ask).await
                } else {
                    quic::Quic::connect(
                        scope,
                        SocketAddr::new(remote_ip, remote.port),
                        &identity,
                        Some(lrd_session::identity::fingerprint(&tls.peer_certificate()?)),
                        ask,
                    )
                    .await
                }
            })
            .await?
        })?;
        runtime.block_on(async {
            tokio::time::timeout(Duration::from_secs(5), async {
                connection
                    .send_reliable(Packet::json(wire::CONFIG, &proof)?)
                    .await?;
                let packet = connection.receive_reliable().await?;
                if packet.kind != wire::CONFIG || packet.parse::<String>()? != proof {
                    return fail("QUIC bootstrap channel binding mismatch");
                }
                Ok(())
            })
            .await?
        })?;
        drop(tls);
        Channel::Quic(Box::new(connection))
    };
    let (sender, incoming, driver) = channel::external::start()?;
    std::thread::spawn(move || {
        let transport = Arc::new(transport);
        let _ = runtime.block_on(run(transport.clone(), driver, client, epoch));
        transport.abort();
        drop(transport);
        runtime.shutdown_timeout(Duration::from_secs(2));
    });
    Ok((sender, incoming))
}

struct Frame {
    epoch: u64,
    packet: Packet,
}
enum Received {
    Packet(Packet),
    Failed,
}

async fn fragments(
    transport: &Channel,
    frame: Frame,
    sequence: u64,
    started: Instant,
) -> Result<usize> {
    let kind = match frame.packet.kind {
        wire::VIDEO => Kind::Video,
        wire::AUDIO => Kind::Audio,
        _ => return fail("invalid live media type"),
    };
    if frame.packet.payload.is_empty() || frame.packet.payload.len() > media::MAX_FRAME_BYTES {
        return fail("live media size limit");
    }
    let count = frame.packet.payload.len().div_ceil(FRAGMENT);
    if count > media::MAX_FRAGMENTS {
        return fail("live media fragment limit");
    }
    let captured = started.elapsed().as_micros().min(u64::MAX as u128) as u64;
    let mut bytes = 0;
    for (index, payload) in frame.packet.payload.chunks(FRAGMENT).enumerate() {
        let packet = media::Packet {
            kind,
            flags: 0,
            session_id: frame.epoch,
            stream_id: kind as u32,
            sequence,
            fragment_index: index as u16,
            fragment_count: count as u16,
            capture_time_us: captured,
            payload,
        };
        let encoded = packet
            .encode()
            .map_err(|e| std::io::Error::other(format!("media encoding: {e:?}")))?;
        bytes += encoded.len();
        transport.send_datagram(kind as u8, &encoded).await?;
        if index.is_multiple_of(16) {
            tokio::task::yield_now().await;
        }
    }
    Ok(bytes)
}
fn assembly(epoch: u64, kind: Kind) -> Reassembler {
    Reassembler::new(epoch, kind, kind as u32, Duration::from_millis(100))
}

async fn run(
    transport: Arc<Channel>,
    mut driver: Driver,
    client: bool,
    mut epoch: u64,
) -> Result<()> {
    let (control_tx, mut control_rx) = queue::channel(8);
    let (datagram_tx, mut datagram_rx) = queue::channel(64);
    let (media_tx, mut media_rx) = queue::channel::<Frame>(2);
    let (sent_tx, mut sent_rx) = queue::channel::<usize>(4);
    let control_transport = transport.clone();
    let control = tokio::spawn(async move {
        loop {
            let packet = match control_transport.receive_reliable().await {
                Ok(packet) => Received::Packet(packet),
                Err(_) => Received::Failed,
            };
            let failed = matches!(packet, Received::Failed);
            if control_tx.send(packet).await.is_err() || failed {
                break;
            }
        }
    });
    let datagram_transport = transport.clone();
    let datagram = tokio::spawn(async move {
        while let Ok(message) = datagram_transport.receive_datagram().await {
            if datagram_tx.send(message).await.is_err() {
                break;
            }
        }
    });
    let send_transport = transport.clone();
    let media = tokio::spawn(async move {
        let started = Instant::now();
        let mut sequences = [0u64; 2];
        while let Some(frame) = media_rx.recv().await {
            let sequence = &mut sequences[usize::from(frame.packet.kind == wire::AUDIO)];
            *sequence = sequence.checked_add(1).ok_or("media sequence exhausted")?;
            let count = fragments(&send_transport, frame, *sequence, started).await?;
            if sent_tx.send(count).await.is_err() {
                break;
            }
        }
        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    });
    let result = async {
        let started = Instant::now();
        let mut tick = tokio::time::interval(Duration::from_millis(2));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut video = assembly(epoch, Kind::Video);
        let mut audio = assembly(epoch, Kind::Audio);
        let mut last_video: Option<u64> = None;
        let mut last_read = Instant::now();
        let mut last_ping = Instant::now();
        let mut ping_sequence = 0u64;
        let mut keyframe_at = Instant::now() - Duration::from_secs(1);
        let mut keyframe = false;
        let mut pending: VecDeque<Packet> = VecDeque::new();
        let mut datagrams_open = true;
        let mut sent_open = true;
        loop {
            if driver.aborted() { return Ok(()); }
            if driver.closing() {
                transport.finish().await?;
                return Ok(());
            }
            if last_read.elapsed() > Duration::from_secs(5) { return fail("live transport heartbeat expired"); }
            while let Some(packet) = pending.pop_front() {
                let close = packet.kind == wire::CLOSE;
                match driver.deliver(packet) {
                    Ok(()) if close => return Ok(()),
                    Ok(()) => (),
                    Err(mpsc::TrySendError::Full(packet)) => { pending.push_front(packet); break; }
                    Err(_) => return Ok(()),
                }
            }
            tokio::select! {
                _ = tick.tick() => {
                    if let Some(packet) = driver.outgoing() {
                        if matches!(packet.kind, wire::VIDEO | wire::AUDIO) {
                            let video = packet.kind == wire::VIDEO;
                            match media_tx.try_send(Frame { epoch, packet }) {
                                Ok(()) => (),
                                Err(queue::error::TrySendError::Full(_)) => {
                                    driver.dropped(); if video { keyframe = true; }
                                }
                                Err(_) => return fail("live media sender stopped"),
                            }
                        } else {
                            if !client && packet.kind == wire::CONFIG {
                                if let Ok(config) = packet.parse::<VideoConfig>() {
                                    config.validate()?; epoch = config.input_epoch;
                                }
                            }
                            let count = packet.payload.len() + 6;
                            tokio::time::timeout(Duration::from_secs(5), transport.send_reliable(packet)).await??;
                            driver.sent(count);
                        }
                    }
                    if last_ping.elapsed() >= Duration::from_secs(1) {
                        ping_sequence = ping_sequence.checked_add(1).ok_or("heartbeat exhausted")?;
                        let mut payload = vec![0]; payload.extend(ping_sequence.to_be_bytes());
                        transport.send_reliable(Packet {kind:wire::PING,payload}).await?;
                        last_ping = Instant::now(); driver.sent(15);
                    }
                    if keyframe && keyframe_at.elapsed() > Duration::from_millis(500) && pending.len() < 8 {
                        let request = Packet::json(wire::CONFIG, &serde_json::json!({"keyframe":true}))?;
                        if client { transport.send_reliable(request).await?; }
                        else { pending.push_back(request); }
                        keyframe = false; keyframe_at = Instant::now();
                    }
                    if video.expire(started.elapsed()) > 0 { keyframe = true; driver.dropped(); }
                    audio.expire(started.elapsed());
                }
                count = sent_rx.recv(), if sent_open => {
                    if let Some(count) = count { driver.sent(count); } else { sent_open = false; }
                }
                received = control_rx.recv(), if pending.len() < 8 => {
                    let Some(Received::Packet(packet)) = received else { return fail("live reliable transport closed"); };
                    last_read = Instant::now(); driver.received(packet.payload.len() + 6);
                    if packet.kind == wire::PING {
                        if packet.payload.len() != 9 { return fail("invalid live heartbeat"); }
                        let sequence = u64::from_be_bytes(packet.payload[1..].try_into()?);
                        match packet.payload[0] {
                            0 => { let mut packet = packet; packet.payload[0] = 1;
                                transport.send_reliable(packet).await?; driver.sent(15); }
                            1 if sequence == ping_sequence => driver.rtt(last_ping.elapsed()),
                            1 => (), _ => return fail("invalid live heartbeat kind"),
                        }
                    } else {
                        if client && packet.kind == wire::CONFIG {
                            if let Ok(config) = packet.parse::<VideoConfig>() {
                                config.validate()?;
                                epoch = config.input_epoch; video = assembly(epoch, Kind::Video);
                                audio = assembly(epoch, Kind::Audio); last_video = None; keyframe = true;
                            }
                        }
                        pending.push_back(packet);
                    }
                }
                message = datagram_rx.recv(), if datagrams_open => {
                    let Some((kind, bytes)) = message else { datagrams_open = false; continue; };
                    let Ok(packet) = media::decode(&bytes) else { continue; };
                    if !client || kind != packet.kind as u8 || packet.session_id != epoch { continue; }
                    last_read = Instant::now(); driver.received(bytes.len());
                    let kind = packet.kind; let sequence = packet.sequence;
                    let assembled = match kind {
                        Kind::Video => video.push(packet, started.elapsed()),
                        Kind::Audio => audio.push(packet, started.elapsed()),
                        _ => continue,
                    };
                    if let Ok(Some(payload)) = assembled {
                        if kind == Kind::Video {
                            if last_video.is_some_and(|last| sequence > last + 1) {
                                keyframe = true; driver.dropped();
                            }
                            last_video = Some(sequence);
                        }
                        let packet = Packet {kind:if kind==Kind::Video {wire::VIDEO} else {wire::AUDIO},payload};
                        match driver.deliver(packet) {
                            Ok(()) => (),
                            Err(mpsc::TrySendError::Full(_)) => { driver.dropped(); if kind==Kind::Video {keyframe=true;} }
                            Err(mpsc::TrySendError::Disconnected(_)) => return Ok(()),
                        }
                    }
                }
            }
        }
    }.await;
    control.abort();
    datagram.abort();
    media.abort();
    result
}
