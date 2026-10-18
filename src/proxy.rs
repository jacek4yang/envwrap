//! Proxy URL parsing, redaction and reachability helpers.

/// Parsed proxy URL. Credentials are never retained, only the fact that a
/// userinfo section was present.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProxyParts {
    pub scheme: String,
    pub host: String,
    pub port: Option<u16>,
    pub had_userinfo: bool,
}

const SCHEMES: &[&str] = &["http", "https", "socks4", "socks4a", "socks5", "socks5h"];

/// Parse a proxy URL. A missing scheme is treated as `http`.
///
/// The authority is split on the last `@` *before* the path is cut, because a
/// password may contain `/`; the reference implementation's order loses the
/// host in that case.
pub fn parse_proxy_url(raw: &str) -> Result<ProxyParts, String> {
    let raw = raw.trim();
    let (scheme, rest) = match raw.find("://") {
        Some(i) => {
            let scheme = raw[..i].to_ascii_lowercase();
            if !SCHEMES.contains(&scheme.as_str()) {
                return Err(format!("unsupported scheme '{scheme}'"));
            }
            (scheme, &raw[i + 3..])
        }
        None => ("http".to_string(), raw),
    };
    let (userinfo, hostpath) = match rest.rfind('@') {
        Some(at) => (Some(&rest[..at]), &rest[at + 1..]),
        None => (None, rest),
    };
    let end = hostpath.find(['/', '?', '#']).unwrap_or(hostpath.len());
    let hostport = &hostpath[..end];
    let had_userinfo = userinfo.is_some();
    let (host, port) = split_hostport(hostport)?;
    if host.is_empty() {
        return Err("missing host".to_string());
    }
    Ok(ProxyParts {
        scheme,
        host,
        port,
        had_userinfo,
    })
}

/// `[v6][:port]` or `host[:port]`.
fn split_hostport(hostport: &str) -> Result<(String, Option<u16>), String> {
    if let Some(stripped) = hostport.strip_prefix('[') {
        let Some(close) = stripped.find(']') else {
            return Err("missing host".to_string());
        };
        let host = &stripped[..close];
        if host.is_empty() {
            return Err("missing host".to_string());
        }
        let after = &stripped[close + 1..];
        let port = match after.strip_prefix(':') {
            None if after.is_empty() => None,
            None => return Err("invalid port".to_string()),
            Some(p) => Some(parse_port(p)?),
        };
        // Canonical form without brackets; callers re-bracket for display.
        Ok((host.to_string(), port))
    } else {
        match hostport.rsplit_once(':') {
            // More than one colon without brackets is not a valid authority.
            Some((h, _)) if h.contains(':') => Err("invalid port".to_string()),
            Some(("", _)) => Err("missing host".to_string()),
            Some((h, p)) => Ok((h.to_string(), Some(parse_port(p)?))),
            None => Ok((hostport.to_string(), None)),
        }
    }
}

fn parse_port(s: &str) -> Result<u16, String> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return Err("invalid port".to_string());
    }
    s.parse::<u16>().map_err(|_| "invalid port".to_string())
}

/// Host as it must appear in a URL (`[v6]` when it contains a colon).
pub fn display_host(host: &str) -> String {
    if host.contains(':') {
        format!("[{host}]")
    } else {
        host.to_string()
    }
}

/// Render a proxy URL with credentials, path, query and fragment removed.
pub fn redact_proxy(raw: &str) -> String {
    match parse_proxy_url(raw) {
        Ok(p) => {
            let mut out = format!("{}://", p.scheme);
            if p.had_userinfo {
                out.push_str("<redacted>@");
            }
            out.push_str(&display_host(&p.host));
            if let Some(port) = p.port {
                out.push_str(&format!(":{port}"));
            }
            out
        }
        Err(_) => "<redacted>".to_string(),
    }
}

/// Default port for a scheme.
pub fn default_port(scheme: &str) -> u16 {
    match scheme {
        "http" => 80,
        "https" => 443,
        _ => 1080,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_port_defaults_to_http() {
        let p = parse_proxy_url("host:8080").unwrap();
        assert_eq!(p.scheme, "http");
        assert_eq!(p.host, "host");
        assert_eq!(p.port, Some(8080));
        assert!(!p.had_userinfo);
    }

    #[test]
    fn rejects_bad_scheme() {
        assert_eq!(
            parse_proxy_url("httpsx://h:1").unwrap_err(),
            "unsupported scheme 'httpsx'"
        );
    }

    #[test]
    fn rejects_bad_port() {
        assert_eq!(
            parse_proxy_url("http://h:notaport").unwrap_err(),
            "invalid port"
        );
    }

    #[test]
    fn redact_keeps_port() {
        assert_eq!(redact_proxy("socks5://u:p@h:1"), "socks5://<redacted>@h:1");
        assert_eq!(
            redact_proxy("http://u:p@[::1]:3128/x?a=1"),
            "http://<redacted>@[::1]:3128"
        );
        assert_eq!(redact_proxy("http://h:3128"), "http://h:3128");
        assert_eq!(redact_proxy(""), "<redacted>");
    }

    #[test]
    fn userinfo_with_slash() {
        let p = parse_proxy_url("http://u/p@h:1").unwrap();
        assert_eq!(p.host, "h");
        assert_eq!(p.port, Some(1));
        assert!(p.had_userinfo);
    }

    #[test]
    fn default_ports() {
        assert_eq!(default_port("http"), 80);
        assert_eq!(default_port("https"), 443);
        assert_eq!(default_port("socks5"), 1080);
    }

    #[test]
    fn empty_port_is_invalid() {
        assert_eq!(parse_proxy_url("http://h:").unwrap_err(), "invalid port");
    }

    #[test]
    fn out_of_range_port_is_invalid() {
        assert_eq!(
            parse_proxy_url("http://h:99999").unwrap_err(),
            "invalid port"
        );
    }

    #[test]
    fn empty_userinfo_marked() {
        assert_eq!(redact_proxy("https://@h:1"), "https://<redacted>@h:1");
    }

    #[test]
    fn ipv6_host_is_canonical() {
        let p = parse_proxy_url("socks5://[::1]:1080").unwrap();
        assert_eq!(p.host, "::1");
        assert_eq!(display_host(&p.host), "[::1]");
        assert_eq!(
            redact_proxy("http://u:p@[::1]:3128/x?a=1"),
            "http://<redacted>@[::1]:3128"
        );
    }

    #[test]
    fn missing_host() {
        assert_eq!(parse_proxy_url("http://:1").unwrap_err(), "missing host");
    }
}

/// A single `NO_PROXY` entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NoProxyRule {
    /// `*` — bypass every host.
    All,
    /// CIDR block, matched only by the caller that understands networks.
    Cidr(String),
    /// `example.com` / `.example.com` / `*.example.com`.
    Suffix(String),
    /// A literal host, optionally port-qualified.
    Exact { domain: String, port: Option<u16> },
}

/// Parse a `NO_PROXY` value into rules. Unparseable CIDRs are dropped, matching
/// the reference checker.
pub fn parse_no_proxy(raw: &str) -> Vec<NoProxyRule> {
    let mut out = Vec::new();
    for tok in raw
        .split([',', ' ', '\t', '\n'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        if tok == "*" {
            out.push(NoProxyRule::All);
            continue;
        }
        if tok.contains('/') {
            // A well-formed IPv4/IPv6 prefix keeps its digits, dots and colons.
            if is_cidr(tok) {
                out.push(NoProxyRule::Cidr(tok.to_string()));
            }
            continue;
        }
        let (host, port) = match tok.rsplit_once(':') {
            Some((h, p))
                if !h.contains(':') && !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()) =>
            {
                (h.to_string(), p.parse::<u16>().ok())
            }
            _ => (tok.to_string(), None),
        };
        let host = host.trim_end_matches('.').to_ascii_lowercase();
        if let Some(d) = host.strip_prefix("*.").or_else(|| host.strip_prefix('.')) {
            out.push(NoProxyRule::Suffix(d.to_string()));
        } else if host.is_empty() {
            continue;
        } else {
            out.push(NoProxyRule::Exact { domain: host, port });
        }
    }
    out
}

fn is_cidr(tok: &str) -> bool {
    let Some((addr, len)) = tok.rsplit_once('/') else {
        return false;
    };
    if len.is_empty() || !len.bytes().all(|b| b.is_ascii_digit()) {
        return false;
    }
    addr.parse::<std::net::IpAddr>().is_ok()
}

/// Does `host` fall inside a CIDR rule? Only literal IPs can match.
fn cidr_matches(cidr: &str, ip: std::net::IpAddr) -> bool {
    let Some((addr, len)) = cidr.rsplit_once('/') else {
        return false;
    };
    let Ok(len) = len.parse::<u8>() else {
        return false;
    };
    match (addr.parse::<std::net::IpAddr>(), ip) {
        (Ok(std::net::IpAddr::V4(net)), std::net::IpAddr::V4(ip)) if len <= 32 => {
            let mask = if len == 0 { 0 } else { u32::MAX << (32 - len) };
            (u32::from(net) & mask) == (u32::from(ip) & mask)
        }
        (Ok(std::net::IpAddr::V6(net)), std::net::IpAddr::V6(ip)) if len <= 128 => {
            let mask = if len == 0 {
                0u128
            } else {
                u128::MAX << (128 - len)
            };
            (u128::from(net) & mask) == (u128::from(ip) & mask)
        }
        _ => false,
    }
}

/// Would `host:port` bypass the proxy under these rules?
pub fn no_proxy_bypasses(rules: &[NoProxyRule], host: &str, port: u16) -> bool {
    let host = host.trim_start_matches('[').trim_end_matches(']');
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    let ip = host.parse::<std::net::IpAddr>().ok();
    rules.iter().any(|r| match r {
        NoProxyRule::All => true,
        NoProxyRule::Cidr(c) => ip.is_some_and(|ip| cidr_matches(c, ip)),
        NoProxyRule::Suffix(d) => host == *d || host.ends_with(&format!(".{d}")),
        NoProxyRule::Exact { domain, port: p } => {
            let host_ok = if domain.contains(':') || ip.is_some() {
                // IPv6 rules are written without brackets.
                host == *domain
            } else {
                host == *domain || host.ends_with(&format!(".{domain}"))
            };
            host_ok && p.is_none_or(|p| p == port)
        }
    })
}

#[cfg(test)]
mod noproxy_tests {
    use super::*;

    #[test]
    fn parsing_rules() {
        let r = parse_no_proxy("localhost,.example.com,10.0.0.0/8,*.corp,host:8080,*,bad/cidr");
        assert_eq!(
            r,
            vec![
                NoProxyRule::Exact {
                    domain: "localhost".into(),
                    port: None
                },
                NoProxyRule::Suffix("example.com".into()),
                NoProxyRule::Cidr("10.0.0.0/8".into()),
                NoProxyRule::Suffix("corp".into()),
                NoProxyRule::Exact {
                    domain: "host".into(),
                    port: Some(8080)
                },
                NoProxyRule::All,
            ]
        );
    }

    #[test]
    fn bypass_cases() {
        let r = parse_no_proxy("localhost,.example.com,10.0.0.0/8,host:8080");
        assert!(no_proxy_bypasses(&r, "localhost", 443));
        assert!(no_proxy_bypasses(&r, "api.example.com", 443));
        assert!(no_proxy_bypasses(&r, "example.com", 443));
        assert!(!no_proxy_bypasses(&r, "notexample.com", 443));
        assert!(no_proxy_bypasses(&r, "10.1.2.3", 80));
        assert!(!no_proxy_bypasses(&r, "11.1.2.3", 80));
        assert!(no_proxy_bypasses(&r, "host", 8080));
        assert!(!no_proxy_bypasses(&r, "host", 8081));
    }

    #[test]
    fn star_bypasses_all() {
        assert!(no_proxy_bypasses(
            &parse_no_proxy("*"),
            "anything.example",
            1
        ));
    }

    #[test]
    fn ipv6_suffix_and_literal() {
        let r = parse_no_proxy("::1,[fe80::1]");
        assert!(no_proxy_bypasses(&r, "::1", 80));
    }
}
