//! Candidate A: mutual TLS 1.3, explicit pairing, QUIC DATAGRAM + reliable stream.
use crate::{crypto::MAX_PAYLOAD, hybrid::Ask, Scope};
use lrd_session::{
    fail,
    identity::{fingerprint, Identity},
    tls::{self, Approval},
    wire::{self, Packet},
    Result,
};
use quinn::{
    crypto::rustls::{QuicClientConfig, QuicServerConfig},
    Connection, Endpoint, RecvStream, SendStream, TransportConfig, VarInt,
};
use std::{net::SocketAddr, sync::Arc, time::Duration};
pub struct Quic {
    _watch: Option<crate::LinkWatch>,
    endpoint: Endpoint,
    connection: Connection,
    send: tokio::sync::Mutex<SendStream>,
    receive: tokio::sync::Mutex<RecvStream>,
    scope: Scope,
    peer: String,
}
pub struct Listener {
    endpoint: Endpoint,
    scope: Scope,
}
fn media_provider() -> quinn::rustls::crypto::CryptoProvider {
    let mut provider = quinn::rustls::crypto::ring::default_provider();
    provider.cipher_suites =
        vec![quinn::rustls::crypto::ring::cipher_suite::TLS13_AES_256_GCM_SHA384];
    provider
}
fn initial_suite() -> Result<quinn::rustls::quic::Suite> {
    quinn::rustls::crypto::ring::cipher_suite::TLS13_AES_128_GCM_SHA256
        .tls13()
        .and_then(|suite| suite.quic_suite())
        .ok_or_else(|| std::io::Error::other("QUIC Initial cipher unavailable").into())
}
fn transport() -> Result<Arc<TransportConfig>> {
    let mut config = TransportConfig::default();
    config
        .max_concurrent_bidi_streams(VarInt::from_u32(1))
        .max_concurrent_uni_streams(VarInt::from_u32(0))
        .datagram_receive_buffer_size(Some(1024 * 1024))
        .datagram_send_buffer_size(1024 * 1024)
        .max_idle_timeout(Some(Duration::from_secs(5).try_into()?))
        .keep_alive_interval(Some(Duration::from_secs(1)))
        .initial_mtu(1200)
        .min_mtu(1200)
        .mtu_discovery_config(None)
        .stream_receive_window(VarInt::from_u32(8 * 1024 * 1024))
        .receive_window(VarInt::from_u32(8 * 1024 * 1024));
    Ok(Arc::new(config))
}
impl Listener {
    pub fn bind(scope: Scope, identity: &Identity) -> Result<Self> {
        let mut tls = tls::server_configuration_with_provider(identity, media_provider())?;
        tls.alpn_protocols = vec![b"lrd-q50-v1".to_vec()];
        tls.max_early_data_size = 0;
        let mut config = quinn::ServerConfig::with_crypto(Arc::new(
            QuicServerConfig::with_initial(Arc::new(tls), initial_suite()?)?,
        ));
        config.transport_config(transport()?);
        config.migration(false);
        let socket = scope.udp()?;
        socket.set_nonblocking(true)?;
        let endpoint = Endpoint::new(
            quinn::EndpointConfig::default(),
            Some(config),
            socket,
            Arc::new(quinn::TokioRuntime),
        )?;
        Ok(Self { endpoint, scope })
    }
    pub fn local_address(&self) -> Result<SocketAddr> {
        Ok(self.endpoint.local_addr()?)
    }
    pub async fn accept(&self, identity: &Identity, ask: Ask) -> Result<Quic> {
        loop {
            let incoming = self.endpoint.accept().await.ok_or("QUIC listener closed")?;
            if !self.scope.accepts(incoming.remote_address()) {
                incoming.refuse();
                continue;
            }
            let connection = tokio::time::timeout(Duration::from_secs(10), incoming).await??;
            self.scope
                .validate_socket(self.endpoint.local_addr()?, connection.remote_address())?;
            let (send, receive) =
                tokio::time::timeout(Duration::from_secs(10), connection.accept_bi()).await??;
            return Quic::approve(
                self.endpoint.clone(),
                connection,
                send,
                receive,
                self.scope.clone(),
                identity,
                false,
                ask,
            )
            .await;
        }
    }
}
impl Quic {
    pub async fn connect(
        scope: Scope,
        remote: SocketAddr,
        identity: &Identity,
        pin: Option<String>,
        ask: Ask,
    ) -> Result<Self> {
        if !scope.accepts(remote) {
            return fail("QUIC peer outside selected Ethernet scope");
        }
        let mut tls = tls::client_configuration_with_provider(identity, pin, media_provider())?;
        tls.alpn_protocols = vec![b"lrd-q50-v1".to_vec()];
        tls.enable_early_data = false;
        let mut config = quinn::ClientConfig::new(Arc::new(QuicClientConfig::with_initial(
            Arc::new(tls),
            initial_suite()?,
        )?));
        config.transport_config(transport()?);
        let socket = scope.udp()?;
        socket.set_nonblocking(true)?;
        let mut endpoint = Endpoint::new(
            quinn::EndpointConfig::default(),
            None,
            socket,
            Arc::new(quinn::TokioRuntime),
        )?;
        endpoint.set_default_client_config(config);
        let connection = tokio::time::timeout(
            Duration::from_secs(10),
            endpoint.connect(remote, "lanremoted.local")?,
        )
        .await??;
        scope.validate_socket(endpoint.local_addr()?, connection.remote_address())?;
        let (send, receive) = connection.open_bi().await?;
        Self::approve(
            endpoint, connection, send, receive, scope, identity, true, ask,
        )
        .await
    }
    #[allow(clippy::too_many_arguments)]
    async fn approve(
        endpoint: Endpoint,
        connection: Connection,
        mut send: SendStream,
        mut receive: RecvStream,
        scope: Scope,
        identity: &Identity,
        client: bool,
        ask: Ask,
    ) -> Result<Self> {
        let peer = connection
            .peer_identity()
            .ok_or("QUIC peer identity missing")?
            .downcast::<Vec<quinn::rustls::pki_types::CertificateDer<'static>>>()
            .map_err(|_| "unexpected QUIC certificate identity")?;
        if peer.len() != 1 {
            return fail("one QUIC device certificate required");
        }
        let certificate = peer[0].as_ref();
        let first = !identity.trusted(certificate)?;
        send.write_all(&[u8::from(first)]).await?;
        let mut remote = [0];
        tokio::time::timeout(Duration::from_secs(60), receive.read_exact(&mut remote)).await??;
        if remote[0] > 1 {
            return fail("invalid QUIC pairing state");
        }
        let mut exporter = [0; 32];
        connection
            .export_keying_material(&mut exporter, b"EXPORTER-LRD-pairing-v1", b"")
            .map_err(|_| std::io::Error::other("QUIC exporter unavailable"))?;
        let info = Approval {
            fingerprint: fingerprint(certificate),
            code: format!(
                "{:06}",
                u32::from_be_bytes(exporter[..4].try_into()?) % 1_000_000
            ),
            first_pair: first || remote == [1],
        };
        let allowed = if client || info.first_pair {
            let ask_info = info.clone();
            tokio::task::spawn_blocking(move || ask(&ask_info)).await?
        } else {
            true
        };
        send.write_all(&[u8::from(allowed)]).await?;
        tokio::time::timeout(Duration::from_secs(60), receive.read_exact(&mut remote)).await??;
        if !allowed || remote != [1] {
            connection.close(VarInt::from_u32(1), b"approval rejected");
            return fail("QUIC connection rejected by user");
        }
        if first {
            identity.trust(certificate)?;
        }
        let watch_connection = connection.clone();
        let watch =
            scope.watch(move || watch_connection.close(VarInt::from_u32(2), b"Ethernet changed"));
        Ok(Self {
            _watch: watch,
            endpoint,
            connection,
            send: tokio::sync::Mutex::new(send),
            receive: tokio::sync::Mutex::new(receive),
            scope,
            peer: info.fingerprint,
        })
    }
    fn current(&self) -> Result<()> {
        if !self.scope.contains_peer(self.connection.remote_address())
            || self.connection.close_reason().is_some()
        {
            return fail("QUIC session closed or NIC changed");
        }
        Ok(())
    }
    pub fn peer_fingerprint(&self) -> &str {
        &self.peer
    }
    pub async fn send_datagram(&self, kind: u8, payload: &[u8]) -> Result<()> {
        self.current()?;
        if !matches!(kind, 1..=3) || payload.is_empty() || payload.len() > MAX_PAYLOAD {
            return fail("QUIC media kind/size limit");
        }
        let mut bytes = vec![kind];
        bytes.extend(payload);
        self.connection.send_datagram(bytes.into())?;
        Ok(())
    }
    pub async fn receive_datagram(&self) -> Result<(u8, Vec<u8>)> {
        self.current()?;
        let bytes = self.connection.read_datagram().await?;
        self.current()?;
        if bytes.len() < 2 || bytes.len() > MAX_PAYLOAD + 1 || !matches!(bytes[0], 1..=3) {
            return fail("invalid QUIC media datagram");
        }
        Ok((bytes[0], bytes[1..].to_vec()))
    }
    pub async fn send_reliable(&self, packet: Packet) -> Result<()> {
        self.current()?;
        let mut bytes = Vec::new();
        packet.write(&mut bytes)?;
        self.send.lock().await.write_all(&bytes).await?;
        Ok(())
    }
    pub async fn receive_reliable(&self) -> Result<Packet> {
        self.current()?;
        let mut stream = self.receive.lock().await;
        let mut prefix = [0; 6];
        stream.read_exact(&mut prefix).await?;
        let size = u32::from_be_bytes(prefix[2..6].try_into()?) as usize;
        if size > wire::MAX_PACKET || prefix[0] != 1 {
            return fail("QUIC control frame size/version limit");
        }
        let mut bytes = prefix.to_vec();
        bytes.resize(6 + size, 0);
        stream.read_exact(&mut bytes[6..]).await?;
        Packet::read(&mut &bytes[..])
    }
    pub fn close(&self) {
        self.connection.close(VarInt::from_u32(0), b"closed");
    }
}
impl Drop for Quic {
    fn drop(&mut self) {
        self.close();
        let _ = &self.endpoint;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn approved_quic_has_matching_codes_and_both_delivery_modes() {
        let root =
            std::env::temp_dir().join(format!("lrd-quic-{}", lrd_session::random_u64().unwrap()));
        let host = Arc::new(Identity::load_or_create(&root.join("host")).unwrap());
        let client = Identity::load_or_create(&root.join("client")).unwrap();
        let scope = Scope::LoopbackDiagnostics;
        let listener = Arc::new(Listener::bind(scope.clone(), &host).unwrap());
        let address = listener.local_address().unwrap();
        let pin = host.fingerprint();
        let (codes_tx, codes_rx) = std::sync::mpsc::channel();
        let codes_rx = std::sync::Mutex::new(codes_rx);
        let remote_listener = listener.clone();
        let host_key = host.clone();
        let task = tokio::spawn(async move {
            remote_listener
                .accept(
                    &host_key,
                    Arc::new(move |info| {
                        codes_tx.send(info.code.clone()).unwrap();
                        true
                    }),
                )
                .await
        });
        let caller = Quic::connect(
            scope,
            address,
            &client,
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
        let server = task.await.unwrap().unwrap();
        caller.send_datagram(1, b"video").await.unwrap();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(3), server.receive_datagram())
                .await
                .unwrap()
                .unwrap()
                .1,
            b"video"
        );
        server.send_datagram(3, b"input").await.unwrap();
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
        drop(listener);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn one_sided_quic_rejection_never_persists_a_peer() {
        let root = std::env::temp_dir().join(format!(
            "lrd-quic-denial-{}",
            lrd_session::random_u64().unwrap()
        ));
        let host = Arc::new(Identity::load_or_create(&root.join("host")).unwrap());
        let client = Identity::load_or_create(&root.join("client")).unwrap();
        let scope = Scope::LoopbackDiagnostics;
        let listener = Arc::new(Listener::bind(scope.clone(), &host).unwrap());
        let address = listener.local_address().unwrap();
        let pin = host.fingerprint();
        let remote_listener = listener.clone();
        let host_key = host.clone();
        let task =
            tokio::spawn(
                async move { remote_listener.accept(&host_key, Arc::new(|_| false)).await },
            );
        assert!(
            Quic::connect(scope, address, &client, Some(pin), Arc::new(|_| true))
                .await
                .is_err()
        );
        assert!(task.await.unwrap().is_err());
        assert!(!host.trusted(client.certificate.as_ref()).unwrap());
        assert!(!client.trusted(host.certificate.as_ref()).unwrap());
        drop(listener);
        std::fs::remove_dir_all(root).unwrap();
    }
}
