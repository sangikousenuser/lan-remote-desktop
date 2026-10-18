#[cfg(windows)]
mod platform;

#[cfg(windows)]
fn main() {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if (args.is_empty() || args.iter().any(|a| a == "--serve"))
        && matches!(
            lrd_session::updates::service::bootstrap(env!("CARGO_PKG_VERSION")),
            Ok(true)
        )
    {
        return;
    }
    if let Err(error) = platform::run() {
        eprintln!("Host起動・接続エラー: {error}");
        std::process::exit(1);
    }
}

#[cfg(not(windows))]
fn main() {
    eprintln!("Windows Host requires a logged-in Windows desktop.");
    std::process::exit(1);
}
