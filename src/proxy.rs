//! Proxy URL parsing, redaction and reachability helpers.

/// Parsed proxy URL. Credentials are never retained.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProxyParts {
    pub scheme: String,
    pub host: String,
    pub port: Option<u16>,
    pub had_userinfo: bool,
}

const SCHEMES: &[&str] = &["http", "https", "socks4", "socks4a", "socks5", "socks5h"];

/// Parse a proxy URL. A missing scheme is treated as `http`.
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
    // Split userinfo first: it may itself contain '/'. Only then cut the path.
    let (userinfo, hostpath) = match rest.rfind('@') {
        Some(at) => (Some(&rest[..at]), &rest[at + 1..]),
        None => (None, rest),
    };
    let end = hostpath.find(['/', '?', '#']).unwrap_or(hostpath.len());
    let hostport = &hostpath[..end];
    let (host, port) = if let Some(stripped) = hostport.strip_prefix('[') {
        let Some(close) = stripped.find(']') else {
            return Err("missing host".to_string());
        };
        let host = &stripped[..close];
        let after = &stripped[close + 1..];
        let port = if after.is_empty() {
            None
        } else {
            let Some(p) = after.strip_prefix(':') else {
                return Err("invalid port".to_string());
            };
            Some(parse_port(p)?)
        };
        (format!("[{host}]"), port)
    } else {
        match hostport.rfind(':') {
            Some(colon) => {
                let host = &hostport[..colon];
                let port = &hostport[colon + 1..];
                if host.contains(':') {
                    return Err("invalid port".to_string());
                }
                (host.to_string(), Some(parse_port(port)?))
            }
            None => (hostport.to_string(), None),
        }
    };
    if host.is_empty() || host == "[]" {
        return Err("missing host".to_string());
    }
    Ok(ProxyParts {
        scheme,
        host,
        port,
        had_userinfo: userinfo.is_some_and(|u| !u.is_empty()),
    })
}

fn parse_port(s: &str) -> Result<u16, String> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return Err("invalid port".to_string());
    }
    s.parse::<u16>().map_err(|_| "invalid port".to_string())
}

/// Render a proxy URL with credentials, path, query and fragment removed.
pub fn redact_proxy(raw: &str) -> String {
    match parse_proxy_url(raw) {
        Ok(p) => {
            let mut out = format!("{}://", p.scheme);
            if p.had_userinfo {
                out.push_str("<redacted>@");
            }
            out.push_str(&p.host);
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
    fn missing_host() {
        assert_eq!(parse_proxy_url("http://:1").unwrap_err(), "missing host");
    }
}
