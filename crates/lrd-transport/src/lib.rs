//! Q50 candidates. Loopback is an explicit diagnostics scope, never a fallback
//! when the user's chosen physical Ethernet is unavailable.
pub mod benchmark;
pub mod crypto;
pub mod hybrid;
pub mod live;
pub mod quic;
use lrd_session::{fail, network::Nic, Result};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream, UdpSocket};

pub enum Channel {
    Hybrid(Box<hybrid::Hybrid>),
    Quic(Box<quic::Quic>),
}
impl Channel {
    pub async fn finish(&self) -> Result<()> {
        match self {
            Self::Hybrid(t) => t.finish().await,
            Self::Quic(t) => t.finish().await,
        }
    }
    pub async fn send_datagram(&self, kind: u8, bytes: &[u8]) -> Result<()> {
        match self {
            Self::Hybrid(t) => t.send_datagram(kind, bytes).await,
            Self::Quic(t) => t.send_datagram(kind, bytes).await,
        }
    }
    pub async fn receive_datagram(&self) -> Result<(u8, Vec<u8>)> {
        match self {
            Self::Hybrid(t) => t.receive_datagram().await,
            Self::Quic(t) => t.receive_datagram().await,
        }
    }
    pub async fn send_reliable(&self, packet: lrd_session::wire::Packet) -> Result<()> {
        match self {
            Self::Hybrid(t) => t.send_reliable(packet).await,
            Self::Quic(t) => t.send_reliable(packet).await,
        }
    }
    pub async fn receive_reliable(&self) -> Result<lrd_session::wire::Packet> {
        match self {
            Self::Hybrid(t) => t.receive_reliable().await,
            Self::Quic(t) => t.receive_reliable().await,
        }
    }
    pub fn close(&self) {
        match self {
            Self::Hybrid(t) => t.close(),
            Self::Quic(t) => t.close(),
        }
    }
}

#[derive(Clone)]
pub enum Scope {
    Ethernet(Nic),
    LoopbackDiagnostics,
}
pub(crate) struct LinkWatch(std::sync::Arc<std::sync::atomic::AtomicBool>);
impl Drop for LinkWatch {
    fn drop(&mut self) {
        self.0.store(true, std::sync::atomic::Ordering::Release);
    }
}
impl Scope {
    pub fn address(&self) -> Ipv4Addr {
        match self {
            Self::Ethernet(n) => n.address,
            Self::LoopbackDiagnostics => Ipv4Addr::LOCALHOST,
        }
    }
    pub fn accepts(&self, remote: SocketAddr) -> bool {
        self.current() && self.contains_peer(remote)
    }
    pub fn current(&self) -> bool {
        match self {
            Self::Ethernet(n) => n.current(),
            Self::LoopbackDiagnostics => true,
        }
    }
    pub fn contains_peer(&self, remote: SocketAddr) -> bool {
        match (self, remote.ip()) {
            (Self::Ethernet(n), std::net::IpAddr::V4(ip)) => n.contains(ip),
            (Self::LoopbackDiagnostics, std::net::IpAddr::V4(ip)) => ip == Ipv4Addr::LOCALHOST,
            _ => false,
        }
    }
    pub(crate) fn watch(&self, on_loss: impl FnOnce() + Send + 'static) -> Option<LinkWatch> {
        if matches!(self, Self::LoopbackDiagnostics) {
            return None;
        }
        let stopped = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = stopped.clone();
        let scope = self.clone();
        std::thread::spawn(move || {
            while !flag.load(std::sync::atomic::Ordering::Acquire) {
                if !scope.current() {
                    on_loss();
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(250));
            }
        });
        Some(LinkWatch(stopped))
    }
    pub fn udp(&self) -> Result<UdpSocket> {
        match self {
            Self::Ethernet(n) => n.udp(0),
            Self::LoopbackDiagnostics => Ok(UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))?),
        }
    }
    pub fn listen(&self) -> Result<TcpListener> {
        match self {
            Self::Ethernet(n) => n.listen(0),
            Self::LoopbackDiagnostics => Ok(TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?),
        }
    }
    pub fn connect(&self, remote: SocketAddr) -> Result<TcpStream> {
        if !self.accepts(remote) {
            return fail("transport peer is outside selected scope");
        }
        match self {
            Self::Ethernet(n) => n.connect(remote),
            Self::LoopbackDiagnostics => Ok(TcpStream::connect_timeout(
                &remote,
                std::time::Duration::from_secs(5),
            )?),
        }
    }
    pub fn validate_socket(&self, local: SocketAddr, remote: SocketAddr) -> Result<()> {
        if local.ip() != self.address() || !self.accepts(remote) {
            return fail("transport socket is outside selected scope");
        }
        Ok(())
    }
    pub fn constrain_tcp(&self, stream: &TcpStream) -> Result<()> {
        match self {
            Self::Ethernet(n) => n.constrain_tcp(stream),
            Self::LoopbackDiagnostics => {
                self.validate_socket(stream.local_addr()?, stream.peer_addr()?)
            }
        }
    }
}
