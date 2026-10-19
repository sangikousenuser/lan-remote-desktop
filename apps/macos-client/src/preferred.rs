//! Ranking hints are private preferences, never proof of a discovered identity.
use lrd_session::{identity, network::Advertisement, Result};
use std::{net::SocketAddr, path::Path};
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Preferred {
    fingerprint: String,
    user_session: u32,
    nic_mac: String,
}
impl Preferred {
    fn valid(&self) -> bool {
        self.fingerprint.len() == 64
            && self.fingerprint.bytes().all(|b| b.is_ascii_hexdigit())
            && !self.nic_mac.is_empty()
            && self.nic_mac.len() <= 32
    }
    pub fn matches(&self, nic: &str, host: &Advertisement) -> bool {
        self.valid()
            && self.nic_mac == nic
            && self.fingerprint == host.fingerprint
            && self.user_session == host.user_session
    }
}
pub fn load() -> Option<Preferred> {
    let path = identity::data_directory().ok()?.join("last-host.json");
    let preferred: Preferred =
        serde_json::from_slice(&identity::read_private(&path, 4096).ok()?).ok()?;
    preferred.valid().then_some(preferred)
}
pub fn remember(fingerprint: String, user_session: u32, nic_mac: String) -> Result<()> {
    save(
        &identity::data_directory()?,
        &Preferred {
            fingerprint,
            user_session,
            nic_mac,
        },
    )
}
fn save(root: &Path, preferred: &Preferred) -> Result<()> {
    if !preferred.valid() {
        return lrd_session::fail("invalid preferred host");
    }
    identity::private_directory(root)?;
    let path = root.join("last-host.json");
    let temporary = root.join(format!("last-host-{}.tmp", lrd_session::random_u64()?));
    identity::write_private_new(&temporary, &serde_json::to_vec(preferred)?)?;
    std::fs::rename(temporary, path)?;
    Ok(())
}
pub fn prioritize(
    hosts: &mut [(SocketAddr, Advertisement)],
    nic: &str,
    preferred: Option<&Preferred>,
    known: &[String],
) {
    hosts.sort_by_key(|(_, host)| {
        let fallback = if known.contains(&host.fingerprint) {
            2
        } else {
            3
        };
        preferred.map_or(fallback, |last| {
            if last.matches(nic, host) {
                0
            } else if last.nic_mac == nic && last.fingerprint == host.fingerprint {
                1
            } else {
                fallback
            }
        })
    });
}
pub fn known() -> Vec<String> {
    identity::data_directory()
        .and_then(|root| identity::Identity::load_or_create(&root))
        .and_then(|identity| identity.paired_fingerprints())
        .unwrap_or_default()
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn saved_preference_prioritizes_exact_key_and_login_without_trusting_discovery() {
        let root = std::env::temp_dir().join(format!(
            "lrd-preferred-{}",
            lrd_session::random_u64().unwrap()
        ));
        let preferred = Preferred {
            fingerprint: "a".repeat(64),
            user_session: 7,
            nic_mac: "ethernet".into(),
        };
        save(&root, &preferred).unwrap();
        save(&root, &preferred).unwrap();
        let saved: Preferred = serde_json::from_slice(
            &identity::read_private(&root.join("last-host.json"), 4096).unwrap(),
        )
        .unwrap();
        let ad = |key: &str, session| Advertisement {
            major: 1,
            name: "untrusted".into(),
            fingerprint: key.repeat(64),
            user_session: session,
            port: 47990,
            nonce: 1,
        };
        let endpoint = "127.0.0.1:47990".parse().unwrap();
        let mut hosts = vec![
            (endpoint, ad("c", 7)),
            (endpoint, ad("b", 7)),
            (endpoint, ad("a", 8)),
            (endpoint, ad("a", 7)),
        ];
        prioritize(&mut hosts, "ethernet", Some(&saved), &["b".repeat(64)]);
        assert!(saved.matches("ethernet", &hosts[0].1));
        assert!(!saved.matches("other-nic", &hosts[0].1));
        assert_eq!(hosts[1].1.user_session, 8);
        assert_eq!(hosts[2].1.fingerprint, "b".repeat(64));
        assert_eq!(hosts[3].1.fingerprint, "c".repeat(64));
        std::fs::remove_dir_all(root).unwrap();
    }
}
