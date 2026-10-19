//! A per-user LaunchAgent watches only the explicitly selected physical Ethernet.
//! One notification per link/address generation prevents rejection prompt loops.
use lrd_session::{
    identity,
    network::{self, Nic},
    Result,
};
use serde::{Deserialize, Serialize};
use std::{
    ffi::{c_char, CString},
    time::Duration,
};
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Selection {
    mac: String,
}
unsafe extern "C" {
    fn lrd_agent_discovered(index: u32, address: *const c_char);
}
#[no_mangle]
pub extern "C" fn lrd_agent_configure(index: u32) -> bool {
    (|| -> Result<()> {
        let nic = Nic::select(index)?;
        let directory = identity::data_directory()?;
        identity::private_directory(&directory)?;
        let path = directory.join("agent-ethernet.json");
        let tmp = path.with_extension(format!("{}.tmp", lrd_session::random_u64()?));
        identity::write_private_new(&tmp, &serde_json::to_vec(&Selection { mac: nic.mac })?)?;
        std::fs::rename(tmp, path)?;
        Ok(())
    })()
    .is_ok()
}
pub fn run() -> Result<()> {
    let path = identity::data_directory()?.join("agent-ethernet.json");
    let bytes = identity::read_private(&path, 4096)?;
    let selection: Selection = serde_json::from_slice(&bytes)?;
    if selection.mac.len() > 32 || selection.mac.is_empty() {
        return Err("invalid Ethernet identity".into());
    }
    let mut generation = None;
    let mut notified = false;
    loop {
        if let Some(nic) = network::interfaces()
            .into_iter()
            .find(|n| n.mac == selection.mac)
        {
            let key = (nic.index, nic.address, nic.prefix, nic.mac.clone());
            if generation.as_ref() != Some(&key) {
                generation = Some(key);
                notified = false;
            }
            if !notified {
                if let Ok(mut hosts) = network::discover(&nic, Duration::from_secs(3)) {
                    crate::preferred::prioritize(
                        &mut hosts,
                        &nic.mac,
                        crate::preferred::load().as_ref(),
                        &crate::preferred::known(),
                    );
                    if let Some((address, _)) = hosts.first() {
                        if let Ok(address) = CString::new(address.to_string()) {
                            unsafe {
                                lrd_agent_discovered(nic.index, address.as_ptr());
                            }
                            notified = true;
                        }
                    }
                }
            }
        } else {
            generation = None;
            notified = false;
        }
        std::thread::sleep(Duration::from_secs(2));
    }
}
