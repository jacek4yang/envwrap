//! Bounded HTTP CONNECT and SOCKS5 handshakes. Never dial a target directly.

use std::io::{self, BufReader, Read, Write};
use std::net::{IpAddr, SocketAddr, TcpStream};
use std::sync::Arc;

use crate::{
    proxy,
    transport::{self, Cancel, Channel},
    wire,
};

enum Protocol {
    Http { authorization: String },
    Socks5 { authentication: Vec<u8> },
}

pub struct Upstream {
    address: SocketAddr,
    protocol: Protocol,
}
impl Upstream {
    pub fn parse(raw: &str) -> Result<Self, String> {
        let parts = proxy::parse_proxy_url(raw).map_err(|_| "invalid proxy URL; use http://, socks5:// or socks5h:// with a literal IP endpoint")?;
        if !matches!(parts.scheme.as_str(), "http" | "socks5" | "socks5h") {
            return Err("upstream must use http://, socks5:// or socks5h://; HTTPS proxies are not implemented".into());
        }
        // Requiring a literal endpoint removes an otherwise unprotected proxy
        // bootstrap DNS lookup. Application DNS always uses the broker policy.
        let ip: IpAddr = parts.host.parse().map_err(
            |_| "upstream must use an IP literal, not an unprotected bootstrap DNS lookup",
        )?;
        let port = parts
            .port
            .unwrap_or_else(|| proxy::default_port(&parts.scheme));
        if port == 0 || ip.is_unspecified() || ip.is_multicast() {
            return Err("upstream needs a unicast address and nonzero port".into());
        }
        let rest = raw
            .trim()
            .split_once("://")
            .map_or(raw.trim(), |(_, rest)| rest);
        let (userinfo, authority) = rest
            .rsplit_once('@')
            .map_or((None, rest), |(user, host)| (Some(user), host));
        let host_end = authority.find(['/', '?', '#']).unwrap_or(authority.len());
        if !matches!(&authority[host_end..], "" | "/") {
            return Err("proxy paths, queries and fragments are not supported".into());
        }
        let credentials = userinfo
            .map(|value| {
                let (user, password) = value.split_once(':').unwrap_or((value, ""));
                Ok::<_, String>((decode(user)?, decode(password)?))
            })
            .transpose()?;
        let protocol = if parts.scheme == "http" {
            let authorization = if let Some((mut user, password)) = credentials {
                user.push(b':');
                user.extend_from_slice(&password);
                format!("Proxy-Authorization: Basic {}\r\n", base64(&user))
            } else {
                String::new()
            };
            Protocol::Http { authorization }
        } else {
            let authentication = if let Some((user, password)) = credentials {
                if user.is_empty()
                    || user.len() > 255
                    || password.is_empty()
                    || password.len() > 255
                {
                    return Err("SOCKS5 username and password must each contain 1–255 bytes".into());
                }
                let mut frame = Vec::with_capacity(user.len() + password.len() + 3);
                frame.extend_from_slice(&[1, user.len() as u8]);
                frame.extend_from_slice(&user);
                frame.push(password.len() as u8);
                frame.extend_from_slice(&password);
                frame
            } else {
                Vec::new()
            };
            Protocol::Socks5 { authentication }
        };
        Ok(Self {
            address: SocketAddr::new(ip, port),
            protocol,
        })
    }
    pub fn connect(
        &self,
        target: SocketAddr,
        cancel: Arc<Cancel>,
    ) -> io::Result<BufReader<Channel<TcpStream>>> {
        let stream = transport::connect(self.address, cancel, wire::SETUP_TIMEOUT, None)?;
        let mut reader = BufReader::new(stream);
        match &self.protocol {
            Protocol::Http { authorization } => {
                write!(
                    reader.get_mut(),
                    "CONNECT {target} HTTP/1.1\r\nHost: {target}\r\n{authorization}\r\n"
                )?;
                let mut budget = 8192;
                let status = header_line(&mut reader, &mut budget)?;
                let mut fields = status.split_whitespace();
                if !matches!(fields.next(), Some("HTTP/1.0" | "HTTP/1.1"))
                    || fields.next() != Some("200")
                {
                    // Never echo an upstream-controlled status line or password.
                    return Err(io::Error::new(
                        io::ErrorKind::ConnectionRefused,
                        "HTTP proxy refused CONNECT",
                    ));
                }
                while !header_line(&mut reader, &mut budget)?.is_empty() {}
            }
            Protocol::Socks5 { authentication } => {
                let method = if authentication.is_empty() { 0 } else { 2 };
                reader.get_mut().write_all(&[5, 1, method])?;
                let mut reply = [0; 2];
                reader.read_exact(&mut reply)?;
                if reply != [5, method] {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "SOCKS5 authentication method refused",
                    ));
                }
                if method == 2 {
                    reader.get_mut().write_all(authentication)?;
                    reader.read_exact(&mut reply)?;
                    if reply != [1, 0] {
                        return Err(io::Error::new(
                            io::ErrorKind::PermissionDenied,
                            "SOCKS5 authentication rejected",
                        ));
                    }
                }
                let mut request = [0; 22];
                request[..3].copy_from_slice(&[5, 1, 0]);
                let end = match target.ip() {
                    IpAddr::V4(ip) => {
                        request[3] = 1;
                        request[4..8].copy_from_slice(&ip.octets());
                        8
                    }
                    IpAddr::V6(ip) => {
                        request[3] = 4;
                        request[4..20].copy_from_slice(&ip.octets());
                        20
                    }
                };
                request[end..end + 2].copy_from_slice(&target.port().to_be_bytes());
                reader.get_mut().write_all(&request[..end + 2])?;
                let mut head = [0; 4];
                reader.read_exact(&mut head)?;
                if head[..3] != [5, 0, 0] {
                    return Err(io::Error::new(
                        io::ErrorKind::ConnectionRefused,
                        "SOCKS5 CONNECT failed",
                    ));
                }
                let length = match head[3] {
                    1 => 4,
                    4 => 16,
                    3 => {
                        let mut n = [0];
                        reader.read_exact(&mut n)?;
                        usize::from(n[0])
                    }
                    _ => return Err(io::Error::other("invalid SOCKS5 reply address")),
                };
                let mut address = [0; 257];
                reader.read_exact(&mut address[..length + 2])?;
            }
        }
        reader.get_mut().nonblocking();
        Ok(reader)
    }
}

fn header_line(reader: &mut impl std::io::BufRead, budget: &mut usize) -> io::Result<String> {
    let line = wire::line(reader, *budget)?;
    *budget = budget.saturating_sub(line.len() + 2);
    Ok(line)
}
fn decode(value: &str) -> Result<Vec<u8>, String> {
    let mut decoded = Vec::with_capacity(value.len());
    let mut bytes = value.bytes();
    while let Some(byte) = bytes.next() {
        decoded.push(if byte == b'%' {
            let high = bytes
                .next()
                .and_then(hex)
                .ok_or("invalid proxy credential encoding")?;
            let low = bytes
                .next()
                .and_then(hex)
                .ok_or("invalid proxy credential encoding")?;
            high * 16 + low
        } else {
            byte
        });
    }
    Ok(decoded)
}
fn hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}
fn base64(input: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut output = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let a = chunk[0];
        let b = chunk.get(1).copied().unwrap_or(0);
        let c = chunk.get(2).copied().unwrap_or(0);
        output.push(TABLE[(a >> 2) as usize] as char);
        output.push(TABLE[(((a & 3) << 4) | (b >> 4)) as usize] as char);
        output.push(if chunk.len() > 1 {
            TABLE[(((b & 15) << 2) | (c >> 6)) as usize] as char
        } else {
            '='
        });
        output.push(if chunk.len() > 2 {
            TABLE[(c & 63) as usize] as char
        } else {
            '='
        });
    }
    output
}
