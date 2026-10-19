use crate::lease::{self, Backend, Snapshot, Storage, PREFIX};
use ::windows::{
    core::w,
    Win32::{
        Foundation::*,
        NetworkManagement::IpHelper::*,
        Networking::WinSock::*,
        System::{Console::*, Registry::*, Threading::*},
        UI::Shell::IsUserAnAdmin,
    },
};
use lrd_session::{
    fail,
    network::{self, Adapter},
    Result,
};
use std::{
    net::Ipv4Addr,
    time::{Duration, Instant},
};
pub const ADDRESS: Ipv4Addr = lease::WINDOWS_ADDRESS;
static CANCELLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static FINISHED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
pub fn finished() {
    FINISHED.store(true, std::sync::atomic::Ordering::Relaxed);
}
unsafe extern "system" fn stop(kind: u32) -> BOOL {
    CANCELLED.store(true, std::sync::atomic::Ordering::Relaxed);
    if matches!(kind, 2 | 5 | 6) {
        let start = Instant::now();
        while !FINISHED.load(std::sync::atomic::Ordering::Relaxed)
            && start.elapsed() < Duration::from_secs(4)
        {
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    true.into()
}
pub fn authorized() -> Result<()> {
    if !unsafe { IsUserAnAdmin() }.as_bool() {
        return fail("use the local wizard and Windows UAC authorization");
    }
    unsafe {
        SetConsoleCtrlHandler(Some(stop), true)?;
    }
    Ok(())
}
pub struct Store(HKEY);
impl Drop for Store {
    fn drop(&mut self) {
        unsafe {
            let _ = RegCloseKey(self.0);
        }
    }
}
impl Store {
    pub fn open() -> Result<Self> {
        authorized()?;
        let mut key = HKEY::default();
        unsafe {
            RegCreateKeyExW(
                HKEY_LOCAL_MACHINE,
                w!("SOFTWARE\\LAN Remote Desktop\\NetworkLease"),
                0,
                None,
                REG_OPTION_NON_VOLATILE,
                KEY_READ | KEY_WRITE | KEY_WOW64_64KEY,
                None,
                &mut key,
                None,
            )
            .ok()?;
        }
        Ok(Self(key))
    }
    pub fn revoke(&self) -> Result<()> {
        self.write(w!("Revoke"), b"1")
    }
    fn write(&self, name: ::windows::core::PCWSTR, bytes: &[u8]) -> Result<()> {
        unsafe {
            RegSetValueExW(self.0, name, 0, REG_BINARY, Some(bytes)).ok()?;
            RegFlushKey(self.0).ok()?;
        }
        Ok(())
    }
    fn read(&self, name: ::windows::core::PCWSTR) -> Result<Option<Vec<u8>>> {
        let mut size = 0;
        let first = unsafe {
            RegGetValueW(
                self.0,
                None,
                name,
                RRF_RT_REG_BINARY,
                None,
                None,
                Some(&mut size),
            )
        };
        if first == ERROR_FILE_NOT_FOUND {
            return Ok(None);
        }
        first.ok()?;
        if size > 65536 {
            return fail("IP registry snapshot size limit");
        }
        let mut bytes = vec![0u8; size as usize];
        unsafe {
            RegGetValueW(
                self.0,
                None,
                name,
                RRF_RT_REG_BINARY,
                None,
                Some(bytes.as_mut_ptr().cast()),
                Some(&mut size),
            )
            .ok()?;
        }
        bytes.truncate(size as usize);
        Ok(Some(bytes))
    }
    fn delete(&self, name: ::windows::core::PCWSTR) -> Result<()> {
        let result = unsafe { RegDeleteValueW(self.0, name) };
        if result != ERROR_FILE_NOT_FOUND {
            result.ok()?;
        }
        unsafe {
            RegFlushKey(self.0).ok()?;
        }
        Ok(())
    }
}
impl Storage for Store {
    fn status(&self, phase: lease::Phase) -> Result<()> {
        self.write(
            w!("Status"),
            &serde_json::to_vec(&lease::Status::new(phase))?,
        )
    }
    fn load(&self) -> Result<Option<Snapshot>> {
        self.read(w!("Snapshot"))?
            .map(|b| serde_json::from_slice(&b).map_err(Into::into))
            .transpose()
    }
    fn save(&self, value: Option<&Snapshot>) -> Result<()> {
        if let Some(value) = value {
            self.write(w!("Snapshot"), &serde_json::to_vec(value)?)
        } else {
            self.delete(w!("Snapshot"))
        }
    }
    fn revoked(&self) -> Result<bool> {
        Ok(self.read(w!("Revoke"))?.is_some())
    }
    fn clear_revoke(&self) -> Result<()> {
        self.delete(w!("Revoke"))
    }
}
pub struct Lock(HANDLE);
impl Drop for Lock {
    fn drop(&mut self) {
        unsafe {
            let _ = ReleaseMutex(self.0);
            let _ = CloseHandle(self.0);
        }
    }
}
pub fn lock() -> Result<Option<Lock>> {
    let handle = unsafe {
        CreateMutexW(
            None,
            false,
            w!("Global\\LANRemoteDesktop-PhysicalEthernetIPLease-v1"),
        )?
    };
    let wait = unsafe { WaitForSingleObject(handle, 0) };
    if wait == WAIT_OBJECT_0 || wait == WAIT_ABANDONED {
        Ok(Some(Lock(handle)))
    } else {
        unsafe {
            let _ = CloseHandle(handle);
        }
        if wait == WAIT_TIMEOUT {
            Ok(None)
        } else {
            Err(std::io::Error::last_os_error().into())
        }
    }
}
pub fn read_status() -> Result<lease::Status> {
    let mut key = HKEY::default();
    let result = unsafe {
        RegOpenKeyExW(
            HKEY_LOCAL_MACHINE,
            w!("SOFTWARE\\LAN Remote Desktop\\NetworkLease"),
            0,
            KEY_QUERY_VALUE | KEY_WOW64_64KEY,
            &mut key,
        )
    };
    if result == ERROR_FILE_NOT_FOUND {
        return Ok(lease::Status::default());
    }
    result.ok()?;
    Store(key)
        .read(w!("Status"))?
        .map(|b| serde_json::from_slice(&b).map_err(Into::into))
        .transpose()
        .map(|s| s.unwrap_or_default())
}
fn row(snapshot: &Snapshot) -> Result<MIB_UNICASTIPADDRESS_ROW> {
    let mut interface = MIB_IF_ROW2 {
        InterfaceIndex: snapshot.index,
        ..Default::default()
    };
    unsafe {
        GetIfEntry2(&mut interface).ok()?;
    }
    let mac = interface.PhysicalAddress
        [..(interface.PhysicalAddressLength as usize).min(interface.PhysicalAddress.len())]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(":");
    if !mac.eq_ignore_ascii_case(&snapshot.mac)
        || interface.Type != 6
        || interface.InterfaceAndOperStatusFlags._bitfield & 0b101 != 0b101
    {
        return fail("physical Ethernet identity changed");
    }
    let mut row = MIB_UNICASTIPADDRESS_ROW::default();
    unsafe {
        InitializeUnicastIpAddressEntry(&mut row);
    }
    row.InterfaceIndex = snapshot.index;
    row.InterfaceLuid = interface.InterfaceLuid;
    row.Address = SOCKADDR_INET {
        Ipv4: SOCKADDR_IN {
            sin_family: AF_INET,
            sin_addr: IN_ADDR {
                S_un: IN_ADDR_0 {
                    S_addr: u32::from_ne_bytes(snapshot.added.octets()),
                },
            },
            ..Default::default()
        },
    };
    row.OnLinkPrefixLength = PREFIX;
    row.PrefixOrigin = IpPrefixOriginManual;
    row.SuffixOrigin = IpSuffixOriginManual;
    // An OS-enforced expiry also removes the alias if this process crashes.
    row.ValidLifetime = 12 * 60 * 60;
    row.PreferredLifetime = 12 * 60 * 60;
    // Never request optimistic DAD (IpDadStatePreferred).
    row.DadState = IpDadStateTentative;
    Ok(row)
}
struct Table<T>(*mut T);
impl<T> Drop for Table<T> {
    fn drop(&mut self) {
        unsafe {
            FreeMibTable(self.0.cast());
        }
    }
}
pub struct Network;
impl Backend for Network {
    fn cancelled(&self) -> bool {
        CANCELLED.load(std::sync::atomic::Ordering::Relaxed)
    }
    fn inventory(&self) -> Result<Vec<Adapter>> {
        Ok(network::ethernet_adapters())
    }
    fn preflight(&self, adapter: &Adapter, _address: Ipv4Addr) -> Result<()> {
        if self.inventory()?.iter().all(|a| {
            a.index != adapter.index
                || a.mac != adapter.mac
                || !a.active
                || a.addresses != adapter.addresses
        }) {
            return fail("Ethernet changed before configuration");
        }
        unsafe {
            let mut ptr = std::ptr::null_mut();
            GetUnicastIpAddressTable(AF_INET, &mut ptr).ok()?;
            let table = Table(ptr);
            let count = (*table.0).NumEntries as usize;
            if count > 65536 {
                return fail("address inventory limit");
            }
            for address in std::slice::from_raw_parts((*table.0).Table.as_ptr(), count) {
                let ip = Ipv4Addr::from(address.Address.Ipv4.sin_addr.S_un.S_addr.to_ne_bytes());
                if lease::overlaps(ip, address.OnLinkPrefixLength) {
                    return fail("temporary network overlaps another interface");
                }
            }
            let mut ptr = std::ptr::null_mut();
            GetIpForwardTable2(AF_INET, &mut ptr).ok()?;
            let table = Table(ptr);
            let count = (*table.0).NumEntries as usize;
            if count > 65536 {
                return fail("route inventory limit");
            }
            for route in std::slice::from_raw_parts((*table.0).Table.as_ptr(), count) {
                let ip = Ipv4Addr::from(
                    route
                        .DestinationPrefix
                        .Prefix
                        .Ipv4
                        .sin_addr
                        .S_un
                        .S_addr
                        .to_ne_bytes(),
                );
                if lease::overlaps(ip, route.DestinationPrefix.PrefixLength) {
                    return fail("temporary network overlaps a non-default route");
                }
            }
            let mut properties = MIB_IPINTERFACE_ROW {
                Family: AF_INET,
                InterfaceIndex: adapter.index,
                ..Default::default()
            };
            GetIpInterfaceEntry(&mut properties).ok()?;
            if properties.DadTransmits == 0
                || properties.WeakHostSend.as_bool()
                || properties.WeakHostReceive.as_bool()
                || properties.ForwardingEnabled.as_bool()
            {
                return fail("Ethernet must enable DAD and strict interface isolation");
            }
        }
        Ok(())
    }
    fn add(&self, snapshot: &Snapshot) -> Result<()> {
        let mut row = row(snapshot)?;
        unsafe {
            CreateUnicastIpAddressEntry(&row).ok()?;
        }
        let start = Instant::now();
        loop {
            unsafe {
                GetUnicastIpAddressEntry(&mut row).ok()?;
            }
            if row.DadState == IpDadStatePreferred {
                return Ok(());
            }
            if row.DadState != IpDadStateTentative || start.elapsed() > Duration::from_secs(15) {
                return fail("Ethernet address conflict or duplicate-address detection timeout");
            }
            std::thread::sleep(Duration::from_millis(250));
        }
    }
    fn remove(&self, snapshot: &Snapshot) -> Result<()> {
        let mut row = row(snapshot)?;
        let result = unsafe { GetUnicastIpAddressEntry(&mut row) };
        if result == ERROR_NOT_FOUND {
            return Ok(());
        }
        result.ok()?;
        if row.OnLinkPrefixLength != PREFIX
            || row.PrefixOrigin != IpPrefixOriginManual
            || row.SuffixOrigin != IpSuffixOriginManual
        {
            return fail("managed address was changed manually; restoration deferred");
        }
        unsafe {
            DeleteUnicastIpAddressEntry(&row).ok()?;
        }
        Ok(())
    }
}
