//! A local, OS-authorized, one-shot lease. No IPC listener or stored grant.
#[cfg(any(target_os = "macos", windows, test))]
mod lease;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(windows)]
mod windows;
use lrd_session::{fail, Result};
#[cfg(target_os = "macos")]
use macos as platform;
#[cfg(windows)]
use windows as platform;

#[derive(Debug, PartialEq, Eq)]
enum Request {
    List,
    Apply { index: u32, mac: String },
    Restore,
    Recover,
    Status,
}
fn parse(args: &[String]) -> Result<Request> {
    match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["--interfaces"] => Ok(Request::List),
        ["--restore"] => Ok(Request::Restore),
        ["--recover"] => Ok(Request::Recover),
        ["--status"] => Ok(Request::Status),
        ["--apply", "--interface", index, "--mac", mac] => {
            let index: u32 = index.parse()?;
            if index == 0
                || mac.len() != 17
                || !mac.bytes().enumerate().all(|(i, b)| {
                    if i % 3 == 2 {
                        b == b':'
                    } else {
                        b.is_ascii_hexdigit()
                    }
                })
            {
                return fail("invalid Ethernet identity");
            }
            Ok(Request::Apply {
                index,
                mac: mac.to_ascii_lowercase(),
            })
        }
        _ => fail("use --interfaces, --status, --apply --interface INDEX --mac MAC, --restore or --recover"),
    }
}
fn run() -> Result<()> {
    let request = parse(&std::env::args().skip(1).collect::<Vec<_>>())?;
    if request == Request::List {
        println!(
            "{}",
            serde_json::to_string(&lrd_session::network::ethernet_adapters())?
        );
        return Ok(());
    }
    #[cfg(any(target_os = "macos", windows))]
    {
        if request == Request::Status {
            println!("{}", serde_json::to_string(&platform::read_status()?)?);
            return Ok(());
        }
        platform::authorized()?;
        let store = platform::Store::open()?;
        if request == Request::Restore {
            // The lease owner polls this separate privileged flag. A new
            // invocation cannot race its journal writes or steal its OS lock.
            store.revoke()?;
            if let Some(_guard) = platform::lock()? {
                return lease::recover(&store, &platform::Network);
            }
            return Ok(());
        }
        let _guard = platform::lock()?.ok_or("another IP lease is running; restore it first")?;
        if let Err(error) = lease::recover(&store, &platform::Network) {
            lease::Storage::status(&store, lease::Phase::Deferred)?;
            return Err(error);
        }
        if request == Request::Recover {
            return Ok(());
        }
        let Request::Apply { index, mac } = request else {
            unreachable!()
        };
        let adapter = lrd_session::network::ethernet_adapters()
            .into_iter()
            .find(|a| a.index == index && a.mac.eq_ignore_ascii_case(&mac))
            .ok_or("selected physical Ethernet identity changed")?;
        lease::run(&store, &platform::Network, adapter, platform::ADDRESS)
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    fail("IP configuration is supported only by the Windows/macOS local wizard")
}
fn main() {
    let result = run();
    #[cfg(windows)]
    platform::finished();
    if let Err(error) = result {
        eprintln!("Ethernet IP helper: {error}");
        std::process::exit(1);
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|v| v.to_string()).collect()
    }
    #[test]
    fn requests_are_closed_and_have_no_addresses_paths_or_commands() {
        assert_eq!(parse(&args(&["--restore"])).unwrap(), Request::Restore);
        assert!(parse(&args(&[
            "--apply",
            "--interface",
            "1",
            "--mac",
            "02:ab:cd:00:12:ef"
        ]))
        .is_ok());
        for values in [
            vec!["--apply", "--address", "192.168.1.1"],
            vec!["--restore", "x"],
            vec!["--apply", "--interface", "0", "--mac", "02:ab:cd:00:12:ef"],
            vec!["--apply", "--interface", "1", "--mac", "xx:ab:cd:00:12:ef"],
        ] {
            assert!(parse(&args(&values)).is_err());
        }
    }
}
