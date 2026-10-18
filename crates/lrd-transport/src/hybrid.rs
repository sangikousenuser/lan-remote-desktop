//! Candidate B: bilateral TLS approval, reliable TLS control, exporter-keyed UDP.
use crate::{
    crypto::{self, Opener, Sealer},
    Scope,
};
use lrd_session::{
    channel, fail,
    identity::Identity,
    tls::{self, Approval, Tls},
    wire::{self, Packet},
    Result,
};
use std::{
    net::{SocketAddr, TcpListener, UdpSocket},
    sync::{mpsc::Receiver, Arc, Mutex},
    time::Duration,
};
pub type Ask = Arc<dyn Fn(&Approval) -> bool + Send + Sync>;
pub struct Hybrid {
    _watch: Option<crate::LinkWatch>,
    sender: channel::Sender,
    receiver: Arc<Mutex<Receiver<Packet>>>,
    udp: tokio::net::UdpSocket,
    seal: Mutex<Sealer>,
    open: tokio::sync::Mutex<Opener>,
    scope: Scope,
    remote: SocketAddr,
    peer: String,
}
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Hello {
    transport: String,
    port: u16,
}
impl Hybrid {
    pub async fn connect(
        scope: Scope,
        remote: SocketAddr,
        identity: Arc<Identity>,
        pin: Option<String>,
        ask: Ask,
    ) -> Result<Self> {
        let work_scope = scope.clone();
        let (tls, udp, peer) = tokio::task::spawn_blocking(move || -> Result<_> {
            let mut tls = Tls::client(work_scope.connect(remote)?, &identity, pin)?;
            let peer = tls::approve(&mut tls, &identity, true, |info| ask(info))?;
            Ok((tls, work_scope.udp()?, peer))
        })
        .await??;
        Self::establish(tls, udp, scope, peer, true).await
    }
    pub async fn accept(
        scope: Scope,
        listener: Arc<TcpListener>,
        identity: Arc<Identity>,
        ask: Ask,
    ) -> Result<Self> {
        listener.set_nonblocking(true)?;
        let listener = tokio::net::TcpListener::from_std(listener.try_clone()?)?;
        let (socket, _) = listener.accept().await?;
        let socket = socket.into_std()?;
        scope.constrain_tcp(&socket)?;
        socket.set_nonblocking(false)?;
        let work_scope = scope.clone();
        let (tls, udp, peer) = tokio::task::spawn_blocking(move || -> Result<_> {
            let mut tls = Tls::server(socket, &identity)?;
            let peer = tls::approve(&mut tls, &identity, false, |info| ask(info))?;
            Ok((tls, work_scope.udp()?, peer))
        })
        .await??;
        Self::establish(tls, udp, scope, peer, false).await
    }
    async fn establish(
        mut tls: Tls,
        udp: UdpSocket,
        scope: Scope,
        peer: String,
        client: bool,
    ) -> Result<Self> {
        let (sender, receiver, udp, seal, open, remote) =
            tokio::task::spawn_blocking(move || -> Result<_> {
                let mut exporter = tls.export_media_keys()?;
                let (seal, open) = crypto::directional(&exporter, client)?;
                exporter.fill(0);
                Packet::json(
                    wire::CONFIG,
                    &Hello {
                        transport: "tls-aead-udp-v1".into(),
                        port: udp.local_addr()?.port(),
                    },
                )?
                .write(&mut tls)?;
                std::io::Write::flush(&mut tls)?;
                let packet = Packet::read(&mut tls)?;
                if packet.kind != wire::CONFIG {
                    return fail("hybrid negotiation required");
                }
                let hello: Hello = packet.parse()?;
                if hello.transport != "tls-aead-udp-v1" || hello.port == 0 {
                    return fail("invalid hybrid negotiation");
                }
                let remote = SocketAddr::new(tls.socket().peer_addr()?.ip(), hello.port);
                if udp.local_addr()?.ip() != tls.socket().local_addr()?.ip() {
                    return fail("UDP/TLS interface mismatch");
                }
                udp.connect(remote)?;
                udp.set_nonblocking(true)?;
                let (sender, receiver) = channel::start(tls)?;
                Ok((sender, receiver, udp, seal, open, remote))
            })
            .await??;
        let link_sender = sender.clone();
        let watch = scope.watch(move || link_sender.abort());
        Ok(Self {
            _watch: watch,
            sender,
            receiver: Arc::new(Mutex::new(receiver)),
            udp: tokio::net::UdpSocket::from_std(udp)?,
            seal: Mutex::new(seal),
            open: tokio::sync::Mutex::new(open),
            scope,
            remote,
            peer,
        })
    }
    fn current(&self) -> Result<()> {
        if self.sender.stopped() || !self.scope.contains_peer(self.remote) {
            return fail("hybrid session closed or NIC changed");
        }
        Ok(())
    }
    pub fn peer_fingerprint(&self) -> &str {
        &self.peer
    }
    pub async fn send_datagram(&self, kind: u8, payload: &[u8]) -> Result<()> {
        self.current()?;
        let bytes = self
            .seal
            .lock()
            .map_err(|_| "media sender unavailable")?
            .seal(kind, payload)?;
        if self.udp.send(&bytes).await? != bytes.len() {
            return fail("partial UDP send");
        }
        Ok(())
    }
    pub async fn receive_datagram(&self) -> Result<(u8, Vec<u8>)> {
        self.current()?;
        let mut opener = self.open.lock().await;
        let mut bytes = [0u8; crypto::MAX_DATAGRAM + 1];
        for _ in 0..32 {
            let n = self.udp.recv(&mut bytes).await?;
            self.current()?;
            if let Ok(message) = opener.open(&bytes[..n]) {
                return Ok(message);
            }
        }
        fail("invalid datagram rate limit")
    }
    pub async fn send_reliable(&self, packet: Packet) -> Result<()> {
        self.current()?;
        let sender = self.sender.clone();
        tokio::task::spawn_blocking(move || sender.send_bulk(packet)).await?
    }
    pub async fn receive_reliable(&self) -> Result<Packet> {
        if !self.scope.contains_peer(self.remote) {
            return fail("hybrid NIC changed");
        }
        let receiver = self.receiver.clone();
        tokio::task::spawn_blocking(move || -> Result<Packet> {
            Ok(receiver
                .lock()
                .map_err(|_| "control receiver unavailable")?
                .recv_timeout(Duration::from_secs(5))?)
        })
        .await?
    }
    pub fn close(&self) {
        self.sender.close();
    }
}
impl Drop for Hybrid {
    fn drop(&mut self) {
        self.sender.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn approved_tls_binds_bidirectional_udp_and_reliable_control() {
        let root =
            std::env::temp_dir().join(format!("lrd-hybrid-{}", lrd_session::random_u64().unwrap()));
        let host = Arc::new(Identity::load_or_create(&root.join("host")).unwrap());
        let client = Arc::new(Identity::load_or_create(&root.join("client")).unwrap());
        let scope = Scope::LoopbackDiagnostics;
        let listener = Arc::new(scope.listen().unwrap());
        let address = listener.local_addr().unwrap();
        let pin = host.fingerprint();
        let (codes_tx, codes_rx) = std::sync::mpsc::channel();
        let codes_rx = std::sync::Mutex::new(codes_rx);
        let host_task = tokio::spawn(Hybrid::accept(
            scope.clone(),
            listener,
            host,
            Arc::new(move |info| {
                codes_tx.send(info.code.clone()).unwrap();
                true
            }),
        ));
        let caller = Hybrid::connect(
            scope,
            address,
            client,
            Some(pin),
            Arc::new(move |info| {
                assert_eq!(
                    codes_rx
                        .lock()
                        .unwrap()
                        .recv_timeout(Duration::from_secs(3))
                        .unwrap(),
                    info.code
                );
                true
            }),
        )
        .await
        .unwrap();
        let server = host_task.await.unwrap().unwrap();
        caller.send_datagram(1, b"encoded frame").await.unwrap();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(3), server.receive_datagram())
                .await
                .unwrap()
                .unwrap()
                .1,
            b"encoded frame"
        );
        server.send_datagram(3, b"input state").await.unwrap();
        assert_eq!(caller.receive_datagram().await.unwrap().0, 3);
        caller
            .send_reliable(Packet::json(wire::CONFIG, &42).unwrap())
            .await
            .unwrap();
        assert_eq!(
            server
                .receive_reliable()
                .await
                .unwrap()
                .parse::<u64>()
                .unwrap(),
            42
        );
        drop(caller);
        drop(server);
        std::fs::remove_dir_all(root).unwrap();
    }
}
