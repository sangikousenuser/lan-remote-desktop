use super::*;
use lrd_session::tls;

type Native = (channel::Sender, mpsc::Receiver<Packet>);
fn pair(mode: TransportMode) -> (Native, Native, std::path::PathBuf) {
    let root =
        std::env::temp_dir().join(format!("lrd-live-{}", lrd_session::random_u64().unwrap()));
    let host = Arc::new(Identity::load_or_create(&root.join("host")).unwrap());
    let client = Arc::new(Identity::load_or_create(&root.join("client")).unwrap());
    let listener = Scope::LoopbackDiagnostics.listen().unwrap();
    let address = listener.local_addr().unwrap();
    let pin = host.fingerprint();
    let server = std::thread::spawn(move || {
        let (socket, _) = listener.accept().unwrap();
        let mut tls = Tls::server(socket, &host).unwrap();
        let peer = tls::approve(&mut tls, &host, false, |_| true).unwrap();
        start(tls, Scope::LoopbackDiagnostics, host, false, mode, peer, 1).unwrap()
    });
    let mut tls = Tls::client(
        Scope::LoopbackDiagnostics.connect(address).unwrap(),
        &client,
        Some(pin),
    )
    .unwrap();
    let peer = tls::approve(&mut tls, &client, true, |_| true).unwrap();
    let local = start(tls, Scope::LoopbackDiagnostics, client, true, mode, peer, 1).unwrap();
    (server.join().unwrap(), local, root)
}
fn receive_kind(receiver: &mpsc::Receiver<Packet>, kind: u8) -> Packet {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let packet = receiver
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .unwrap();
        if packet.kind == kind {
            return packet;
        }
    }
}
fn application_roundtrip(mode: TransportMode) {
    let ((host, host_rx), (client, client_rx), root) = pair(mode);
    let frame = include_bytes!("../../../../assets/test-frame.h264");
    host.send_bulk(Packet {
        kind: wire::VIDEO,
        payload: frame.to_vec(),
    })
    .unwrap();
    assert_eq!(receive_kind(&client_rx, wire::VIDEO).payload, frame);
    let audio = vec![0u8; 3840];
    host.send(Packet {
        kind: wire::AUDIO,
        payload: audio.clone(),
    })
    .unwrap();
    assert_eq!(receive_kind(&client_rx, wire::AUDIO).payload, audio);
    // Stateful key release stays on reliable delivery while both directions
    // transfer a large packet; the datagram adapter must not reinterpret it.
    let key = wire::InputPacket {
        epoch: 1,
        sequence: 1,
        input: wire::Input::Release,
    };
    client
        .send(Packet::json(wire::INPUT, &key).unwrap())
        .unwrap();
    let bulk = vec![42; 512 * 1024];
    host.send_bulk(Packet {
        kind: wire::FILE,
        payload: bulk.clone(),
    })
    .unwrap();
    client
        .send_bulk(Packet {
            kind: wire::FILE,
            payload: bulk.clone(),
        })
        .unwrap();
    assert!(matches!(
        receive_kind(&host_rx, wire::INPUT)
            .parse::<wire::InputPacket>()
            .unwrap()
            .input,
        wire::Input::Release
    ));
    assert_eq!(receive_kind(&host_rx, wire::FILE).payload, bulk);
    assert_eq!(receive_kind(&client_rx, wire::FILE).payload, bulk);
    let config = VideoConfig {
        width: 1920,
        height: 1080,
        fps: 60,
        monitor: 0,
        monitors: vec![],
        input_epoch: 2,
        session_id: 1,
        resume_token: "a".repeat(64),
    };
    host.send_bulk(Packet::json(wire::CONFIG, &config).unwrap())
        .unwrap();
    assert_eq!(
        receive_kind(&client_rx, wire::CONFIG)
            .parse::<VideoConfig>()
            .unwrap()
            .input_epoch,
        2
    );
    host.send_bulk(Packet {
        kind: wire::VIDEO,
        payload: frame.to_vec(),
    })
    .unwrap();
    assert_eq!(receive_kind(&client_rx, wire::VIDEO).payload, frame);
    client.close();
    assert_eq!(receive_kind(&host_rx, wire::CLOSE).kind, wire::CLOSE);
    host.abort();
    client.abort();
    std::fs::remove_dir_all(root).unwrap();
}
#[test]
fn hybrid_native_media_control_files_and_manual_close() {
    application_roundtrip(TransportMode::Hybrid);
}
#[test]
fn quic_native_media_control_files_and_manual_close() {
    application_roundtrip(TransportMode::Quic);
}
#[test]
fn tcp_native_adapter_preserves_existing_wire() {
    application_roundtrip(TransportMode::Tcp);
}
