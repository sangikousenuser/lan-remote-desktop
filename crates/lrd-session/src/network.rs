use crate::{fail, Result};
use netdev::interface::types::InterfaceType;
use netdev::Interface;
use serde::{Deserialize, Serialize};
use socket2::{Domain, Protocol, SockRef, Socket, Type};
use std::{
    net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream, UdpSocket},
    time::{Duration, Instant},
};

pub const PORT: u16 = 47990;
pub const DISCOVERY_PORT: u16 = 47991;
pub const HOST_SLOTS: u16 = 32;
pub const SLOT_STRIDE: u16 = 32;
pub struct HostBinding {
    pub listener: TcpListener,
    pub discovery: UdpSocket,
    pub port: u16,
}
fn occupied(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::AddrInUse
        || cfg!(windows) && error.raw_os_error() == Some(10013)
}
fn exclusive(socket: &Socket) -> Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawSocket;
        use windows::Win32::Networking::WinSock::{
            setsockopt, WSAGetLastError, SOCKET, SOL_SOCKET, SO_EXCLUSIVEADDRUSE,
        };
        if unsafe {
            setsockopt(
                SOCKET(socket.as_raw_socket() as usize),
                SOL_SOCKET,
                SO_EXCLUSIVEADDRUSE,
                Some(&1i32.to_ne_bytes()),
            )
        } != 0
        {
            return Err(std::io::Error::from_raw_os_error(unsafe { WSAGetLastError() }.0).into());
        }
    }
    #[cfg(not(windows))]
    let _ = socket;
    Ok(())
}
fn reserve_host(
    address: Ipv4Addr,
    create: impl Fn(Type, Protocol) -> Result<Socket>,
) -> Result<HostBinding> {
    for slot in 0..HOST_SLOTS {
        let port = PORT + slot * SLOT_STRIDE;
        let tcp = create(Type::STREAM, Protocol::TCP)?;
        exclusive(&tcp)?;
        if let Err(error) = tcp.bind(&SocketAddr::from((address, port)).into()) {
            if occupied(&error) {
                continue;
            }
            return Err(error.into());
        }
        let udp = create(Type::DGRAM, Protocol::UDP)?;
        exclusive(&udp)?;
        udp.set_broadcast(true)?;
        if let Err(error) = udp.bind(&SocketAddr::from((address, port + 1)).into()) {
            if occupied(&error) {
                continue;
            }
            return Err(error.into());
        }
        tcp.listen(8)?;
        return Ok(HostBinding {
            listener: tcp.into(),
            discovery: udp.into(),
            port,
        });
    }
    fail("all per-user Host listening slots are occupied")
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Nic {
    pub index: u32,
    pub name: String,
    pub label: String,
    pub address: Ipv4Addr,
    pub prefix: u8,
    pub mac: String,
}
fn wired(i: &Interface) -> bool {
    matches!(
        i.if_type,
        InterfaceType::Ethernet
            | InterfaceType::FastEthernetT
            | InterfaceType::FastEthernetFx
            | InterfaceType::GigabitEthernet
    ) && !i.is_loopback()
        && i.is_up()
        && physical(i)
}
#[cfg(windows)]
fn physical(i: &Interface) -> bool {
    use windows::Win32::NetworkManagement::IpHelper::{GetIfEntry2, MIB_IF_ROW2};
    let mut row = MIB_IF_ROW2 {
        InterfaceIndex: i.index,
        ..Default::default()
    };
    unsafe {
        GetIfEntry2(&mut row).is_ok()
            && row.InterfaceAndOperStatusFlags._bitfield & 0b101 == 0b101
            && row.InterfaceAndOperStatusFlags._bitfield & 0b10000 == 0
    }
}
#[cfg(not(windows))]
fn physical(i: &Interface) -> bool {
    i.is_physical()
        && (cfg!(not(target_os = "macos"))
            || (i.name.starts_with("en") && mac_link_active(&i.name)))
}
#[cfg(all(not(target_os = "macos"), not(windows)))]
fn mac_link_active(_name: &str) -> bool {
    true
}
#[cfg(target_os = "macos")]
fn mac_link_active(name: &str) -> bool {
    use std::{
        ffi::{c_char, c_void, CString},
        ptr::null,
    };
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFStringCreateWithCString(
            allocator: *const c_void,
            value: *const c_char,
            encoding: u32,
        ) -> *const c_void;
        fn CFGetTypeID(value: *const c_void) -> usize;
        fn CFDictionaryGetTypeID() -> usize;
        fn CFDictionaryGetValue(dictionary: *const c_void, key: *const c_void) -> *const c_void;
        fn CFBooleanGetTypeID() -> usize;
        fn CFBooleanGetValue(value: *const c_void) -> u8;
        fn CFRelease(value: *const c_void);
    }
    #[link(name = "SystemConfiguration", kind = "framework")]
    unsafe extern "C" {
        fn SCDynamicStoreCopyValue(store: *const c_void, key: *const c_void) -> *const c_void;
    }
    let Ok(key) = CString::new(format!("State:/Network/Interface/{name}/Link")) else {
        return false;
    };
    unsafe {
        let key = CFStringCreateWithCString(null(), key.as_ptr(), 0x08000100);
        if key.is_null() {
            return false;
        }
        let value = SCDynamicStoreCopyValue(null(), key);
        CFRelease(key);
        if value.is_null() {
            return false;
        }
        let active = CFStringCreateWithCString(null(), c"Active".as_ptr(), 0x08000100);
        let result = if !active.is_null() && CFGetTypeID(value) == CFDictionaryGetTypeID() {
            let flag = CFDictionaryGetValue(value, active);
            !flag.is_null()
                && CFGetTypeID(flag) == CFBooleanGetTypeID()
                && CFBooleanGetValue(flag) != 0
        } else {
            false
        };
        if !active.is_null() {
            CFRelease(active);
        }
        CFRelease(value);
        result
    }
}
pub fn interfaces() -> Vec<Nic> {
    netdev::get_interfaces()
        .into_iter()
        .filter(wired)
        .filter(|i| i.mac_addr.is_some())
        .flat_map(|i| {
            i.ipv4
                .iter()
                .filter(|n| !n.addr().is_loopback() && !n.addr().is_unspecified())
                .map(|n| Nic {
                    index: i.index,
                    name: i.name.clone(),
                    label: i.friendly_name.clone().unwrap_or_else(|| i.name.clone()),
                    address: n.addr(),
                    prefix: n.prefix_len(),
                    mac: i.mac_addr.map(|m| m.to_string()).unwrap_or_default(),
                })
                .collect::<Vec<_>>()
        })
        .collect()
}
#[cfg(not(windows))]
fn isolation_ok() -> bool {
    true
}
#[cfg(windows)]
fn isolation_ok() -> bool {
    use windows::Win32::{
        NetworkManagement::IpHelper::{GetIpInterfaceEntry, MIB_IPINTERFACE_ROW},
        Networking::WinSock::AF_INET,
    };
    for nic in netdev::get_interfaces()
        .iter()
        .filter(|n| !n.is_loopback() && n.is_up() && !n.ipv4.is_empty())
    {
        let mut row = MIB_IPINTERFACE_ROW {
            Family: AF_INET,
            InterfaceIndex: nic.index,
            ..Default::default()
        };
        if unsafe { GetIpInterfaceEntry(&mut row) }.is_err()
            || row.WeakHostSend.as_bool()
            || row.WeakHostReceive.as_bool()
            || row.ForwardingEnabled.as_bool()
        {
            return false;
        }
    }
    true
}
impl Nic {
    pub fn select_mac(mac: &str) -> Result<Self> {
        if mac.is_empty() {
            return fail("Ethernet identity is missing");
        }
        interfaces()
            .into_iter()
            .find(|n| n.mac == mac)
            .ok_or_else(|| std::io::Error::other("selected Ethernet device unavailable").into())
    }
    pub fn select(index: u32) -> Result<Self> {
        interfaces()
            .into_iter()
            .find(|i| i.index == index)
            .ok_or_else(|| {
                std::io::Error::other("Ethernet interface is unavailable or has no IPv4 address")
                    .into()
            })
    }
    pub fn current(&self) -> bool {
        isolation_ok()
            && interfaces().iter().any(|i| {
                i.index == self.index
                    && i.name == self.name
                    && i.address == self.address
                    && i.prefix == self.prefix
                    && i.mac == self.mac
            })
    }
    pub fn contains(&self, address: Ipv4Addr) -> bool {
        if self.prefix == 0
            || self.prefix > 32
            || address == self.address
            || address.is_unspecified()
            || address.is_multicast()
            || address.is_broadcast()
            || address.is_loopback()
            || (self.prefix < 31
                && (address == self.broadcast()
                    || u32::from(address) & !(u32::MAX << (32 - self.prefix)) == 0))
        {
            return false;
        }
        let mask = u32::MAX << (32 - self.prefix);
        u32::from(address) & mask == u32::from(self.address) & mask
    }
    pub fn broadcast(&self) -> Ipv4Addr {
        let mask = if self.prefix == 0 {
            0
        } else {
            u32::MAX << (32 - self.prefix)
        };
        Ipv4Addr::from(u32::from(self.address) | !mask)
    }
    fn socket(&self, kind: Type, protocol: Protocol) -> Result<Socket> {
        if !self.current() {
            return fail("selected Ethernet link/address changed");
        }
        let socket = Socket::new(Domain::IPV4, kind, Some(protocol))?;
        exclusive(&socket)?;
        bind_interface(&socket, self.index, protocol == Protocol::UDP)?;
        Ok(socket)
    }
    pub fn listen(&self, port: u16) -> Result<TcpListener> {
        let socket = self.socket(Type::STREAM, Protocol::TCP)?;
        socket.bind(&SocketAddr::from((self.address, port)).into())?;
        socket.listen(8)?;
        Ok(socket.into())
    }
    pub fn bind_host(&self) -> Result<HostBinding> {
        reserve_host(self.address, |kind, protocol| self.socket(kind, protocol))
    }
    pub fn accept(&self, listener: &TcpListener) -> Result<TcpStream> {
        let (stream, _) = listener.accept()?;
        self.constrain_tcp(&stream)?;
        Ok(stream)
    }
    pub fn constrain_tcp(&self, stream: &TcpStream) -> Result<()> {
        let remote = stream.peer_addr()?;
        let SocketAddr::V4(remote) = remote else {
            return fail("IPv4 peer required");
        };
        if !self.contains(*remote.ip())
            || stream.local_addr()?.ip() != self.address
            || !self.current()
        {
            return fail("peer is outside selected Ethernet subnet");
        }
        bind_interface(&SockRef::from(stream), self.index, false)?;
        Ok(())
    }
    pub fn connect(&self, remote: SocketAddr) -> Result<TcpStream> {
        let SocketAddr::V4(remote_v4) = remote else {
            return fail("IPv4 peer required");
        };
        if !self.contains(*remote_v4.ip()) {
            return fail("peer is outside selected Ethernet subnet");
        }
        let socket = self.socket(Type::STREAM, Protocol::TCP)?;
        socket.bind(&SocketAddr::from((self.address, 0)).into())?;
        socket.connect_timeout(&remote.into(), Duration::from_secs(5))?;
        Ok(socket.into())
    }
    pub fn udp(&self, port: u16) -> Result<UdpSocket> {
        let socket = self.socket(Type::DGRAM, Protocol::UDP)?;
        socket.set_broadcast(true)?;
        socket.bind(&SocketAddr::from((self.address, port)).into())?;
        Ok(socket.into())
    }
}

fn bind_interface(socket: &Socket, index: u32, udp: bool) -> Result<()> {
    if index == 0 {
        return fail("interface index zero is forbidden");
    }
    #[cfg(target_os = "macos")]
    {
        let _ = udp;
        socket.bind_device_by_index_v4(std::num::NonZeroU32::new(index))?;
    }
    #[cfg(target_os = "linux")]
    {
        let _ = udp;
        socket.bind_device_by_index_v4(std::num::NonZeroU32::new(index))?;
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawSocket;
        use windows::Win32::Networking::WinSock::{setsockopt, IPPROTO_IP, SOCKET};
        if !isolation_ok() {
            return fail("cannot verify strict Ethernet isolation");
        }
        let value = index.to_be();
        let bytes = unsafe { std::slice::from_raw_parts((&value as *const u32).cast::<u8>(), 4) };
        let status = unsafe {
            setsockopt(
                SOCKET(socket.as_raw_socket() as usize),
                IPPROTO_IP.0,
                31,
                Some(bytes),
            )
        };
        if status != 0 {
            return Err(std::io::Error::from_raw_os_error(
                unsafe { windows::Win32::Networking::WinSock::WSAGetLastError() }.0,
            )
            .into());
        }
        if udp {
            for (option, value) in [(28, 1u32), (29, index)] {
                let bytes =
                    unsafe { std::slice::from_raw_parts((&value as *const u32).cast::<u8>(), 4) };
                if unsafe {
                    setsockopt(
                        SOCKET(socket.as_raw_socket() as usize),
                        IPPROTO_IP.0,
                        option,
                        Some(bytes),
                    )
                } != 0
                {
                    return Err(std::io::Error::from_raw_os_error(
                        unsafe { windows::Win32::Networking::WinSock::WSAGetLastError() }.0,
                    )
                    .into());
                }
            }
        }
    }
    Ok(())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Advertisement {
    pub major: u32,
    pub name: String,
    pub fingerprint: String,
    pub port: u16,
    pub user_session: u32,
    pub nonce: u64,
}
pub fn discover(nic: &Nic, timeout: Duration) -> Result<Vec<(SocketAddr, Advertisement)>> {
    let socket = nic.udp(0)?;
    socket.set_read_timeout(Some(Duration::from_millis(200)))?;
    let nonce = crate::random_u64()?;
    let mut request = b"LRD1".to_vec();
    request.extend(nonce.to_be_bytes());
    let start = Instant::now();
    let mut results = Vec::new();
    let mut buffer = [0u8; 4096];
    let mut last = Instant::now() - Duration::from_secs(1);
    while start.elapsed() < timeout {
        if !nic.current() {
            return fail("Ethernet link/address changed");
        }
        if last.elapsed() > Duration::from_millis(500) {
            for slot in 0..HOST_SLOTS {
                socket.send_to(
                    &request,
                    (nic.broadcast(), DISCOVERY_PORT + slot * SLOT_STRIDE),
                )?;
            }
            last = Instant::now();
        }
        match socket.recv_from(&mut buffer) {
            Ok((n, source)) => {
                if !source.ip().is_ipv4()
                    || !nic.contains(match source.ip() {
                        std::net::IpAddr::V4(a) => a,
                        _ => continue,
                    })
                {
                    continue;
                }
                if let Ok(ad) = serde_json::from_slice::<Advertisement>(&buffer[..n]) {
                    if ad.major == 1
                        && ad.nonce == nonce
                        && ad.port != 0
                        && ad.name.len() <= 128
                        && ad.fingerprint.len() == 64
                        && ad.fingerprint.bytes().all(|b| b.is_ascii_hexdigit())
                        && results.len() < 32
                    {
                        let address = SocketAddr::new(source.ip(), ad.port);
                        if !results.iter().any(|(a, _)| *a == address) {
                            results.push((address, ad));
                        }
                    }
                }
            }
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(results)
}
pub fn advertise(
    nic: Nic,
    mut info: Advertisement,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    socket: UdpSocket,
) -> Result<std::thread::JoinHandle<()>> {
    if socket.local_addr()?.ip() != nic.address || socket.local_addr()?.port() != info.port + 1 {
        return fail("discovery socket is outside its reserved Host slot");
    }
    socket.set_read_timeout(Some(Duration::from_millis(250)))?;
    Ok(std::thread::spawn(move || {
        let mut bytes = [0u8; 128];
        let mut last_reply = Instant::now() - Duration::from_secs(1);
        while !stop.load(std::sync::atomic::Ordering::Acquire) && nic.current() {
            if let Ok((n, source)) = socket.recv_from(&mut bytes) {
                #[cfg(windows)]
                if !interactive_session(info.user_session) {
                    continue;
                }
                if n != 12
                    || &bytes[..4] != b"LRD1"
                    || last_reply.elapsed() < Duration::from_millis(100)
                {
                    continue;
                }
                if let std::net::IpAddr::V4(ip) = source.ip() {
                    if !nic.contains(ip) {
                        continue;
                    }
                }
                info.nonce = u64::from_be_bytes(bytes[4..12].try_into().unwrap());
                if let Ok(reply) = serde_json::to_vec(&info) {
                    let _ = socket.send_to(&reply, source);
                    last_reply = Instant::now();
                }
            }
        }
    }))
}

#[cfg(windows)]
pub fn interactive_session(session: u32) -> bool {
    use windows::{core::PWSTR, Win32::System::RemoteDesktop::*};
    let mut buffer = PWSTR::null();
    let mut bytes = 0;
    if unsafe {
        WTSQuerySessionInformationW(
            WTS_CURRENT_SERVER_HANDLE,
            session,
            WTSConnectState,
            &mut buffer,
            &mut bytes,
        )
    }
    .is_err()
    {
        return false;
    }
    let active = !buffer.is_null()
        && bytes >= std::mem::size_of::<WTS_CONNECTSTATE_CLASS>() as u32
        && unsafe { std::ptr::read_unaligned(buffer.0.cast::<WTS_CONNECTSTATE_CLASS>()) }
            == WTSActive;
    unsafe { WTSFreeMemory(buffer.0.cast()) };
    active
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn multiple_user_hosts_reserve_distinct_tcp_and_discovery_pairs() {
        let make = |kind, protocol| -> Result<Socket> {
            Ok(Socket::new(Domain::IPV4, kind, Some(protocol))?)
        };
        let first = reserve_host(Ipv4Addr::LOCALHOST, make).unwrap();
        let second = reserve_host(Ipv4Addr::LOCALHOST, make).unwrap();
        assert_ne!(first.port, second.port);
        assert_eq!(first.discovery.local_addr().unwrap().port(), first.port + 1);
        assert_eq!(
            second.discovery.local_addr().unwrap().port(),
            second.port + 1
        );
        assert!((first.port - PORT).is_multiple_of(SLOT_STRIDE));
        assert!((second.port - PORT).is_multiple_of(SLOT_STRIDE));
    }
    #[test]
    fn peer_must_be_on_selected_subnet() {
        let n = Nic {
            index: 1,
            name: "test".into(),
            label: "test".into(),
            address: Ipv4Addr::new(10, 254, 240, 1),
            prefix: 30,
            mac: String::new(),
        };
        assert!(n.contains(Ipv4Addr::new(10, 254, 240, 2)));
        assert!(!n.contains(Ipv4Addr::new(192, 168, 1, 1)));
        assert!(!n.contains(n.address));
        assert_eq!(n.broadcast(), Ipv4Addr::new(10, 254, 240, 3));
    }
}
