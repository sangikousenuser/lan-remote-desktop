//! A one-use local startup acknowledgement. Starting a process is not evidence
//! that the GUI/Host initialized. No keys, screen content or peer data is sent.
use crate::{fail, identity, Result};
use std::{
    io::{Read, Write},
    net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream},
    process::{Child, Command},
    time::{Duration, Instant},
};

pub struct Health {
    listener: TcpListener,
    token: String,
    version: String,
}
impl Health {
    pub fn new(version: &str) -> Result<Self> {
        semver::Version::parse(version)?;
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        listener.set_nonblocking(true)?;
        Ok(Self {
            listener,
            token: format!(
                "{:016x}{:016x}{:016x}{:016x}",
                crate::random_u64()?,
                crate::random_u64()?,
                crate::random_u64()?,
                crate::random_u64()?
            ),
            version: version.into(),
        })
    }
    pub fn configure(&self, command: &mut Command) -> Result<()> {
        command
            .env(
                "LRD_UPDATE_HEALTH_ADDRESS",
                self.listener.local_addr()?.to_string(),
            )
            .env("LRD_UPDATE_HEALTH_TOKEN", &self.token)
            .env("LRD_UPDATE_HEALTH_VERSION", &self.version);
        Ok(())
    }
    /// Keep watching briefly after the acknowledgement to catch immediate exit.
    /// A timeout or failed startup is returned to the transaction owner, which
    /// stops the new child before restoring the previous files.
    pub fn observe(&self, child: &mut Child, timeout: Duration, settle: Duration) -> Result<bool> {
        let deadline = Instant::now() + timeout;
        let mut acknowledged = None;
        while Instant::now() < deadline {
            if child.try_wait()?.is_some() {
                return Ok(false);
            }
            if let Some(at) = acknowledged {
                if Instant::now().duration_since(at) >= settle {
                    return Ok(true);
                }
            } else {
                match self.listener.accept() {
                    Ok((mut stream, address)) if address.ip().is_loopback() => {
                        stream.set_read_timeout(Some(Duration::from_millis(100)))?;
                        let mut message = [0u8; 128];
                        if stream.read_exact(&mut message).is_ok()
                            && message[..64] == *self.token.as_bytes()
                            && message[64..]
                                == *identity::fingerprint(self.version.as_bytes()).as_bytes()
                        {
                            acknowledged = Some(Instant::now());
                        }
                    }
                    Ok(_) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                    Err(e) => return Err(e.into()),
                }
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        Ok(false)
    }
}
/// Call when the native UI or Host initialization is complete. A mismatching
/// version cannot acknowledge another release's startup. No-op on normal runs.
pub fn notify_ready(version: &str) -> Result<()> {
    let Ok(address) = std::env::var("LRD_UPDATE_HEALTH_ADDRESS") else {
        return Ok(());
    };
    let address: SocketAddr = address.parse()?;
    let token = std::env::var("LRD_UPDATE_HEALTH_TOKEN")?;
    if address.ip() != Ipv4Addr::LOCALHOST
        || address.port() == 0
        || token.len() != 64
        || !token.bytes().all(|b| b.is_ascii_hexdigit())
        || std::env::var("LRD_UPDATE_HEALTH_VERSION")?.as_str() != version
    {
        return fail("invalid startup health request");
    }
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(1))?;
    stream.set_write_timeout(Some(Duration::from_secs(1)))?;
    stream.write_all(token.as_bytes())?;
    stream.write_all(identity::fingerprint(version.as_bytes()).as_bytes())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_exact_token_and_version_acknowledge_startup() {
        let health = Health::new("1.0.0").unwrap();
        let mut wrong = TcpStream::connect(health.listener.local_addr().unwrap()).unwrap();
        wrong.write_all(&[b'x'; 128]).unwrap();
        let (mut read, _) = health.listener.accept().unwrap();
        let mut bytes = [0; 128];
        read.read_exact(&mut bytes).unwrap();
        assert_ne!(&bytes[..64], health.token.as_bytes());
        let mut command = Command::new("unused");
        health.configure(&mut command).unwrap();
        assert_eq!(
            command
                .get_envs()
                .filter(|(k, _)| k.to_str().unwrap().starts_with("LRD_UPDATE_HEALTH_"))
                .count(),
            3
        );
    }
    #[test]
    fn real_child_must_initialize_with_the_expected_version_and_stay_alive() {
        for (version, report, expected) in [
            ("1.0.0", true, true),
            ("2.0.0", true, false),
            ("1.0.0", false, false),
        ] {
            let health = Health::new(version).unwrap();
            let mut command = Command::new(std::env::current_exe().unwrap());
            command
                .args([
                    "--ignored",
                    "--exact",
                    "updates::health::tests::health_child_fixture",
                    "--nocapture",
                ])
                .env(
                    "LRD_HEALTH_FIXTURE_REPORT",
                    if report { "yes" } else { "no" },
                )
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null());
            health.configure(&mut command).unwrap();
            let mut child = command.spawn().unwrap();
            assert_eq!(
                health
                    .observe(
                        &mut child,
                        Duration::from_secs(2),
                        Duration::from_millis(100)
                    )
                    .unwrap(),
                expected
            );
            let _ = child.kill();
            let _ = child.wait();
        }
    }
    #[test]
    #[ignore = "subprocess fixture used by the startup observer test"]
    fn health_child_fixture() {
        if std::env::var("LRD_HEALTH_FIXTURE_REPORT").unwrap() == "yes" {
            notify_ready("1.0.0").unwrap();
        }
        std::thread::sleep(Duration::from_secs(10));
    }
}
