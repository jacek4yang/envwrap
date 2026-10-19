//! One bounded, certificate-verified exit-family observation through SOCKS5.
//! The observed address is neither logged nor persisted; it is only a DNS hint.

use std::io::{self, BufRead, BufReader, Write};
use std::net::IpAddr;
use std::sync::Arc;

use crate::{transport::Cancel, upstream::Upstream, wire};

const HOST: &str = "api64.ipify.org";
const MAX_BODY: usize = 64;

pub fn observe(upstream: &Upstream, cancel: Arc<Cancel>) -> io::Result<IpAddr> {
    let tunnel = upstream.connect_name(HOST, 443, cancel)?;
    if !tunnel.buffer().is_empty() {
        return Err(io::Error::other("unsolicited bytes before TLS handshake"));
    }
    let mut socket = tunnel.into_inner();
    socket.bounded(wire::SETUP_TIMEOUT);
    let roots = rustls::RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let config = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let connection = rustls::ClientConnection::new(
        Arc::new(config),
        HOST.try_into().expect("constant DNS name"),
    )
    .map_err(io::Error::other)?;
    let mut tls = rustls::StreamOwned::new(connection, socket);
    tls.write_all(b"GET / HTTP/1.1\r\nHost: api64.ipify.org\r\nAccept: text/plain\r\nConnection: close\r\n\r\n")?;
    response(&mut BufReader::new(tls))
}

fn response(reader: &mut impl BufRead) -> io::Result<IpAddr> {
    let mut budget = 8192;
    let status = line(reader, &mut budget)?;
    let mut parts = status.split_whitespace();
    if !matches!(parts.next(), Some("HTTP/1.0" | "HTTP/1.1")) || parts.next() != Some("200") {
        return Err(io::Error::other("exit observer did not return HTTP 200"));
    }
    let mut length = None;
    let mut chunked = false;
    loop {
        let header = line(reader, &mut budget)?;
        if header.is_empty() {
            break;
        }
        let (key, value) = header
            .split_once(':')
            .ok_or_else(|| io::Error::other("invalid HTTP header"))?;
        if key.eq_ignore_ascii_case("content-length") {
            if length.is_some() {
                return Err(io::Error::other("duplicate content length"));
            }
            length = Some(
                value
                    .trim()
                    .parse::<usize>()
                    .map_err(|_| io::Error::other("invalid content length"))?,
            );
        } else if key.eq_ignore_ascii_case("transfer-encoding") {
            if chunked || !value.trim().eq_ignore_ascii_case("chunked") {
                return Err(io::Error::other("unsupported transfer encoding"));
            }
            chunked = true;
        }
    }
    if chunked && length.is_some() {
        return Err(io::Error::other("ambiguous HTTP framing"));
    }
    let mut body = [0; MAX_BODY];
    let count = if chunked {
        let mut used = 0;
        loop {
            let size = line(reader, &mut budget)?;
            let size = usize::from_str_radix(size.split(';').next().unwrap_or("").trim(), 16)
                .map_err(|_| io::Error::other("invalid HTTP chunk"))?;
            if size == 0 {
                while !line(reader, &mut budget)?.is_empty() {}
                break;
            }
            if size > MAX_BODY - used {
                return Err(io::Error::other("exit response too large"));
            }
            reader.read_exact(&mut body[used..used + size])?;
            used += size;
            let mut ending = [0; 2];
            reader.read_exact(&mut ending)?;
            if ending != *b"\r\n" {
                return Err(io::Error::other("invalid chunk ending"));
            }
        }
        used
    } else if let Some(length) = length {
        if length > MAX_BODY {
            return Err(io::Error::other("exit response too large"));
        }
        reader.read_exact(&mut body[..length])?;
        length
    } else {
        let mut used = 0;
        loop {
            if used == MAX_BODY {
                return Err(io::Error::other("exit response too large"));
            }
            let count = reader.read(&mut body[used..])?;
            if count == 0 {
                break;
            }
            used += count;
        }
        used
    };
    std::str::from_utf8(&body[..count])
        .ok()
        .and_then(|s| s.trim().parse::<IpAddr>().ok())
        .map(|ip| ip.to_canonical())
        .ok_or_else(|| io::Error::other("exit response is not an IP address"))
}
fn line(reader: &mut impl BufRead, budget: &mut usize) -> io::Result<String> {
    let line = wire::line(reader, *budget)?;
    *budget = budget.saturating_sub(line.len() + 2);
    Ok(line)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exit_observation_handles_both_families_and_chunked_responses() {
        assert_eq!(
            response(&mut &b"HTTP/1.1 200 OK\r\nContent-Length: 11\r\n\r\n203.0.113.1"[..])
                .unwrap(),
            "203.0.113.1".parse::<IpAddr>().unwrap()
        );
        assert_eq!(response(&mut &b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nb\r\n2001:db8::1\r\n0\r\n\r\n"[..]).unwrap(), "2001:db8::1".parse::<IpAddr>().unwrap());
    }
    #[test]
    fn errors_and_ambiguous_or_truncated_bodies_never_choose_a_family() {
        for bytes in [
            "HTTP/1.1 403 Forbidden\r\n\r\n203.0.113.1",
            "HTTP/1.1 200 OK\r\nContent-Length: 100000\r\n\r\n",
            "HTTP/1.1 200 OK\r\nContent-Length: 11\r\n\r\n203.0.113",
            "HTTP/1.1 200 OK\r\nContent-Length: 11\r\nTransfer-Encoding: chunked\r\n\r\n",
            "HTTP/1.1 200 OK\r\n\r\nnot an IP",
        ] {
            assert!(response(&mut bytes.as_bytes()).is_err());
        }
    }
}
