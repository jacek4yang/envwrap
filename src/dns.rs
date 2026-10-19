//! DNS policy: proxied TCP by default; explicit suffix-scoped local resolvers.

use std::io::{self, BufReader, Read, Write};
use std::net::{IpAddr, SocketAddr, UdpSocket};
use std::sync::Arc;

use crate::transport::{Cancel, Channel};
use crate::{upstream::Upstream, wire};

pub struct LocalResolver {
    pub suffix: String,
    pub address: SocketAddr,
}
struct Rule {
    suffix: Vec<u8>,
    address: SocketAddr,
}
pub struct Resolver {
    default: SocketAddr,
    local: Vec<Rule>,
}
impl Resolver {
    pub fn new(default: SocketAddr, rules: Vec<LocalResolver>) -> Result<Self, String> {
        validate_address(default)?;
        let mut local = Vec::with_capacity(rules.len());
        for rule in rules {
            validate_address(rule.address)?;
            let suffix = suffix(&rule.suffix)?;
            if local
                .iter()
                .any(|existing: &Rule| existing.suffix == suffix)
            {
                return Err("duplicate internal DNS suffix".into());
            }
            local.push(Rule {
                suffix,
                address: rule.address,
            });
        }
        local.sort_unstable_by_key(|rule| std::cmp::Reverse(rule.suffix.len()));
        Ok(Self { default, local })
    }
    pub fn local_addresses(&self) -> impl Iterator<Item = SocketAddr> + '_ {
        self.local.iter().map(|r| r.address)
    }
    fn route(&self, query: &[u8]) -> io::Result<Option<SocketAddr>> {
        let header = header(query)?;
        if header.flags & 0x8000 != 0 {
            return Err(invalid("DNS query has the response bit set"));
        }
        if self.local.is_empty() {
            return Ok(None);
        }
        let mut cursor = 12;
        let mut route = None;
        for question in 0..header.questions {
            let name = name(query, &mut cursor)?;
            cursor = cursor
                .checked_add(4)
                .filter(|&n| n <= query.len())
                .ok_or_else(|| invalid("short DNS question"))?;
            let selected = self
                .local
                .iter()
                .find(|rule| name.matches(&rule.suffix))
                .map(|rule| rule.address);
            if question != 0 && selected != route {
                return Err(invalid("DNS questions require different resolver policies"));
            }
            route = selected;
        }
        Ok(route)
    }
    pub fn lookup(
        &self,
        query: &[u8],
        upstream: &Upstream,
        cancel: Arc<Cancel>,
    ) -> io::Result<Vec<u8>> {
        let local = self.route(query)?;
        let target = local.unwrap_or(self.default);
        // TCP handles complete responses and DNS-over-TCP clients. This is a
        // fresh connection, so a failed setup is never retried through host DNS.
        let mut stream = if local.is_some() {
            BufReader::new(crate::transport::connect(
                target,
                Arc::clone(&cancel),
                wire::SETUP_TIMEOUT,
                None,
            )?)
        } else {
            upstream.connect(target, Arc::clone(&cancel))?
        };
        stream.get_mut().bounded(wire::SETUP_TIMEOUT);
        write_tcp(&mut stream.get_mut(), query)?;
        let response = read_tcp(&mut stream)?;
        validate_response(query, &response)?;
        Ok(response)
    }
    pub fn lookup_udp(
        &self,
        query: &[u8],
        upstream: &Upstream,
        cancel: Arc<Cancel>,
    ) -> io::Result<Vec<u8>> {
        let response = if let Some(target) = self.route(query)? {
            let bind = match target.ip() {
                IpAddr::V4(_) => "0.0.0.0:0",
                IpAddr::V6(_) => "[::]:0",
            };
            let socket = UdpSocket::bind(bind)?;
            socket.set_nonblocking(true)?;
            socket.connect(target)?;
            cancel.check()?;
            socket.send(query)?;
            cancel.wait(
                std::os::fd::AsRawFd::as_raw_fd(&socket),
                libc::POLLIN,
                Some(std::time::Instant::now() + wire::SETUP_TIMEOUT),
            )?;
            let mut response = vec![0; 65_535];
            let length = socket.recv(&mut response)?;
            response.truncate(length);
            validate_response(query, &response)?;
            response
        } else {
            self.lookup(query, upstream, cancel)?
        };
        truncate_udp(query, response)
    }
}

pub fn validate_address(address: SocketAddr) -> Result<(), String> {
    if address.port() == 0 || address.ip().is_unspecified() || address.ip().is_multicast() {
        Err("DNS resolver needs a unicast IP address and nonzero port".into())
    } else {
        Ok(())
    }
}
pub fn suffix(value: &str) -> Result<Vec<u8>, String> {
    if value == "." {
        return Ok(vec![0]);
    }
    let value = value.strip_suffix('.').unwrap_or(value);
    if value.is_empty() || value.len() > 253 {
        return Err("invalid internal DNS suffix".into());
    }
    let mut wire = Vec::with_capacity(value.len() + 2);
    for label in value.split('.') {
        if label.is_empty()
            || label.len() > 63
            || !label
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
        {
            return Err("internal DNS suffix must use ASCII DNS labels (use punycode for international names)".into());
        }
        wire.push(label.len() as u8);
        wire.extend(label.bytes().map(|b| b.to_ascii_lowercase()));
    }
    wire.push(0);
    Ok(wire)
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
struct Header {
    flags: u16,
    questions: u16,
    answers: u16,
    authority: u16,
    additional: u16,
}
fn header(bytes: &[u8]) -> io::Result<Header> {
    if bytes.len() < 12 {
        return Err(invalid("short DNS header"));
    }
    Ok(Header {
        flags: word(bytes, 2),
        questions: word(bytes, 4),
        answers: word(bytes, 6),
        authority: word(bytes, 8),
        additional: word(bytes, 10),
    })
}
fn word(bytes: &[u8], offset: usize) -> u16 {
    u16::from_be_bytes([bytes[offset], bytes[offset + 1]])
}
struct Name {
    bytes: [u8; 255],
    length: usize,
}
impl Name {
    fn matches(&self, suffix: &[u8]) -> bool {
        let mut offset = 0;
        while offset < self.length {
            if &self.bytes[offset..self.length] == suffix {
                return true;
            }
            if self.bytes[offset] == 0 {
                break;
            }
            offset += usize::from(self.bytes[offset]) + 1;
        }
        false
    }
}
fn name(packet: &[u8], cursor: &mut usize) -> io::Result<Name> {
    let mut result = Name {
        bytes: [0; 255],
        length: 0,
    };
    let mut offset = *cursor;
    let mut jumped = false;
    for _ in 0..128 {
        let length = *packet
            .get(offset)
            .ok_or_else(|| invalid("short DNS name"))?;
        if length & 0xc0 == 0xc0 {
            let low = *packet
                .get(offset + 1)
                .ok_or_else(|| invalid("short DNS compression pointer"))?;
            let target = (usize::from(length & 0x3f) << 8) | usize::from(low);
            if target < 12 || target >= offset {
                return Err(invalid("invalid DNS compression pointer"));
            }
            if !jumped {
                *cursor = offset + 2;
                jumped = true;
            }
            offset = target;
            continue;
        }
        if length & 0xc0 != 0 {
            return Err(invalid("invalid DNS label"));
        }
        let end = offset + usize::from(length) + 1;
        let label = packet
            .get(offset..end)
            .ok_or_else(|| invalid("short DNS label"))?;
        if result.length + label.len() > result.bytes.len() {
            return Err(invalid("DNS name exceeds 255 bytes"));
        }
        result.bytes[result.length] = length;
        for (target, source) in result.bytes[result.length + 1..result.length + label.len()]
            .iter_mut()
            .zip(&label[1..])
        {
            *target = source.to_ascii_lowercase();
        }
        result.length += label.len();
        offset = end;
        if !jumped {
            *cursor = end;
        }
        if length == 0 {
            return Ok(result);
        }
    }
    Err(invalid("too many DNS compression pointers"))
}
fn questions_end(query: &[u8], count: u16) -> io::Result<usize> {
    let mut cursor = 12;
    for _ in 0..count {
        name(query, &mut cursor)?;
        cursor = cursor
            .checked_add(4)
            .filter(|&n| n <= query.len())
            .ok_or_else(|| invalid("short DNS question"))?;
    }
    Ok(cursor)
}
fn udp_limit(query: &[u8], h: &Header, question_end: usize) -> io::Result<usize> {
    let mut cursor = question_end;
    for record in 0..u32::from(h.answers) + u32::from(h.authority) + u32::from(h.additional) {
        name(query, &mut cursor)?;
        if cursor + 10 > query.len() {
            return Err(invalid("short DNS resource record"));
        }
        let kind = word(query, cursor);
        let class = word(query, cursor + 2);
        let size = usize::from(word(query, cursor + 8));
        cursor = cursor
            .checked_add(10 + size)
            .filter(|&n| n <= query.len())
            .ok_or_else(|| invalid("short DNS record data"))?;
        if record >= u32::from(h.answers) + u32::from(h.authority) && kind == 41 {
            // A response must also fit IPv4, even if EDNS advertises 65535.
            return Ok(usize::from(class).clamp(512, 65_507));
        }
    }
    Ok(512)
}
fn truncate_udp(query: &[u8], mut response: Vec<u8>) -> io::Result<Vec<u8>> {
    let h = header(query)?;
    let end = questions_end(query, h.questions)?;
    let limit = udp_limit(query, &h, end)?;
    if response.len() <= limit {
        return Ok(response);
    }
    if end > limit {
        return Err(invalid("DNS questions exceed advertised UDP buffer"));
    }
    let flags = word(&response, 2) | 0x0200;
    response.truncate(12);
    response[2..4].copy_from_slice(&flags.to_be_bytes());
    response[4..6].copy_from_slice(&h.questions.to_be_bytes());
    response[6..12].fill(0);
    response.extend_from_slice(&query[12..end]);
    Ok(response)
}
fn validate_response(query: &[u8], response: &[u8]) -> io::Result<()> {
    let h = header(response)?;
    if query.len() < 12 || response[..2] != query[..2] || h.flags & 0x8000 == 0 {
        return Err(invalid("invalid DNS response transaction"));
    }
    Ok(())
}
pub fn read_tcp(reader: &mut impl Read) -> io::Result<Vec<u8>> {
    let mut length = [0; 2];
    reader.read_exact(&mut length)?;
    let mut bytes = vec![0; u16::from_be_bytes(length) as usize];
    reader.read_exact(&mut bytes)?;
    Ok(bytes)
}
pub fn write_tcp(writer: &mut impl Write, bytes: &[u8]) -> io::Result<()> {
    let length =
        u16::try_from(bytes.len()).map_err(|_| invalid("DNS message exceeds 65535 bytes"))?;
    writer.write_all(&length.to_be_bytes())?;
    writer.write_all(bytes)
}

/// Persistent TCP client: idle is unlimited, an incomplete frame is bounded.
pub fn serve<S: Read + Write + std::os::fd::AsRawFd>(
    client: &mut BufReader<Channel<S>>,
    resolver: &Resolver,
    upstream: &Upstream,
    cancel: &Arc<Cancel>,
) -> io::Result<()> {
    loop {
        client.get_mut().idle();
        let mut length = [0; 2];
        match client.read(&mut length[..1])? {
            0 => return Ok(()),
            1 => {}
            _ => unreachable!(),
        }
        client.get_mut().bounded(wire::SETUP_TIMEOUT);
        client.read_exact(&mut length[1..])?;
        let mut query = vec![0; u16::from_be_bytes(length) as usize];
        client.read_exact(&mut query)?;
        let response = resolver.lookup(&query, upstream, Arc::clone(cancel))?;
        write_tcp(client.get_mut(), &response)?;
    }
}
