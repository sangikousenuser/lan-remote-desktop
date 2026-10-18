#[cfg(windows)]
mod platform;

#[cfg(windows)]
fn main() {
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
