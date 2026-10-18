//! Identical H.264 frames, framing, pacing and reliable bulk workload on both
//! candidates. RTT is NOT input-to-display latency or measured display FPS.
use crate::{
    crypto::MAX_PAYLOAD,
    hybrid::{Ask, Hybrid},
    quic::{Listener, Quic},
    Channel, Scope,
};
use lrd_protocol::media::{self, Kind, Reassembler};
use lrd_session::{
    fail,
    identity::{fingerprint, Identity},
    wire::{self, Packet},
    Result,
};
use serde::{Deserialize, Serialize};
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub samples: u32,
    pub interval_us: u64,
    pub timeout_ms: u64,
    pub bulk_chunks: u32,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            samples: 120,
            interval_us: 16667,
            timeout_ms: 250,
            bulk_chunks: 128,
        }
    }
}
impl Config {
    pub fn validate(&self) -> Result<()> {
        if !(1..=60000).contains(&self.samples)
            || self.interval_us > 1000000
            || !(10..=5000).contains(&self.timeout_ms)
            || self.bulk_chunks > 1024
        {
            return fail("benchmark configuration limits");
        }
        Ok(())
    }
}
#[derive(Serialize)]
pub struct Report {
    pub schema: u32,
    pub transport: &'static str,
    pub scope: String,
    pub samples_sent: u32,
    pub frames_echoed: u32,
    pub timed_out: u32,
    pub frame_bytes: usize,
    pub frame_sha256: String,
    pub interval_us: u64,
    pub reliable_bytes_echoed: u64,
    pub elapsed_ms: f64,
    pub completed_frame_rtt_p50_us: Option<u64>,
    pub completed_frame_rtt_p95_us: Option<u64>,
    pub completed_frame_rtt_p99_us: Option<u64>,
    pub input_to_display_measured: bool,
    pub display_fps_measured: bool,
    pub hardware_codec_measured: bool,
    pub selected_winner: Option<String>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Hello {
    config: Config,
    session: u64,
    frame_sha256: String,
}
const FRAME: &[u8] = include_bytes!("../../../assets/test-frame.h264");
const BULK: usize = 65536;
fn fragments(session: u64, sequence: u64, at: u64) -> Result<Vec<Vec<u8>>> {
    let maximum = MAX_PAYLOAD - media::HEADER_BYTES;
    let count = FRAME.len().div_ceil(maximum) as u16;
    FRAME
        .chunks(maximum)
        .enumerate()
        .map(|(i, part)| {
            media::Packet {
                kind: Kind::Video,
                flags: 1,
                session_id: session,
                stream_id: 0,
                sequence,
                fragment_index: i as u16,
                fragment_count: count,
                capture_time_us: at,
                payload: part,
            }
            .encode()
            .map_err(|_| std::io::Error::other("benchmark frame encoding rejected").into())
        })
        .collect()
}
pub async fn echo(channel: Arc<Channel>) -> Result<()> {
    let packet = channel.receive_reliable().await?;
    if packet.kind != wire::CONFIG {
        return fail("benchmark hello required");
    }
    let hello: Hello = packet.parse()?;
    hello.config.validate()?;
    if hello.session == 0 || hello.frame_sha256 != fingerprint(FRAME) {
        return fail("benchmark fixture mismatch");
    }
    let stopped = Arc::new(AtomicBool::new(false));
    let notify = Arc::new(tokio::sync::Notify::new());
    let reliable = channel.clone();
    let done = stopped.clone();
    let wake = notify.clone();
    let chunks = hello.config.bulk_chunks;
    let file_task = tokio::spawn(async move {
        let result = (async {
            for _ in 0..chunks {
                let packet = reliable.receive_reliable().await?;
                if packet.kind != wire::FILE || packet.payload.len() != BULK {
                    return fail("benchmark bulk packet mismatch");
                }
                reliable.send_reliable(packet).await?;
            }
            if reliable.receive_reliable().await?.kind != wire::CLOSE {
                return fail("benchmark close required");
            }
            Ok(())
        })
        .await;
        done.store(true, Ordering::Release);
        wake.notify_one();
        result
    });
    let result=(async {
        let mut assembler=Reassembler::new(hello.session,Kind::Video,0,Duration::from_secs(5));let started=Instant::now();
        while !stopped.load(Ordering::Acquire) {
            tokio::select! {
                _=notify.notified()=>break,
                packet=channel.receive_datagram()=>{
                    let(kind,bytes)=match packet {Ok(packet)=>packet,Err(_)=>break};
                    if kind!=1 {return fail("benchmark media kind mismatch");}
                    let fragment=media::decode(&bytes).map_err(|_|"benchmark fragment rejected")?;
                    if fragment.sequence==0 || fragment.sequence>hello.config.samples as u64 {return fail("benchmark sequence limit");}
                    match assembler.push(fragment,started.elapsed()) {
                        Ok(Some(frame))=>if fingerprint(&frame)!=hello.frame_sha256 {return fail("benchmark H.264 integrity mismatch");},
                        Ok(None)|Err(media::Error::Stale)=>{},
                        Err(_)=>return fail("benchmark frame rejected"),
                    }
                    channel.send_datagram(1,&bytes).await?;
                }
            }
        }
        Ok(())
    }).await;
    if result.is_err() {
        file_task.abort();
        let _ = file_task.await;
        return result;
    }
    file_task.await??;
    Ok(())
}
pub async fn measure(
    channel: Arc<Channel>,
    config: Config,
    transport: &'static str,
    scope: String,
) -> Result<Report> {
    config.validate()?;
    let session = lrd_session::random_u64()?;
    channel
        .send_reliable(Packet::json(
            wire::CONFIG,
            &Hello {
                config: config.clone(),
                session,
                frame_sha256: fingerprint(FRAME),
            },
        )?)
        .await?;
    let reliable = channel.clone();
    let chunks = config.bulk_chunks;
    let file_task = tokio::spawn(async move {
        let mut bytes = 0u64;
        for i in 0..chunks {
            let mut block = vec![0x42; BULK];
            block[..4].copy_from_slice(&i.to_be_bytes());
            reliable
                .send_reliable(Packet {
                    kind: wire::FILE,
                    payload: block.clone(),
                })
                .await?;
            let echoed = reliable.receive_reliable().await?;
            if echoed.kind != wire::FILE || echoed.payload != block {
                return fail("benchmark bulk integrity mismatch");
            }
            bytes += BULK as u64;
        }
        Ok(bytes)
    });
    let started = Instant::now();
    let mut rtts = lrd_common::telemetry::Latencies::new(config.samples as usize);
    let mut assembler = Reassembler::new(session, Kind::Video, 0, Duration::from_secs(5));
    let mut received = 0;
    let result = (async {
        for sequence in 1..=config.samples as u64 {
            let expected =
                started + Duration::from_micros(config.interval_us.saturating_mul(sequence - 1));
            tokio::time::sleep_until(tokio::time::Instant::from_std(expected)).await;
            let sent = Instant::now();
            for fragment in fragments(session, sequence, started.elapsed().as_micros() as u64)? {
                channel.send_datagram(1, &fragment).await?;
            }
            let deadline = tokio::time::Instant::now() + Duration::from_millis(config.timeout_ms);
            loop {
                let Ok(packet) =
                    tokio::time::timeout_at(deadline, channel.receive_datagram()).await
                else {
                    break;
                };
                let (kind, bytes) = packet?;
                if kind != 1 {
                    return fail("benchmark echo kind mismatch");
                }
                let fragment =
                    media::decode(&bytes).map_err(|_| "benchmark echo fragment invalid")?;
                if fragment.sequence != sequence {
                    continue;
                }
                if let Some(frame) = assembler
                    .push(fragment, started.elapsed())
                    .map_err(|_| "benchmark echo reassembly failed")?
                {
                    if frame != FRAME {
                        return fail("benchmark echo content mismatch");
                    }
                    rtts.record(sent.elapsed().as_micros().min(u64::MAX as u128) as u64);
                    received += 1;
                    break;
                }
            }
        }
        Ok(())
    })
    .await;
    if let Err(error) = result {
        file_task.abort();
        let _ = file_task.await;
        return Err(error);
    }
    let bulk = file_task.await??;
    channel
        .send_reliable(Packet {
            kind: wire::CLOSE,
            payload: vec![],
        })
        .await?;
    let percentiles = rtts.percentiles();
    Ok(Report {
        schema: 1,
        transport,
        scope,
        samples_sent: config.samples,
        frames_echoed: received,
        timed_out: config.samples - received,
        frame_bytes: FRAME.len(),
        frame_sha256: fingerprint(FRAME),
        interval_us: config.interval_us,
        reliable_bytes_echoed: bulk,
        elapsed_ms: started.elapsed().as_secs_f64() * 1000.,
        completed_frame_rtt_p50_us: percentiles.as_ref().map(|p| p.p50_us),
        completed_frame_rtt_p95_us: percentiles.as_ref().map(|p| p.p95_us),
        completed_frame_rtt_p99_us: percentiles.as_ref().map(|p| p.p99_us),
        input_to_display_measured: false,
        display_fps_measured: false,
        hardware_codec_measured: false,
        selected_winner: None,
    })
}

pub async fn loopback(config: Config) -> Result<Vec<Report>> {
    config.validate()?;
    let root = std::env::temp_dir().join(format!("lrd-compare-{}", lrd_session::random_u64()?));
    lrd_session::identity::private_directory(&root)?;
    let host = Arc::new(Identity::load_or_create(&root.join("host"))?);
    let client = Arc::new(Identity::load_or_create(&root.join("client"))?);
    let allow: Ask = Arc::new(|_| true);
    let mut reports = Vec::new();
    for quic in [false, true] {
        let scope = Scope::LoopbackDiagnostics;
        let (client_channel, server_task) = if quic {
            let listener = Arc::new(Listener::bind(scope.clone(), &host)?);
            let address = listener.local_address()?;
            let key = host.clone();
            let ask = allow.clone();
            let server = tokio::spawn(async move {
                let t = listener.accept(&key, ask).await?;
                echo(Arc::new(Channel::Quic(Box::new(t)))).await
            });
            let client = Quic::connect(
                scope,
                address,
                &client,
                Some(host.fingerprint()),
                allow.clone(),
            )
            .await?;
            (Arc::new(Channel::Quic(Box::new(client))), server)
        } else {
            let listener = Arc::new(scope.listen()?);
            let address = listener.local_addr()?;
            let key = host.clone();
            let ask = allow.clone();
            let server_scope = scope.clone();
            let server = tokio::spawn(async move {
                let t = Hybrid::accept(server_scope, listener, key, ask).await?;
                echo(Arc::new(Channel::Hybrid(Box::new(t)))).await
            });
            let client = Hybrid::connect(
                scope,
                address,
                client.clone(),
                Some(host.fingerprint()),
                allow.clone(),
            )
            .await?;
            (Arc::new(Channel::Hybrid(Box::new(client))), server)
        };
        let report = measure(
            client_channel.clone(),
            config.clone(),
            if quic {
                "QUIC DATAGRAM"
            } else {
                "TLS + AES-256-GCM UDP"
            },
            "loopback diagnostics; no physical Ethernet, display or input latency measurement"
                .into(),
        )
        .await;
        match report {
            Ok(report) => reports.push(report),
            Err(error) => {
                server_task.abort();
                client_channel.close();
                return Err(error);
            }
        }
        tokio::time::timeout(Duration::from_secs(6), server_task).await???;
        client_channel.close();
        drop(client_channel);
    }
    drop(host);
    drop(client);
    std::fs::remove_dir_all(root)?;
    Ok(reports)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn both_candidates_complete_the_same_h264_and_reliable_bulk_workload() {
        let reports = tokio::time::timeout(
            Duration::from_secs(15),
            loopback(Config {
                samples: 3,
                interval_us: 10000,
                timeout_ms: 1000,
                bulk_chunks: 4,
            }),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(reports.len(), 2);
        for r in reports {
            assert_eq!(r.frames_echoed, 3);
            assert_eq!(r.reliable_bytes_echoed, 4 * BULK as u64);
            assert!(r.selected_winner.is_none());
        }
    }
}
