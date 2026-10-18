//! TLS 1.3 with certificate pinning and an explicit first-pair approval gate.
//! Before approval, callers can only exchange pairing/hello messages.
use crate::{
    fail,
    identity::{fingerprint, Identity},
    Result,
};
use rustls::{
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    crypto::{verify_tls13_signature, CryptoProvider},
    pki_types::{CertificateDer, ServerName, UnixTime},
    server::danger::{ClientCertVerified, ClientCertVerifier},
    ClientConfig, ClientConnection, DigitallySignedStruct, DistinguishedName, ServerConfig,
    ServerConnection, SignatureScheme, StreamOwned,
};
use std::{
    io::{Read, Write},
    net::TcpStream,
    sync::Arc,
    time::Duration,
};

#[derive(Debug)]
struct DeviceVerifier {
    expected: Option<String>,
    provider: CryptoProvider,
}
impl DeviceVerifier {
    fn validate(
        &self,
        certificate: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
    ) -> std::result::Result<(), rustls::Error> {
        if !intermediates.is_empty()
            || certificate.as_ref().len() > 16384
            || certificate.as_ref().is_empty()
        {
            return Err(rustls::Error::General("invalid device certificate".into()));
        }
        if self
            .expected
            .as_ref()
            .is_some_and(|pin| *pin != fingerprint(certificate.as_ref()))
        {
            return Err(rustls::Error::General("paired device key changed".into()));
        }
        // The TLS CertificateVerify signature is checked below. Identity is pinned
        // or established by comparing the channel-bound code at BOTH endpoints.
        Ok(())
    }
    fn verify(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        signed: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(
            message,
            cert,
            signed,
            &self.provider.signature_verification_algorithms,
        )
    }
    fn schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}
impl ServerCertVerifier for DeviceVerifier {
    fn verify_server_cert(
        &self,
        cert: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        _: &ServerName<'_>,
        _: &[u8],
        _: UnixTime,
    ) -> std::result::Result<ServerCertVerified, rustls::Error> {
        self.validate(cert, intermediates)?;
        Ok(ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        _: &[u8],
        _: &CertificateDer<'_>,
        _: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        Err(rustls::Error::General("TLS 1.2 disabled".into()))
    }
    fn verify_tls13_signature(
        &self,
        m: &[u8],
        c: &CertificateDer<'_>,
        d: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        self.verify(m, c, d)
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.schemes()
    }
}
impl ClientCertVerifier for DeviceVerifier {
    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        &[]
    }
    fn verify_client_cert(
        &self,
        cert: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        _: UnixTime,
    ) -> std::result::Result<ClientCertVerified, rustls::Error> {
        self.validate(cert, intermediates)?;
        Ok(ClientCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        _: &[u8],
        _: &CertificateDer<'_>,
        _: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        Err(rustls::Error::General("TLS 1.2 disabled".into()))
    }
    fn verify_tls13_signature(
        &self,
        m: &[u8],
        c: &CertificateDer<'_>,
        d: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        self.verify(m, c, d)
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.schemes()
    }
}

pub enum Tls {
    Client(StreamOwned<ClientConnection, TcpStream>),
    Server(StreamOwned<ServerConnection, TcpStream>),
}
impl Read for Tls {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Self::Client(c) => c.read(bytes),
            Self::Server(s) => s.read(bytes),
        }
    }
}
impl Write for Tls {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        match self {
            Self::Client(c) => c.write(bytes),
            Self::Server(s) => s.write(bytes),
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Self::Client(c) => c.flush(),
            Self::Server(s) => s.flush(),
        }
    }
}
/// Transport configurations authenticate certificate possession; the application
/// must still complete the channel-bound, bilateral approval gate.
pub fn client_configuration(
    identity: &Identity,
    expected_pin: Option<String>,
) -> Result<ClientConfig> {
    let provider = rustls::crypto::ring::default_provider();
    client_configuration_with_provider(identity, expected_pin, provider)
}
pub fn client_configuration_with_provider(
    identity: &Identity,
    expected_pin: Option<String>,
    provider: CryptoProvider,
) -> Result<ClientConfig> {
    Ok(
        ClientConfig::builder_with_provider(Arc::new(provider.clone()))
            .with_protocol_versions(&[&rustls::version::TLS13])?
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(DeviceVerifier {
                expected: expected_pin,
                provider,
            }))
            .with_client_auth_cert(vec![identity.certificate.clone()], identity.key())?,
    )
}
pub fn server_configuration(identity: &Identity) -> Result<ServerConfig> {
    let provider = rustls::crypto::ring::default_provider();
    server_configuration_with_provider(identity, provider)
}
pub fn server_configuration_with_provider(
    identity: &Identity,
    provider: CryptoProvider,
) -> Result<ServerConfig> {
    Ok(
        ServerConfig::builder_with_provider(Arc::new(provider.clone()))
            .with_protocol_versions(&[&rustls::version::TLS13])?
            .with_client_cert_verifier(Arc::new(DeviceVerifier {
                expected: None,
                provider,
            }))
            .with_single_cert(vec![identity.certificate.clone()], identity.key())?,
    )
}
impl Tls {
    pub fn export_media_keys(&self) -> Result<[u8; 72]> {
        Ok(match self {
            Self::Client(s) => {
                s.conn
                    .export_keying_material([0u8; 72], b"EXPORTER-LRD-hybrid-media-v1", None)?
            }
            Self::Server(s) => {
                s.conn
                    .export_keying_material([0u8; 72], b"EXPORTER-LRD-hybrid-media-v1", None)?
            }
        })
    }
    pub fn nonblocking(&mut self) -> std::io::Result<()> {
        self.socket().set_read_timeout(None)?;
        self.socket().set_write_timeout(None)?;
        self.socket().set_nonblocking(true)?;
        match self {
            Self::Client(s) => s.conn.set_buffer_limit(Some(65536)),
            Self::Server(s) => s.conn.set_buffer_limit(Some(65536)),
        };
        Ok(())
    }
    pub fn read_available(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        macro_rules! read {
            ($s:expr) => {{
                let s = $s;
                match s.conn.reader().read(bytes) {
                    Ok(n) => return Ok(n),
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                    Err(e) => return Err(e),
                }
                if s.conn.read_tls(&mut s.sock)? == 0 {
                    return Ok(0);
                }
                s.conn
                    .process_new_packets()
                    .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
                s.conn.reader().read(bytes)
            }};
        }
        match self {
            Self::Client(s) => read!(s),
            Self::Server(s) => read!(s),
        }
    }
    pub fn write_plain(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        match self {
            Self::Client(s) => s.conn.writer().write(bytes),
            Self::Server(s) => s.conn.writer().write(bytes),
        }
    }
    pub fn flush_available(&mut self) -> std::io::Result<usize> {
        match self {
            Self::Client(s) => s.conn.write_tls(&mut s.sock),
            Self::Server(s) => s.conn.write_tls(&mut s.sock),
        }
    }
    pub fn wants_write(&self) -> bool {
        match self {
            Self::Client(s) => s.conn.wants_write(),
            Self::Server(s) => s.conn.wants_write(),
        }
    }
    pub fn client(
        socket: TcpStream,
        identity: &Identity,
        expected_pin: Option<String>,
    ) -> Result<Self> {
        socket.set_read_timeout(Some(Duration::from_secs(60)))?;
        socket.set_write_timeout(Some(Duration::from_secs(10)))?;
        socket.set_nodelay(true)?;
        let config = client_configuration(identity, expected_pin)?;
        let mut connection = ClientConnection::new(
            Arc::new(config),
            ServerName::try_from("lanremoted.local")?.to_owned(),
        )?;
        let mut socket = socket;
        while connection.is_handshaking() {
            connection.complete_io(&mut socket)?;
        }
        Ok(Self::Client(StreamOwned::new(connection, socket)))
    }
    pub fn server(socket: TcpStream, identity: &Identity) -> Result<Self> {
        socket.set_read_timeout(Some(Duration::from_secs(60)))?;
        socket.set_write_timeout(Some(Duration::from_secs(10)))?;
        socket.set_nodelay(true)?;
        let config = server_configuration(identity)?;
        let mut connection = ServerConnection::new(Arc::new(config))?;
        let mut socket = socket;
        while connection.is_handshaking() {
            connection.complete_io(&mut socket)?;
        }
        Ok(Self::Server(StreamOwned::new(connection, socket)))
    }
    pub fn socket(&self) -> &TcpStream {
        match self {
            Self::Client(c) => &c.sock,
            Self::Server(s) => &s.sock,
        }
    }
    pub fn peer_certificate(&self) -> Result<Vec<u8>> {
        let certs = match self {
            Self::Client(c) => c.conn.peer_certificates(),
            Self::Server(s) => s.conn.peer_certificates(),
        }
        .ok_or("peer certificate required")?;
        if certs.len() != 1 {
            return fail("one device certificate is required");
        }
        Ok(certs[0].as_ref().to_vec())
    }
    pub fn pairing_code(&self) -> Result<String> {
        let bytes = match self {
            Self::Client(c) => {
                c.conn
                    .export_keying_material([0u8; 32], b"EXPORTER-LRD-pairing-v1", None)?
            }
            Self::Server(s) => {
                s.conn
                    .export_keying_material([0u8; 32], b"EXPORTER-LRD-pairing-v1", None)?
            }
        };
        Ok(format!(
            "{:06}",
            u32::from_be_bytes(bytes[..4].try_into()?) % 1_000_000
        ))
    }
}

#[derive(Clone, Debug)]
pub struct Approval {
    pub fingerprint: String,
    pub code: String,
    pub first_pair: bool,
}

/// Both peers send their LOCAL decision and wait for the other decision. No
/// remote media API is available until this function succeeds.
pub fn approve(
    tls: &mut Tls,
    identity: &Identity,
    is_client: bool,
    ask: impl FnOnce(&Approval) -> bool,
) -> Result<String> {
    let certificate = tls.peer_certificate()?;
    let first_pair = !identity.trusted(&certificate)?;
    tls.write_all(&[u8::from(first_pair)])?;
    tls.flush()?;
    let mut remote_pair = [0];
    tls.read_exact(&mut remote_pair)?;
    if remote_pair[0] > 1 {
        return fail("invalid pairing state");
    }
    let info = Approval {
        fingerprint: fingerprint(&certificate),
        code: tls.pairing_code()?,
        first_pair: first_pair || remote_pair == [1],
    };
    let allowed = if is_client || info.first_pair {
        ask(&info)
    } else {
        true
    };
    tls.write_all(&[u8::from(allowed)])?;
    tls.flush()?;
    let mut remote = [0];
    tls.read_exact(&mut remote)?;
    if !allowed || remote != [1] {
        return fail("connection rejected by user");
    }
    if first_pair {
        identity.trust(&certificate)?;
    }
    Ok(info.fingerprint)
}
