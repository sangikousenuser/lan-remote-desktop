use lrd_session::{fail, network::Adapter, Result};
use serde::{Deserialize, Serialize};
use std::{
    net::Ipv4Addr,
    time::{Duration, Instant},
};
pub const PREFIX: u8 = 30;
pub const WINDOWS_ADDRESS: Ipv4Addr = Ipv4Addr::new(10, 254, 240, 1);
pub const MAC_ADDRESS: Ipv4Addr = Ipv4Addr::new(10, 254, 240, 2);
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
pub enum Phase {
    #[default]
    Idle,
    Starting,
    Active,
    Restoring,
    Deferred,
    Failed,
}
#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Status {
    pub phase: Phase,
    pub updated: u64,
}
impl Status {
    pub fn new(phase: Phase) -> Self {
        Self {
            phase,
            updated: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    schema: u8,
    pub index: u32,
    pub name: String,
    pub mac: String,
    pub original: Vec<(Ipv4Addr, u8)>,
    pub added: Ipv4Addr,
}
pub trait Storage {
    fn status(&self, _phase: Phase) -> Result<()> {
        Ok(())
    }
    fn load(&self) -> Result<Option<Snapshot>>;
    fn save(&self, value: Option<&Snapshot>) -> Result<()>;
    fn revoked(&self) -> Result<bool>;
    fn clear_revoke(&self) -> Result<()>;
}
pub trait Backend {
    fn cancelled(&self) -> bool {
        false
    }
    fn inventory(&self) -> Result<Vec<Adapter>>;
    /// Reject intersecting non-default routes/other interfaces. Probe the
    /// proposed address on this Ethernet; DAD must finish before success.
    fn preflight(&self, adapter: &Adapter, address: Ipv4Addr) -> Result<()>;
    fn add(&self, snapshot: &Snapshot) -> Result<()>;
    fn remove(&self, snapshot: &Snapshot) -> Result<()>;
}
pub fn overlaps(address: Ipv4Addr, prefix: u8) -> bool {
    if prefix == 0 || prefix > 32 {
        return false;
    }
    let mask = u32::MAX << (32 - prefix.min(PREFIX));
    u32::from(address) & mask == u32::from(WINDOWS_ADDRESS) & mask
}
fn selected(snapshot: &Snapshot, inventory: Vec<Adapter>) -> Result<Option<Adapter>> {
    let by_mac: Vec<_> = inventory
        .into_iter()
        .filter(|a| a.mac.eq_ignore_ascii_case(&snapshot.mac))
        .collect();
    if by_mac.len() > 1 {
        return fail("ambiguous physical Ethernet identity; manual recovery required");
    }
    Ok(by_mac.into_iter().next())
}
fn validate(snapshot: &Snapshot) -> Result<()> {
    if snapshot.schema != 1
        || snapshot.index == 0
        || snapshot.name.len() > 256
        || snapshot.original.len() > 64
        || snapshot.mac.len() != 17
        || !matches!(snapshot.added, WINDOWS_ADDRESS | MAC_ADDRESS)
        || snapshot
            .original
            .iter()
            .any(|(a, p)| *p > 32 || *a == snapshot.added)
    {
        return fail("invalid privileged IP snapshot");
    }
    Ok(())
}
pub fn recover(store: &impl Storage, backend: &impl Backend) -> Result<()> {
    let Some(mut snapshot) = store.load()? else {
        store.clear_revoke()?;
        return store.status(Phase::Idle);
    };
    store.status(Phase::Restoring)?;
    validate(&snapshot)?;
    let Some(adapter) = selected(&snapshot, backend.inventory()?)? else {
        return fail("recorded Ethernet device is absent; keep snapshot until it returns");
    };
    if let Some((_, prefix)) = adapter.addresses.iter().find(|(a, _)| *a == snapshot.added) {
        if *prefix != PREFIX {
            return fail("managed address was changed manually; restoration deferred");
        }
        // Indices/names may change on reboot/replug; only the original physical
        // MAC may be acted on. All unrelated/manual addresses stay untouched.
    }
    snapshot.index = adapter.index;
    snapshot.name = adapter.name;
    // Tentative/duplicate rows can be missing from normal adapter inventory.
    // Query/remove the actual OS row idempotently before clearing the journal.
    backend.remove(&snapshot)?;
    if selected(&snapshot, backend.inventory()?)?
        .is_some_and(|a| a.addresses.iter().any(|(ip, _)| *ip == snapshot.added))
    {
        return fail("temporary address removal was not confirmed");
    }
    store.save(None)?;
    store.clear_revoke()?;
    store.status(Phase::Idle)
}
pub fn begin(
    store: &impl Storage,
    backend: &impl Backend,
    adapter: Adapter,
    address: Ipv4Addr,
) -> Result<Snapshot> {
    if store.load()?.is_some() {
        return fail("previous IP snapshot needs recovery");
    }
    if !adapter.active
        || adapter.index == 0
        || adapter.addresses.len() > 64
        || adapter
            .addresses
            .iter()
            .any(|(ip, _)| !ip.is_unspecified() && !ip.is_link_local())
    {
        return fail(
            "fixed IP is allowed only on connected Ethernet without an acquired IPv4 address",
        );
    }
    for other in backend.inventory()? {
        if other.addresses.iter().any(|(a, p)| overlaps(*a, *p)) {
            return fail("temporary /30 network overlaps an existing address");
        }
    }
    let snapshot = Snapshot {
        schema: 1,
        index: adapter.index,
        name: adapter.name.clone(),
        mac: adapter.mac.clone(),
        original: adapter.addresses.clone(),
        added: address,
    };
    validate(&snapshot)?;
    backend.preflight(&adapter, address)?;
    if backend.cancelled() {
        return fail("local IP request cancelled");
    }
    store.clear_revoke()?;
    // Persist before the first OS mutation, including a crash during DAD.
    store.save(Some(&snapshot))?;
    if let Err(error) = backend.add(&snapshot) {
        recover(store, backend)?;
        return Err(error);
    }
    Ok(snapshot)
}
pub fn run(
    store: &impl Storage,
    backend: &impl Backend,
    adapter: Adapter,
    address: Ipv4Addr,
) -> Result<()> {
    store.status(Phase::Starting)?;
    let snapshot = match begin(store, backend, adapter, address) {
        Ok(snapshot) => snapshot,
        Err(error) => {
            store.status(Phase::Failed)?;
            return Err(error);
        }
    };
    let start = Instant::now();
    let result = (|| -> Result<()> {
        store.status(Phase::Active)?;
        println!("Temporary Ethernet {address}/{PREFIX} active. Unplug, restore or 12-hour expiry removes only this address.");
        let mut heartbeat = Instant::now();
        loop {
            if start.elapsed() >= Duration::from_secs(12 * 60 * 60)
                || store.revoked()?
                || backend.cancelled()
            {
                return Ok(());
            }
            let Some(current) = selected(&snapshot, backend.inventory()?)? else {
                return Ok(());
            };
            if !current.active
                || !current.addresses.contains(&(address, PREFIX))
                || current
                    .addresses
                    .iter()
                    .any(|(ip, _)| *ip != address && !ip.is_link_local() && !ip.is_unspecified())
            {
                return Ok(());
            }
            if heartbeat.elapsed() >= Duration::from_secs(10) {
                store.status(Phase::Active)?;
                heartbeat = Instant::now();
            }
            std::thread::sleep(Duration::from_millis(500));
        }
    })();
    if let Err(error) = recover(store, backend) {
        store.status(Phase::Deferred)?;
        return Err(error);
    }
    result
}
#[cfg(test)]
mod tests;
