use lrd_session::identity::Identity;
use lrd_transport::{
    benchmark::{self, Config},
    hybrid::{Ask, Hybrid},
    quic::{Listener, Quic},
    Channel, Scope,
};
use std::sync::Arc;
async fn run() -> lrd_session::Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let mode = args.first().map(String::as_str).unwrap_or("");
    if mode == "--interfaces" {
        for n in lrd_session::network::interfaces() {
            println!("{}  {}  {}/{}", n.index, n.label, n.address, n.prefix);
        }
        return Ok(());
    }
    if !matches!(mode, "--loopback" | "--listen" | "--connect") {
        return lrd_session::fail(
            "usage: transport-compare --loopback [options] | --interfaces | --listen --interface INDEX --transport quic|hybrid | --connect IP:PORT --interface INDEX --transport quic|hybrid [options]\noptions: --samples N --interval-us N --timeout-ms N --bulk-chunks N --output JSON",
        );
    }
    let mut config = Config::default();
    let mut output = None;
    let mut index = None;
    let mut transport = None;
    let remote = if mode == "--connect" {
        Some(
            args.get(1)
                .ok_or("IP:PORT required")?
                .parse::<std::net::SocketAddr>()?,
        )
    } else {
        None
    };
    let mut i = if remote.is_some() { 2 } else { 1 };
    while i < args.len() {
        let value = args.get(i + 1).ok_or("option needs a value")?;
        match args[i].as_str() {
            "--samples" => config.samples = value.parse()?,
            "--interval-us" => config.interval_us = value.parse()?,
            "--timeout-ms" => config.timeout_ms = value.parse()?,
            "--bulk-chunks" => config.bulk_chunks = value.parse()?,
            "--interface" => index = Some(value.parse::<u32>()?),
            "--transport" => transport = Some(value.as_str()),
            "--output" => output = Some(value),
            _ => return lrd_session::fail("unknown comparison option"),
        };
        i += 2;
    }
    config.validate()?;
    let reports = if mode == "--loopback" {
        if index.is_some() || transport.is_some() {
            return lrd_session::fail(
                "loopback always compares both candidates without an Ethernet interface",
            );
        }
        benchmark::loopback(config).await?
    } else {
        let quic = match transport {
            Some("quic") => true,
            Some("hybrid") => false,
            _ => return lrd_session::fail("select --transport quic or hybrid"),
        };
        let scope = Scope::Ethernet(lrd_session::network::Nic::select(
            index.ok_or("select --interface INDEX")?,
        )?);
        let identity = Arc::new(Identity::load_or_create(
            &lrd_session::identity::data_directory()?.join("transport-bench"),
        )?);
        let ask: Ask = Arc::new(|info| {
            println!("確認コード: {} / 初回: {}\n相手側の表示と一致した場合だけ yes を入力してください。",info.code,info.first_pair);
            let mut line = String::new();
            std::io::stdin().read_line(&mut line).is_ok() && line.trim() == "yes"
        });
        let channel = if let Some(address) = remote {
            if quic {
                Channel::Quic(Box::new(
                    Quic::connect(scope, address, &identity, None, ask).await?,
                ))
            } else {
                Channel::Hybrid(Box::new(
                    Hybrid::connect(scope, address, identity, None, ask).await?,
                ))
            }
        } else if quic {
            let listener = Listener::bind(scope, &identity)?;
            println!("QUIC benchmark listen: {}", listener.local_address()?);
            Channel::Quic(Box::new(listener.accept(&identity, ask).await?))
        } else {
            let listener = Arc::new(scope.listen()?);
            println!("Hybrid benchmark listen: {}", listener.local_addr()?);
            Channel::Hybrid(Box::new(
                Hybrid::accept(scope, listener, identity, ask).await?,
            ))
        };
        let channel = Arc::new(channel);
        if mode == "--listen" {
            benchmark::echo(channel.clone()).await?;
            channel.close();
            return Ok(());
        }
        let report = benchmark::measure(
            channel.clone(),
            config,
            if quic {
                "QUIC DATAGRAM"
            } else {
                "TLS + AES-256-GCM UDP"
            },
            "selected physical Ethernet; H.264 echo RTT, no display/input measurement".into(),
        )
        .await?;
        // Give the reliable close frame a chance to leave before endpoint drop.
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        channel.close();
        vec![report]
    };
    let bytes = serde_json::to_vec_pretty(&reports)?;
    if let Some(path) = output {
        std::fs::write(path, &bytes)?;
    }
    println!("{}", String::from_utf8(bytes)?);
    Ok(())
}
#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("Transport comparison: {error}");
        std::process::exit(1);
    }
}
