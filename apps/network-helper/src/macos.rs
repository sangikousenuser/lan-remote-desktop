use crate::lease::{self, Backend, Snapshot, Storage, PREFIX};
use cap_std::fs::{
    Dir, DirBuilder, DirBuilderExt, MetadataExt as CapMetadataExt, OpenOptions, OpenOptionsExt,
};
use lrd_session::{
    fail,
    network::{self, Adapter},
    Result,
};
use std::{
    ffi::CString,
    fs::File,
    io::{Read, Write},
    net::Ipv4Addr,
    os::unix::fs::MetadataExt,
    process::Command,
};
pub const ADDRESS: Ipv4Addr = lease::MAC_ADDRESS;
static CANCELLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
extern "C" fn stop(_: i32) {
    CANCELLED.store(true, std::sync::atomic::Ordering::Relaxed);
}
pub fn authorized() -> Result<()> {
    if unsafe { libc::geteuid() } != 0 {
        return fail("use the local wizard and macOS administrator authorization");
    }
    unsafe {
        for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
            libc::signal(signal, stop as *const () as libc::sighandler_t);
        }
        libc::signal(libc::SIGPIPE, libc::SIG_IGN);
    }
    Ok(())
}
pub struct Store(Dir);
impl Store {
    pub fn open() -> Result<Self> {
        authorized()?;
        for name in ["/Library", "/Library/Application Support"] {
            let m = std::fs::symlink_metadata(name)?;
            if !m.is_dir() || m.file_type().is_symlink() || m.uid() != 0 || m.mode() & 0o022 != 0 {
                return fail("unsafe privileged journal parent");
            }
        }
        let parent =
            Dir::open_ambient_dir("/Library/Application Support", cap_std::ambient_authority())?;
        let name = "LAN Remote Desktop Network";
        if !parent.exists(name) {
            let mut builder = DirBuilder::new();
            builder.mode(0o755);
            parent.create_dir_with(name, &builder)?;
        }
        let m = parent.symlink_metadata(name)?;
        if !m.is_dir() || m.file_type().is_symlink() || m.uid() != 0 || m.mode() & 0o022 != 0 {
            return fail("unsafe privileged journal ownership/permissions");
        }
        Ok(Self(parent.open_dir(name)?))
    }
    pub fn revoke(&self) -> Result<()> {
        self.write("revoke", b"1")
    }
    fn write(&self, name: &str, bytes: &[u8]) -> Result<()> {
        let temporary = format!("{name}.new");
        let mut options = OpenOptions::new();
        options
            .write(true)
            .create_new(true)
            .mode(if name == "status.json" { 0o644 } else { 0o600 });
        // Only root can create these names; remove a previous interrupted write.
        if self.0.exists(&temporary) {
            self.0.remove_file(&temporary)?;
        }
        let mut file = self.0.open_with(&temporary, &options)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        self.0.rename(&temporary, &self.0, name)?;
        self.0.try_clone()?.into_std_file().sync_all()?;
        Ok(())
    }
}
impl Storage for Store {
    fn status(&self, phase: lease::Phase) -> Result<()> {
        self.write(
            "status.json",
            &serde_json::to_vec(&lease::Status::new(phase))?,
        )
    }
    fn load(&self) -> Result<Option<Snapshot>> {
        if !self.0.exists("snapshot.json") {
            return Ok(None);
        }
        let m = self.0.symlink_metadata("snapshot.json")?;
        if !m.is_file() || m.file_type().is_symlink() || m.uid() != 0 || m.len() > 65536 {
            return fail("invalid IP journal");
        }
        let mut bytes = Vec::new();
        self.0
            .open("snapshot.json")?
            .take(65537)
            .read_to_end(&mut bytes)?;
        if bytes.len() > 65536 {
            return fail("IP journal size limit");
        }
        Ok(Some(serde_json::from_slice(&bytes)?))
    }
    fn save(&self, value: Option<&Snapshot>) -> Result<()> {
        if let Some(value) = value {
            self.write("snapshot.json", &serde_json::to_vec(value)?)
        } else {
            if self.0.exists("snapshot.json") {
                self.0.remove_file("snapshot.json")?;
            }
            self.0.try_clone()?.into_std_file().sync_all()?;
            Ok(())
        }
    }
    fn revoked(&self) -> Result<bool> {
        Ok(self.0.exists("revoke"))
    }
    fn clear_revoke(&self) -> Result<()> {
        if self.0.exists("revoke") {
            self.0.remove_file("revoke")?;
        }
        Ok(())
    }
}
pub fn lock() -> Result<Option<File>> {
    let store = Store::open()?;
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).mode(0o600);
    let file = store.0.open_with("lease.lock", &options)?.into_std();
    match file.try_lock() {
        Ok(()) => Ok(Some(file)),
        Err(std::fs::TryLockError::WouldBlock) => Ok(None),
        Err(error) => Err(error.into()),
    }
}
pub struct Network;
pub fn read_status() -> Result<lease::Status> {
    let path = "/Library/Application Support/LAN Remote Desktop Network/status.json";
    let m = match std::fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(lease::Status::default()),
        Err(e) => return Err(e.into()),
    };
    if !m.is_file()
        || m.file_type().is_symlink()
        || m.uid() != 0
        || m.mode() & 0o022 != 0
        || m.len() > 4096
    {
        return fail("invalid network helper status");
    }
    let mut bytes = Vec::new();
    File::open(path)?.take(4097).read_to_end(&mut bytes)?;
    if bytes.len() > 4096 {
        return fail("status size limit");
    }
    Ok(serde_json::from_slice(&bytes)?)
}
fn ifconfig(snapshot: &Snapshot, add: bool) -> Result<()> {
    // Neither name nor an address supplied by a caller is passed through a shell.
    if !snapshot.name.starts_with("en")
        || snapshot.name.len() > 15
        || !snapshot.name[2..].bytes().all(|b| b.is_ascii_digit())
    {
        return fail("invalid physical Ethernet name");
    }
    if !network::ethernet_adapters().iter().any(|a| {
        a.index == snapshot.index
            && a.name == snapshot.name
            && a.mac.eq_ignore_ascii_case(&snapshot.mac)
            && (!add || a.active)
    }) {
        return fail("Ethernet identity changed before alias operation");
    }
    let mut command = Command::new("/sbin/ifconfig");
    command.env_clear().env("LC_ALL", "C").args([
        &snapshot.name,
        "inet",
        &snapshot.added.to_string(),
    ]);
    if add {
        command.args(["netmask", "255.255.255.252", "alias"]);
    } else {
        command.arg("-alias");
    }
    if !command.status()?.success() {
        return fail("temporary Ethernet alias operation failed");
    }
    Ok(())
}
impl Backend for Network {
    fn cancelled(&self) -> bool {
        CANCELLED.load(std::sync::atomic::Ordering::Relaxed)
    }
    fn inventory(&self) -> Result<Vec<Adapter>> {
        Ok(network::ethernet_adapters())
    }
    fn preflight(&self, adapter: &Adapter, address: Ipv4Addr) -> Result<()> {
        if network::ethernet_adapters().iter().all(|a| {
            a.index != adapter.index
                || a.mac != adapter.mac
                || !a.active
                || a.addresses != adapter.addresses
        }) {
            return fail("Ethernet changed before configuration");
        }
        for nic in net_interfaces() {
            if nic.iter().any(|(ip, p)| lease::overlaps(*ip, *p)) {
                return fail("temporary network overlaps another interface");
            }
        }
        for last in 0..4 {
            let output = Command::new("/sbin/route")
                .env_clear()
                .env("LC_ALL", "C")
                .args(["-n", "get", &Ipv4Addr::new(10, 254, 240, last).to_string()])
                .output()?;
            let stdout = String::from_utf8(output.stdout)?;
            let stderr = String::from_utf8(output.stderr)?;
            if output.status.success() {
                if !stdout
                    .lines()
                    .any(|line| line.trim() == "destination: default")
                {
                    return fail("temporary network overlaps a non-default route");
                }
            } else if !stderr.contains("not in table") {
                return fail("cannot validate IPv4 route isolation");
            }
        }
        let name = CString::new(adapter.name.as_str())?;
        let mac = adapter
            .mac
            .split(':')
            .map(|v| u8::from_str_radix(v, 16))
            .collect::<std::result::Result<Vec<_>, _>>()?;
        if mac.len() != 6 {
            return fail("invalid Ethernet hardware address");
        }
        unsafe extern "C" {
            fn lrd_arp_probe(name: *const libc::c_char, mac: *const u8, ip: *const u8) -> i32;
        }
        match unsafe { lrd_arp_probe(name.as_ptr(), mac.as_ptr(), address.octets().as_ptr()) } {
            0 => Ok(()),
            1 => fail("temporary address is already in use on Ethernet"),
            _ => Err(std::io::Error::last_os_error().into()),
        }
    }
    fn add(&self, snapshot: &Snapshot) -> Result<()> {
        ifconfig(snapshot, true)?;
        if !self.inventory()?.iter().any(|a| {
            a.index == snapshot.index
                && a.mac == snapshot.mac
                && a.addresses.contains(&(snapshot.added, PREFIX))
        }) {
            return fail("temporary address was not confirmed");
        }
        Ok(())
    }
    fn remove(&self, snapshot: &Snapshot) -> Result<()> {
        if !self.inventory()?.iter().any(|a| {
            a.mac.eq_ignore_ascii_case(&snapshot.mac)
                && a.addresses.iter().any(|(ip, _)| *ip == snapshot.added)
        }) {
            return Ok(());
        }
        ifconfig(snapshot, false)
    }
}
fn net_interfaces() -> Vec<Vec<(Ipv4Addr, u8)>> {
    // Read every interface, including Wi-Fi, for conflict detection only.
    let mut addresses = Vec::new();
    unsafe {
        let mut head = std::ptr::null_mut();
        if libc::getifaddrs(&mut head) != 0 {
            return vec![vec![(ADDRESS, PREFIX)]];
        }
        let mut item = head;
        while !item.is_null() {
            let entry = &*item;
            if !entry.ifa_addr.is_null()
                && (*entry.ifa_addr).sa_family as i32 == libc::AF_INET
                && !entry.ifa_netmask.is_null()
            {
                let ip = &*entry.ifa_addr.cast::<libc::sockaddr_in>();
                let mask = &*entry.ifa_netmask.cast::<libc::sockaddr_in>();
                addresses.push(vec![(
                    Ipv4Addr::from(ip.sin_addr.s_addr.to_ne_bytes()),
                    u32::from_be(mask.sin_addr.s_addr).count_ones() as u8,
                )]);
            }
            item = entry.ifa_next;
        }
        libc::freeifaddrs(head);
    }
    addresses
}
#[cfg(test)]
mod tests {
    #[test]
    fn arp_detects_claims_and_simultaneous_probes_only_from_other_macs() {
        unsafe extern "C" {
            fn lrd_arp_conflict_fixture(
                p: *const u8,
                n: usize,
                mac: *const u8,
                ip: *const u8,
            ) -> i32;
        }
        let mac = [2, 0, 0, 0, 0, 1];
        let ip = [10, 254, 240, 2];
        let mut p = [0u8; 60];
        p[12] = 8;
        p[13] = 6;
        p[15] = 1;
        p[16] = 8;
        p[18] = 6;
        p[19] = 4;
        p[21] = 1;
        p[22..28].copy_from_slice(&[2, 0, 0, 0, 0, 2]);
        p[38..42].copy_from_slice(&ip);
        let check = |p: &[u8]| unsafe {
            lrd_arp_conflict_fixture(p.as_ptr(), p.len(), mac.as_ptr(), ip.as_ptr())
        };
        assert_eq!(check(&p), 1);
        p[28..32].copy_from_slice(&ip);
        p[38..42].fill(0);
        assert_eq!(check(&p), 1);
        p[22..28].copy_from_slice(&mac);
        assert_eq!(check(&p), 0);
        p[22..28].fill(3);
        p[28..32].fill(0);
        assert_eq!(check(&p), 0);
        for n in 0..42 {
            assert_eq!(check(&p[..n]), 0);
        }
    }
}
