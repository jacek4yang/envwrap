//! Private-namespace kernel sockets and UDP reply injection.
//!
//! TCP is handled by Linux, not an emulated TCP/IP stack. UDP TPROXY exposes
//! complete datagrams and their original destination, including IPv6 packets
//! that would have been fragmented on the virtual interface.

use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV6, TcpStream, UdpSocket};
use std::os::fd::AsRawFd;

pub const GATEWAY_V4: &str = "198.18.0.1";
pub const GUEST_V4: Ipv4Addr = Ipv4Addr::new(198, 18, 0, 2);
pub const GATEWAY_V6: &str = "fd23:656e:7677::1";
pub const GUEST_V6: Ipv6Addr = Ipv6Addr::new(0xfd23, 0x656e, 0x7677, 0, 0, 0, 0, 2);
pub const LOCAL_MARK: u32 = 1;
pub const REPLY_MARK: u32 = 2;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Key {
    pub source: SocketAddr,
    pub target: SocketAddr,
}
pub fn namespace_local(ip: IpAddr) -> bool {
    ip.is_loopback() || ip == IpAddr::V4(GUEST_V4) || ip == IpAddr::V6(GUEST_V6)
}

fn option<T>(fd: i32, level: i32, name: i32, value: &T) -> io::Result<()> {
    // SAFETY: value remains initialized and readable for its supplied size.
    if unsafe {
        libc::setsockopt(
            fd,
            level,
            name,
            (value as *const T).cast(),
            std::mem::size_of::<T>() as _,
        )
    } < 0
    {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}
unsafe fn address(pointer: *const libc::sockaddr, length: usize) -> io::Result<SocketAddr> {
    // SAFETY: caller supplies initialized sockaddr storage with at least its
    // family member. Each full structure is read only after checking its size.
    let family = unsafe { std::ptr::read_unaligned(pointer.cast::<libc::sa_family_t>()) };
    match i32::from(family) {
        libc::AF_INET if length >= std::mem::size_of::<libc::sockaddr_in>() => {
            let address = unsafe { std::ptr::read_unaligned(pointer.cast::<libc::sockaddr_in>()) };
            Ok(SocketAddr::new(
                Ipv4Addr::from(address.sin_addr.s_addr.to_ne_bytes()).into(),
                u16::from_be(address.sin_port),
            ))
        }
        libc::AF_INET6 if length >= std::mem::size_of::<libc::sockaddr_in6>() => {
            let address = unsafe { std::ptr::read_unaligned(pointer.cast::<libc::sockaddr_in6>()) };
            Ok(SocketAddr::V6(SocketAddrV6::new(
                Ipv6Addr::from(address.sin6_addr.s6_addr),
                u16::from_be(address.sin6_port),
                u32::from_be(address.sin6_flowinfo),
                address.sin6_scope_id,
            )))
        }
        _ => Err(io::Error::other("invalid kernel socket address")),
    }
}
pub fn original_destination(socket: &TcpStream) -> io::Result<SocketAddr> {
    let level = if socket.local_addr()?.is_ipv4() {
        libc::SOL_IP
    } else {
        libc::SOL_IPV6
    };
    // SO_ORIGINAL_DST and IP6T_SO_ORIGINAL_DST are both Linux option 80.
    let mut storage = std::mem::MaybeUninit::<libc::sockaddr_storage>::zeroed();
    let mut length = std::mem::size_of::<libc::sockaddr_storage>() as libc::socklen_t;
    // SAFETY: storage and length are writable, sized for either address family.
    if unsafe {
        libc::getsockopt(
            socket.as_raw_fd(),
            level,
            80,
            storage.as_mut_ptr().cast(),
            &mut length,
        )
    } < 0
    {
        return Err(io::Error::last_os_error());
    }
    if length < std::mem::size_of::<libc::sa_family_t>() as _ {
        return Err(io::Error::other("short original TCP destination"));
    }
    // SAFETY: the kernel initialized the returned sockaddr bytes.
    unsafe { address(storage.as_ptr().cast(), length as usize) }
}

pub struct UdpCapture {
    socket: UdpSocket,
}
impl UdpCapture {
    pub fn new(ipv6: bool) -> io::Result<Self> {
        let family = if ipv6 {
            socket2::Domain::IPV6
        } else {
            socket2::Domain::IPV4
        };
        let socket =
            socket2::Socket::new(family, socket2::Type::DGRAM, Some(socket2::Protocol::UDP))?;
        socket.set_nonblocking(true)?;
        if ipv6 {
            socket.set_only_v6(true)?;
            option(
                socket.as_raw_fd(),
                libc::SOL_IPV6,
                libc::IPV6_TRANSPARENT,
                &1i32,
            )?;
            option(
                socket.as_raw_fd(),
                libc::SOL_IPV6,
                libc::IPV6_RECVORIGDSTADDR,
                &1i32,
            )?;
        } else {
            option(
                socket.as_raw_fd(),
                libc::SOL_IP,
                libc::IP_TRANSPARENT,
                &1i32,
            )?;
            option(
                socket.as_raw_fd(),
                libc::SOL_IP,
                libc::IP_RECVORIGDSTADDR,
                &1i32,
            )?;
        }
        let bind: SocketAddr = if ipv6 { "[::]:0" } else { "0.0.0.0:0" }
            .parse()
            .expect("constant UDP bind address");
        socket.bind(&bind.into())?;
        Ok(Self {
            socket: socket.into(),
        })
    }
    pub fn fd(&self) -> i32 {
        self.socket.as_raw_fd()
    }
    pub fn port(&self) -> io::Result<u16> {
        Ok(self.socket.local_addr()?.port())
    }
    pub fn receive<'a>(&self, bytes: &'a mut [u8]) -> io::Result<Option<(Key, &'a [u8])>> {
        // SAFETY: every pointed-to buffer remains valid through recvmsg; zero
        // initializes padding and lengths not otherwise set below.
        let mut source: libc::sockaddr_storage = unsafe { std::mem::zeroed() };
        let mut control = [0usize; 32];
        let mut iov = libc::iovec {
            iov_base: bytes.as_mut_ptr().cast(),
            iov_len: bytes.len(),
        };
        let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
        message.msg_name = (&mut source as *mut libc::sockaddr_storage).cast();
        message.msg_namelen = std::mem::size_of_val(&source) as _;
        message.msg_iov = &mut iov;
        message.msg_iovlen = 1;
        message.msg_control = control.as_mut_ptr().cast();
        message.msg_controllen = std::mem::size_of_val(&control);
        let count = unsafe { libc::recvmsg(self.fd(), &mut message, libc::MSG_DONTWAIT) };
        if count < 0 {
            let error = io::Error::last_os_error();
            return if matches!(
                error.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
            ) {
                Ok(None)
            } else {
                Err(error)
            };
        }
        if message.msg_flags & (libc::MSG_TRUNC | libc::MSG_CTRUNC) != 0
            || count as usize > bytes.len()
        {
            return Err(io::Error::other("truncated kernel UDP capture"));
        }
        if message.msg_namelen < std::mem::size_of::<libc::sa_family_t>() as _ {
            return Err(io::Error::other("short UDP source address"));
        }
        let source = unsafe {
            address(
                (&source as *const libc::sockaddr_storage).cast(),
                message.msg_namelen as usize,
            )
        }?;
        let mut target = None;
        // SAFETY: libc's CMSG helpers walk the initialized ancillary buffer.
        unsafe {
            let mut current = libc::CMSG_FIRSTHDR(&message);
            while !current.is_null() {
                let header = &*current;
                let original = matches!(
                    (header.cmsg_level, header.cmsg_type),
                    (libc::SOL_IP, libc::IP_ORIGDSTADDR) | (libc::SOL_IPV6, libc::IPV6_ORIGDSTADDR)
                );
                if original {
                    let length = header.cmsg_len.saturating_sub(libc::CMSG_LEN(0) as usize);
                    if length < std::mem::size_of::<libc::sa_family_t>() {
                        return Err(io::Error::other("short original UDP address"));
                    }
                    target = Some(address(libc::CMSG_DATA(current).cast(), length)?);
                }
                current = libc::CMSG_NXTHDR(&message, current);
            }
        }
        let target =
            target.ok_or_else(|| io::Error::other("kernel omitted original UDP destination"))?;
        Ok(Some((Key { source, target }, &bytes[..count as usize])))
    }
}

pub struct UdpReplies {
    sockets: [socket2::Socket; 2],
}
impl UdpReplies {
    pub fn new() -> io::Result<Self> {
        fn socket(family: socket2::Domain) -> io::Result<socket2::Socket> {
            let socket = socket2::Socket::new(
                family,
                socket2::Type::RAW,
                Some(socket2::Protocol::from(libc::IPPROTO_RAW)),
            )?;
            socket.set_nonblocking(true)?;
            socket.set_mark(REPLY_MARK)?;
            Ok(socket)
        }
        Ok(Self {
            sockets: [
                socket(socket2::Domain::IPV4)?,
                socket(socket2::Domain::IPV6)?,
            ],
        })
    }
    pub fn send(&self, key: Key, data: &[u8]) -> io::Result<()> {
        let udp_length = u16::try_from(data.len() + 8)
            .map_err(|_| io::Error::other("UDP response too large"))?;
        let mut header = [0u8; 48];
        let (offset, index, mut sum) = match (key.target.ip(), key.source.ip()) {
            (IpAddr::V4(source), IpAddr::V4(target)) => {
                let length = u16::try_from(data.len() + 28)
                    .map_err(|_| io::Error::other("IPv4 UDP response too large"))?;
                header[0] = 0x45;
                header[2..4].copy_from_slice(&length.to_be_bytes());
                header[6] = 0x40;
                header[8] = 64;
                header[9] = 17;
                header[12..16].copy_from_slice(&source.octets());
                header[16..20].copy_from_slice(&target.octets());
                let checksum = finish(add(0, &header[..20]));
                header[10..12].copy_from_slice(&checksum.to_be_bytes());
                let sum = add(
                    add(0, &header[12..20]),
                    &[0, 17, (udp_length >> 8) as u8, udp_length as u8],
                );
                (20, 0, sum)
            }
            (IpAddr::V6(source), IpAddr::V6(target)) => {
                header[0] = 0x60;
                header[4..6].copy_from_slice(&udp_length.to_be_bytes());
                header[6] = 17;
                header[7] = 64;
                header[8..24].copy_from_slice(&source.octets());
                header[24..40].copy_from_slice(&target.octets());
                let sum = add(
                    add(0, &header[8..40]),
                    &[0, 0, (udp_length >> 8) as u8, udp_length as u8, 0, 0, 0, 17],
                );
                (40, 1, sum)
            }
            _ => return Err(io::Error::other("UDP response address families differ")),
        };
        header[offset..offset + 2].copy_from_slice(&key.target.port().to_be_bytes());
        header[offset + 2..offset + 4].copy_from_slice(&key.source.port().to_be_bytes());
        header[offset + 4..offset + 6].copy_from_slice(&udp_length.to_be_bytes());
        sum = add(add(sum, &header[offset..offset + 8]), data);
        let checksum = finish(sum);
        header[offset + 6..offset + 8]
            .copy_from_slice(&if checksum == 0 { u16::MAX } else { checksum }.to_be_bytes());
        let mut destination = key.source;
        destination.set_port(0);
        let destination = socket2::SockAddr::from(destination);
        let mut vectors = [
            libc::iovec {
                iov_base: header.as_ptr().cast_mut().cast(),
                iov_len: offset + 8,
            },
            libc::iovec {
                iov_base: data.as_ptr().cast_mut().cast(),
                iov_len: data.len(),
            },
        ];
        // SAFETY: sendmsg only reads these buffers; neither iovec nor the
        // sockaddr outlives this call. Header and payload need no joining copy.
        let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
        message.msg_name = destination.as_ptr().cast_mut().cast();
        message.msg_namelen = destination.len();
        message.msg_iov = vectors.as_mut_ptr();
        message.msg_iovlen = vectors.len();
        let result = unsafe {
            libc::sendmsg(
                self.sockets[index].as_raw_fd(),
                &message,
                libc::MSG_DONTWAIT | libc::MSG_NOSIGNAL,
            )
        };
        if result < 0 {
            let error = io::Error::last_os_error();
            if !matches!(
                error.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
            ) && error.raw_os_error() != Some(libc::ENOBUFS)
            {
                return Err(error);
            }
        }
        Ok(())
    }
}
fn add(mut sum: u32, bytes: &[u8]) -> u32 {
    let (chunks, remainder) = bytes.as_chunks::<2>();
    for chunk in chunks {
        sum += u32::from(u16::from_be_bytes(*chunk));
    }
    if let Some(&last) = remainder.first() {
        sum += u32::from(last) << 8;
    }
    sum
}
fn finish(mut sum: u32) -> u16 {
    while sum >> 16 != 0 {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}
